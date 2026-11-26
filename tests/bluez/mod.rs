//! A stand-in for `BlueZ` with a MeshCore companion in range, paired and not
//! connected. It does what the BLE transport relies on as `BlueZ` 5.87 does it
//! (`src/device.c`, `src/gatt-client.c`); the test sets what else happens.

mod bus;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bus::{Body, Bus, Call, Interfaces, Program, Properties, Service, Value};

use crate::common::{err, self_info_frame};

/// The companion's address and name.
pub const ADDRESS: &str = "AA:BB:CC:DD:EE:FF";
pub const NAME: &str = "MeshCore-Node";

/// The adapter has the name bluer takes as the default one.
const ADAPTER: &str = "/org/bluez/hci0";
const DEVICE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
const UART: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service000a";
const RX: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service000a/char000b";
const TX: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF/service000a/char000d";

const ADAPTER_1: &str = "org.bluez.Adapter1";
const AGENTS_1: &str = "org.bluez.AgentManager1";
const DEVICE_1: &str = "org.bluez.Device1";
const SERVICE_1: &str = "org.bluez.GattService1";
const CHARACTERISTIC_1: &str = "org.bluez.GattCharacteristic1";
const OBJECTS: &str = "org.freedesktop.DBus.ObjectManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const FAILED: &str = "org.bluez.Error.Failed";
const IN_PROGRESS: &str = "org.bluez.Error.InProgress";

/// The error of a write that the link dropped with.
const UNLIKELY: &str = "Operation failed with ATT error: 0x0e";

/// What the companion does with a frame that is written to it.
#[derive(Debug, Clone)]
pub struct Reaction {
    /// The frames it sends.
    pub frames: Vec<Vec<u8>>,
    pub then: Then,
}

impl Reaction {
    pub const fn new(frames: Vec<Vec<u8>>, then: Then) -> Self {
        Self { frames, then }
    }
}

/// How a write goes on.
#[derive(Debug, Clone, Copy)]
pub enum Then {
    /// The companion confirms it.
    Confirm,
    /// The companion confirms it, and the link drops.
    ConfirmAndDrop,
    /// The link drops first: `BlueZ` ends the write, and after this time tells
    /// that the link is gone.
    ErrorAndDrop(Duration),
    /// The same the other way round.
    DropAndError(Duration),
    /// The link drops, and `BlueZ` does not end the write.
    Drop,
    /// The companion does not confirm it.
    Nothing,
}

/// What the companion does with the frames that are written to it.
pub type OnWrite = Arc<dyn Fn(&[u8]) -> Reaction + Send + Sync>;

/// What happens from now on.
pub struct Script {
    pub on_write: OnWrite,
    /// How long connecting takes; `None`: the companion is out of reach.
    pub connect_takes: Option<Duration>,
    /// What `BlueZ` reports for the next connects, whose link drops.
    pub drops: VecDeque<&'static str>,
    /// For how long it shows such a link.
    pub drop_shows: Duration,
    /// What it reports while the companion does not know the pairing.
    pub forgot_pairing: Option<&'static str>,
    pub disconnect_takes: Duration,
    pub pin: u32,
    /// Whether a scan hears the companion once it was removed.
    pub heard: bool,
    /// Another program has the notifications for itself.
    pub notify_acquired: bool,
    /// Whether the companion has the Nordic UART service.
    pub has_uart: bool,
}

/// The answers of a companion: session start and battery (3540 mV).
pub fn answer(frame: &[u8]) -> Reaction {
    let reply = match frame[0] {
        1 => self_info_frame("Node"),
        20 => vec![12, 0xd4, 0x0d],
        _ => err(1),
    };
    Reaction::new(vec![reply], Then::Confirm)
}

struct State {
    objects: BTreeMap<String, Interfaces>,
    script: Script,
    /// A connect under way, and how many there have been.
    connecting: Option<Call>,
    connects: u64,
    /// The link is shown as connected, but not served any more.
    leaving: bool,
    disconnects: Vec<Call>,
    /// The programs that get the notifications.
    notified: Vec<String>,
    /// The write under way, and all frames so far.
    writing: Option<Call>,
    written: Vec<Vec<u8>>,
    /// The pairing agents of the programs.
    agents: HashMap<String, (Program, String)>,
    removals: usize,
    /// How often a program asked to disconnect the companion.
    disconnect_calls: usize,
}

struct Inner {
    bus: Bus,
    runtime: tokio::runtime::Handle,
    state: Mutex<State>,
}

/// The test's handle on the stand-in.
#[derive(Clone)]
pub struct Bluez(Arc<Inner>);

/// The lines the library logged at info level and above, as `LEVEL:message`.
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "meshcore" && metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let line = format!("{}:{}", record.level(), record.args());
            LINES.lock().unwrap().push(line);
        }
    }

    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

/// The lines the library logged at info level and above since the last call.
pub fn logged() -> Vec<String> {
    std::mem::take(&mut *LINES.lock().unwrap())
}

/// Runs `test` in a process of its own: D-Bus takes the address of the system
/// bus from the environment a process starts with. `_test` is the test function
/// this is called in. The process captures the library's log for [`logged`].
pub fn run<F: Fn()>(_test: F, test: impl AsyncFnOnce(Bluez)) {
    const TEST: &str = "MESHCORE_BLUEZ_TEST";
    const ADDRESS: &str = "DBUS_SYSTEM_BUS_ADDRESS";
    const ABSTRACT: &str = "unix:abstract=";
    let name = std::any::type_name::<F>().rsplit("::").next().unwrap();
    if std::env::var(TEST).is_ok_and(|test| test == name) {
        // also one of the stand-in, which would leave the test waiting
        let report = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            report(info);
            std::process::exit(101);
        }));
        log::set_logger(&CAPTURE).unwrap();
        log::set_max_level(log::LevelFilter::Info);
        let address = std::env::var(ADDRESS).unwrap();
        let bluez = Bluez::start(address.strip_prefix(ABSTRACT).unwrap());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build();
        return runtime.unwrap().block_on(test(bluez));
    }
    let socket = format!("meshcore-{}-{name}", std::process::id());
    let output = Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--nocapture"])
        .env(TEST, name)
        .env(ADDRESS, format!("{ABSTRACT}{socket}"))
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && printed.contains("1 passed"),
        "{printed}{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn text(text: &str) -> Value {
    Value::Text(text.into())
}

fn properties<const N: usize>(properties: [(&str, Value); N]) -> Properties {
    properties
        .map(|(name, value)| (name.to_string(), value))
        .into()
}

fn object(interface: &str, properties: Properties) -> Interfaces {
    BTreeMap::from([(interface.to_string(), properties)])
}

fn device(address: &str, name: &str) -> Properties {
    properties([
        ("Address", text(address)),
        ("AddressType", text("random")),
        ("Name", text(name)),
        ("Alias", text(name)),
        ("Adapter", Value::Path(ADAPTER.into())),
        ("Paired", Value::Bool(false)),
        ("Connected", Value::Bool(false)),
        ("ServicesResolved", Value::Bool(false)),
    ])
}

/// The objects of the Nordic UART service.
fn uart() -> [(&'static str, Interfaces); 3] {
    let uuid = |number| text(&format!("6e40000{number}-b5a3-f393-e0a9-e50e24dcca9e"));
    let characteristic = |number, flag: &str| {
        let mut properties = properties([
            ("UUID", uuid(number)),
            ("Service", Value::Path(UART.into())),
            ("Flags", Value::Texts(vec![flag.into()])),
        ]);
        if flag == "notify" {
            properties.insert("Notifying".into(), Value::Bool(false));
        }
        object(CHARACTERISTIC_1, properties)
    };
    let service = properties([
        ("UUID", uuid(1)),
        ("Device", Value::Path(DEVICE.into())),
        ("Primary", Value::Bool(true)),
    ]);
    [
        (UART, object(SERVICE_1, service)),
        (RX, characteristic(2, "write")),
        (TX, characteristic(3, "notify")),
    ]
}

impl Bluez {
    fn start(socket: &str) -> Self {
        let adapter = properties([
            ("Address", text("11:22:33:44:55:66")),
            ("Powered", Value::Bool(true)),
        ]);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut state = State {
            objects: BTreeMap::from([
                ("/org/bluez".into(), object(AGENTS_1, Properties::new())),
                (ADAPTER.into(), object(ADAPTER_1, adapter)),
            ]),
            script: Script {
                on_write: Arc::new(answer),
                connect_takes: Some(Duration::from_millis(5)),
                drops: VecDeque::new(),
                drop_shows: Duration::ZERO,
                forgot_pairing: None,
                disconnect_takes: Duration::from_millis(10),
                pin: 123_456,
                heard: true,
                notify_acquired: false,
                has_uart: true,
            },
            connecting: None,
            connects: 0,
            leaving: false,
            disconnects: Vec::new(),
            notified: Vec::new(),
            writing: None,
            written: Vec::new(),
            agents: HashMap::new(),
            removals: 0,
            disconnect_calls: 0,
        };
        let mut companion = device(ADDRESS, NAME);
        companion.insert("Paired".into(), Value::Bool(true));
        companion.insert("RSSI".into(), Value::I16(-50));
        state
            .objects
            .insert(DEVICE.into(), object(DEVICE_1, companion));
        for (path, interfaces) in uart() {
            state.objects.insert(path.into(), interfaces);
        }
        let bluez = Self(Arc::new(Inner {
            bus: Bus::default(),
            runtime: runtime.handle().clone(),
            state: Mutex::new(state),
        }));
        let serve = bluez
            .0
            .bus
            .clone()
            .serve(Bus::listen(socket), bluez.clone());
        std::thread::spawn(move || runtime.block_on(serve));
        bluez
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().unwrap()
    }

    fn after(&self, time: Duration, then: impl FnOnce(&Self) + Send + 'static) {
        let bluez = self.clone();
        self.0.runtime.spawn(async move {
            tokio::time::sleep(time).await;
            then(&bluez);
        });
    }

    // ------------------------------------------------------- for the test

    /// Changes what happens from now on.
    pub fn script(&self, change: impl FnOnce(&mut Script)) {
        change(&mut self.state().script);
    }

    /// Sets what the companion does with the frames written to it.
    pub fn on_write(&self, on_write: impl Fn(&[u8]) -> Reaction + Send + Sync + 'static) {
        self.script(|script| script.on_write = Arc::new(on_write));
    }

    /// Whether the companion's link is shown as connected.
    pub fn connected(&self) -> bool {
        self.state().is(DEVICE, "Connected")
    }

    pub fn paired(&self) -> bool {
        self.state().is(DEVICE, "Paired")
    }

    pub fn set_paired(&self, paired: bool) {
        let value = Value::Bool(paired);
        self.state()
            .set(&self.0.bus, DEVICE, DEVICE_1, "Paired", value);
    }

    pub fn set_powered(&self, powered: bool) {
        let value = Value::Bool(powered);
        self.state()
            .set(&self.0.bus, ADAPTER, ADAPTER_1, "Powered", value);
    }

    /// Whether a program is connecting to it.
    pub fn connecting(&self) -> bool {
        self.state().connecting.is_some()
    }

    /// Whether the link is being taken down.
    pub fn leaving(&self) -> bool {
        self.state().leaving
    }

    /// Connects the companion as a program would that is gone by now.
    pub fn connect(&self) {
        let mut state = self.state();
        self.link_up(&mut state);
    }

    /// Ends the connect under way, which may have had no end.
    pub fn finish_connect(&self) {
        let connect = self.state().connects;
        self.end_connect(connect);
    }

    /// The link drops.
    pub fn drop_link(&self) {
        let mut state = self.state();
        state.fail_write();
        self.link_gone(&mut state);
    }

    /// The companion sends a frame.
    pub fn send(&self, frame: &[u8]) {
        let mut state = self.state();
        self.notify(&mut state, frame);
    }

    /// The frames written to the companion so far.
    pub fn written(&self) -> Vec<Vec<u8>> {
        self.state().written.clone()
    }

    /// How often the companion was removed, with its pairing.
    pub fn removals(&self) -> usize {
        self.state().removals
    }

    /// How often a program asked to disconnect the companion.
    pub fn disconnect_calls(&self) -> usize {
        self.state().disconnect_calls
    }

    /// Adds another device, which is heard with `rssi`.
    pub fn add_device(&self, address: &str, name: &str, rssi: Option<i16>, connected: bool) {
        let mut properties = device(address, name);
        properties.insert("Connected".into(), Value::Bool(connected));
        if let Some(rssi) = rssi {
            properties.insert("RSSI".into(), Value::I16(rssi));
        }
        let path = format!("{ADAPTER}/dev_{}", address.replace(':', "_"));
        self.add_object(&mut self.state(), &path, object(DEVICE_1, properties));
    }

    /// Removes the companion, as when someone removes it by hand.
    pub fn remove(&self) {
        let mut state = self.state();
        state.notified.clear();
        state.leaving = false;
        for path in [TX, RX, UART, DEVICE] {
            if let Some(interfaces) = state.objects.remove(path) {
                let names: Vec<String> = interfaces.into_keys().collect();
                let body = Body::default().path(path).texts(&names);
                self.0.bus.signal("/", OBJECTS, "InterfacesRemoved", &body);
            }
        }
    }

    // ------------------------------------------------------- the companion

    fn add_object(&self, state: &mut State, path: &str, interfaces: Interfaces) {
        let body = Body::default().path(path).interfaces(&interfaces);
        state.objects.insert(path.into(), interfaces);
        self.0.bus.signal("/", OBJECTS, "InterfacesAdded", &body);
    }

    fn notify(&self, state: &mut State, frame: &[u8]) {
        if !state.notified.is_empty() {
            let value = Value::Bytes(frame.into());
            state.set(&self.0.bus, TX, CHARACTERISTIC_1, "Value", value);
        }
    }

    fn link_up(&self, state: &mut State) {
        let bus = &self.0.bus;
        state.leaving = false;
        state.set(bus, DEVICE, DEVICE_1, "Connected", Value::Bool(true));
        if state.script.has_uart {
            for (path, interfaces) in uart() {
                if !state.objects.contains_key(path) {
                    self.add_object(state, path, interfaces);
                }
            }
        } else {
            state.objects.retain(|path, _| !path.starts_with(UART));
        }
        state.set(bus, DEVICE, DEVICE_1, "ServicesResolved", Value::Bool(true));
    }

    /// As `device_remove_connection`.
    fn link_gone(&self, state: &mut State) {
        let bus = &self.0.bus;
        state.leaving = false;
        state.notified.clear();
        if state.is(DEVICE, "Connected") {
            state.set(bus, DEVICE, DEVICE_1, "Connected", Value::Bool(false));
            state.set(
                bus,
                DEVICE,
                DEVICE_1,
                "ServicesResolved",
                Value::Bool(false),
            );
        }
        if state.is(TX, "Notifying") {
            state.set(bus, TX, CHARACTERISTIC_1, "Notifying", Value::Bool(false));
        }
        for call in state.disconnects.drain(..) {
            call.reply(Body::default());
        }
    }

    /// As `dev_connect`.
    fn begin_connect(&self, call: Call) {
        let mut state = self.state();
        if state.connecting.is_some() {
            return call.fail(IN_PROGRESS, "Operation already in progress");
        }
        if state.is(DEVICE, "Connected") {
            return call.reply(Body::default());
        }
        state.connecting = Some(call);
        state.connects += 1;
        let connect = state.connects;
        if let Some(time) = state.script.connect_takes {
            self.after(time, move |bluez| bluez.end_connect(connect));
        }
    }

    fn end_connect(&self, connect: u64) {
        let mut state = self.state();
        // It may have been broken off.
        if state.connects != connect {
            return;
        }
        let Some(call) = state.connecting.take() else {
            return;
        };
        let refused = state.script.forgot_pairing;
        let dropped = match refused.filter(|_| state.is(DEVICE, "Paired")) {
            None => state.script.drops.pop_front(),
            refused => refused,
        };
        let Some(error) = dropped else {
            self.link_up(&mut state);
            return call.reply(Body::default());
        };
        let shows = state.script.drop_shows;
        if !shows.is_zero() {
            let up = Value::Bool(true);
            state.leaving = true;
            state.set(&self.0.bus, DEVICE, DEVICE_1, "Connected", up);
            self.after(shows, Self::take_down);
        }
        call.fail(FAILED, error);
    }

    /// As `device_request_disconnect`.
    fn disconnect(&self, call: Call) {
        let mut state = self.state();
        state.disconnect_calls += 1;
        if let Some(connecting) = state.connecting.take() {
            connecting.fail(FAILED, "br-connection-canceled");
        }
        if !state.is(DEVICE, "Connected") {
            return call.reply(Body::default());
        }
        state.disconnects.push(call);
        if !state.leaving {
            state.leaving = true;
            let resolved = Value::Bool(false);
            state.set(&self.0.bus, DEVICE, DEVICE_1, "ServicesResolved", resolved);
            let time = state.script.disconnect_takes;
            self.after(time, Self::take_down);
        }
    }

    /// Ends a link that is shown but not served any more.
    fn take_down(&self) {
        if self.state().leaving {
            self.drop_link();
        }
    }

    /// Asks the program's agent for the PIN.
    fn pair(&self, call: Call) {
        let bluez = self.clone();
        tokio::spawn(async move {
            let (agent, pin) = {
                let state = bluez.state();
                let agent = state.agents.get(&call.program.name).cloned();
                (agent, state.script.pin)
            };
            let mut given = None;
            if let Some((program, path)) = agent {
                let body = Body::default().path(DEVICE);
                let answer = program.call(&path, "org.bluez.Agent1", "RequestPasskey", body);
                given = answer.await.map(|body| body.reader().u32());
            }
            if given == Some(pin) {
                bluez.set_paired(true);
                bluez.script(|script| script.forgot_pairing = None);
                call.reply(Body::default());
            } else {
                let error = "org.bluez.Error.AuthenticationFailed";
                call.fail(error, "Authentication Failed");
            }
        });
    }

    fn remove_device(&self, call: &Call) {
        self.state().removals += 1;
        self.remove();
        call.reply(Body::default());
    }

    fn start_discovery(&self, call: &Call) {
        call.reply(Body::default());
        self.after(Duration::from_millis(100), |bluez| {
            let mut state = bluez.state();
            if state.script.heard && !state.objects.contains_key(DEVICE) {
                let mut companion = device(ADDRESS, NAME);
                companion.insert("RSSI".into(), Value::I16(-50));
                bluez.add_object(&mut state, DEVICE, object(DEVICE_1, companion));
            }
        });
    }

    /// As `characteristic_start_notify`.
    fn start_notify(&self, call: &Call) {
        let mut state = self.state();
        if state.leaving {
            return call.fail("org.bluez.Error.NotConnected", "Not Connected");
        }
        if state.script.notify_acquired {
            return call.fail("org.bluez.Error.NotPermitted", "Notify acquired");
        }
        if state.is(DEVICE, "Connected") && !state.is(TX, "Notifying") {
            let notifying = Value::Bool(true);
            state.set(&self.0.bus, TX, CHARACTERISTIC_1, "Notifying", notifying);
        }
        let program = call.program.name.clone();
        if !state.notified.contains(&program) {
            state.notified.push(program);
        }
        call.reply(Body::default());
    }

    fn stop_notify(&self, program: &str) -> bool {
        let mut state = self.state();
        let before = state.notified.len();
        state.notified.retain(|notified| notified != program);
        if state.notified.is_empty() && state.is(TX, "Notifying") {
            let notifying = Value::Bool(false);
            state.set(&self.0.bus, TX, CHARACTERISTIC_1, "Notifying", notifying);
        }
        state.notified.len() < before
    }

    /// As `characteristic_write_value`.
    fn write(&self, call: Call) {
        let frame = call.message.body.reader().bytes();
        let on_write = {
            let mut state = self.state();
            if !state.is(DEVICE, "Connected") || state.leaving {
                return call.fail(FAILED, "Not connected");
            }
            if state.writing.is_some() {
                return call.fail(IN_PROGRESS, "Operation already in progress");
            }
            state.written.push(frame.clone());
            state.writing = Some(call);
            Arc::clone(&state.script.on_write)
        };
        let reaction = on_write(&frame);
        let mut state = self.state();
        for frame in &reaction.frames {
            self.notify(&mut state, frame);
        }
        match reaction.then {
            Then::Confirm => state.confirm_write(),
            Then::ConfirmAndDrop => {
                state.confirm_write();
                self.link_gone(&mut state);
            }
            Then::ErrorAndDrop(time) => {
                state.fail_write();
                self.after(time, |bluez| bluez.link_gone(&mut bluez.state()));
            }
            Then::DropAndError(time) => {
                self.link_gone(&mut state);
                self.after(time, |bluez| bluez.state().fail_write());
            }
            Then::Drop => {
                self.link_gone(&mut state);
                state.writing = None;
            }
            Then::Nothing => {}
        }
    }
}

impl State {
    fn property(&self, path: &str, interface: &str, name: &str) -> Option<&Value> {
        self.objects.get(path)?.get(interface)?.get(name)
    }

    /// Whether a property of a device or characteristic is true.
    fn is(&self, path: &str, name: &str) -> bool {
        [DEVICE_1, CHARACTERISTIC_1]
            .into_iter()
            .any(|interface| self.property(path, interface, name) == Some(&Value::Bool(true)))
    }

    /// Sets a property, if there is the object, and tells the programs.
    fn set(&mut self, bus: &Bus, path: &str, interface: &str, name: &str, value: Value) {
        let object = self.objects.get_mut(path);
        let Some(properties) = object.and_then(|object| object.get_mut(interface)) else {
            return;
        };
        properties.insert(name.into(), value.clone());
        let body = Body::default()
            .text(interface)
            .properties(&Properties::from([(name.to_string(), value)]))
            .texts(&[]);
        bus.signal(path, PROPERTIES, "PropertiesChanged", &body);
    }

    fn confirm_write(&mut self) {
        if let Some(call) = self.writing.take() {
            call.reply(Body::default());
        }
    }

    fn fail_write(&mut self) {
        if let Some(call) = self.writing.take() {
            call.fail(FAILED, UNLIKELY);
        }
    }
}

impl Service for Bluez {
    fn call(&self, call: Call) {
        let message = &call.message;
        let done = || call.reply(Body::default());
        if message.path != "/" && !self.state().objects.contains_key(&message.path) {
            let error = "org.freedesktop.DBus.Error.UnknownObject";
            return call.fail(error, &format!("there is no {}", message.path));
        }
        match (message.interface.as_str(), message.member.as_str()) {
            (OBJECTS, "GetManagedObjects") => {
                call.reply(Body::default().objects(&self.state().objects));
            }
            (PROPERTIES, "Get") => {
                let mut arguments = message.body.reader();
                let (interface, name) = (arguments.text(), arguments.text());
                match self.state().property(&message.path, &interface, &name) {
                    Some(value) => call.reply(Body::default().variant(value)),
                    None => call.fail("org.freedesktop.DBus.Error.InvalidArgs", "No such property"),
                }
            }
            (ADAPTER_1, "SetDiscoveryFilter" | "StopDiscovery")
            | (AGENTS_1, "UnregisterAgent" | "RequestDefaultAgent") => done(),
            (ADAPTER_1, "StartDiscovery") => self.start_discovery(&call),
            (ADAPTER_1, "RemoveDevice") => self.remove_device(&call),
            (AGENTS_1, "RegisterAgent") => {
                let agent = (call.program.clone(), message.body.reader().text());
                self.state().agents.insert(call.program.name.clone(), agent);
                done();
            }
            (DEVICE_1, "Connect") => self.begin_connect(call),
            (DEVICE_1, "Disconnect") => self.disconnect(call),
            (DEVICE_1, "Pair") => self.pair(call),
            (CHARACTERISTIC_1, "StartNotify") => self.start_notify(&call),
            (CHARACTERISTIC_1, "StopNotify") => {
                if self.stop_notify(&call.program.name) {
                    done();
                } else {
                    call.fail(FAILED, "No notify session started");
                }
            }
            (CHARACTERISTIC_1, "WriteValue") => self.write(call),
            (interface, member) => panic!("not expected: {interface}.{member}"),
        }
    }

    fn left(&self, program: &str) {
        self.stop_notify(program);
        self.state().agents.remove(program);
    }
}
