//! BLE transport over the Nordic UART service, through BlueZ (feature `ble`,
//! Linux only).
//!
//! Each write to the RX characteristic is one frame and each notification of
//! the TX characteristic is one frame, so no framing bytes are added. As in
//! meshcore_py's `BLEConnection`, the device is found by its address, by a
//! part of its name or as the first MeshCore device in range; frames are
//! written with response, and a write that does not finish within 10 s takes
//! the link down.
//!
//! MeshCore takes frames only over a paired link. A PIN pairs a device that is
//! not paired yet, and pairs it anew when it has forgotten the pairing (after
//! it was flashed); without a PIN an unpaired device is refused before
//! anything is written to it.
//!
//! A device does not advertise while it is connected, so one this computer is
//! connected to counts as in range without advertising. Its link is used when
//! no program uses it (one left behind by a program that ended without
//! disconnecting); while another program gets the device's frames over it, or
//! is connecting to the device, connecting fails with [`Error::DeviceInUse`].
//! When a program lets go of a device, BlueZ shows the link as connected for
//! 2 s more but no longer serves it; connecting then waits for that link to go
//! and connects anew.

use std::fmt;
use std::pin::pin;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bluer::agent::Agent;
use bluer::gatt::WriteOp;
use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::{
    Adapter, AdapterEvent, Address, Device, DeviceEvent, DeviceProperty, DiscoveryFilter,
    DiscoveryTransport, ErrorKind, Session, Uuid,
};
use futures_util::{FutureExt, Stream, StreamExt, stream};
use tokio::sync::{OnceCell, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::transport::{BoxFuture, FrameWriter, LINK_CAPACITY, Link, LinkEvent, Transport};
use crate::util::until;

/// The Nordic UART service.
pub const UART_SERVICE_UUID: Uuid = Uuid::from_u128(0x6E40_0001_B5A3_F393_E0A9_E50E_24DC_CA9E);
/// The characteristic the host writes frames to.
pub const UART_RX_CHAR_UUID: Uuid = Uuid::from_u128(0x6E40_0002_B5A3_F393_E0A9_E50E_24DC_CA9E);
/// The characteristic the device notifies frames on.
pub const UART_TX_CHAR_UUID: Uuid = Uuid::from_u128(0x6E40_0003_B5A3_F393_E0A9_E50E_24DC_CA9E);

/// The prefix MeshCore companions advertise their name with.
pub const NAME_PREFIX: &str = "MeshCore";

/// The largest PIN the firmware takes (six digits).
const MAX_PIN: u32 = 999_999;

/// How long to wait for BlueZ to drop a link a program let go of. BlueZ
/// disconnects 2 s after it was asked to.
const LINK_DROP_WAIT: Duration = Duration::from_secs(3);

/// What BlueZ reports when a link drops while it is being made: the radio link
/// broke off, or the device has forgotten the pairing.
const LINK_DROPPED: [&str; 4] = [
    "le-connection-abort-by-local",
    "br-connection-canceled",
    "le-connection-key-missing",
    "br-connection-key-missing",
];

/// How often to try connecting when the link drops meanwhile.
const CONNECT_TRIES: u32 = 3;

/// How long to wait before trying again.
const RETRY_PAUSE: Duration = Duration::from_millis(500);

/// How long such a link may take to drop. When it takes longer, BlueZ gave up
/// on a device that is not there (after about 40 s), in the same words.
const DROP_TIME: Duration = Duration::from_secs(10);

/// How long a write may take to end after the device disconnected, and how long
/// after a write that failed for that the disconnect may be told. BlueZ does
/// both at once, in either order.
const LINK_END_WAIT: Duration = Duration::from_secs(1);

/// BLE settings.
#[derive(Clone, PartialEq, Eq)]
pub struct BleConfig {
    /// The device: its address (`AA:BB:CC:DD:EE:FF`), a part of its name, or
    /// `None` for the first MeshCore device in range. As in meshcore_py, text
    /// with a `:` is taken for an address, and a reconnect looks for the device
    /// the same way (a name may find another one then).
    pub address: Option<String>,
    /// The PIN to pair with when the device is not paired yet, or has
    /// forgotten the pairing (shown on the device's display, or set in its
    /// firmware). A paired device needs none.
    pub pin: Option<u32>,
    /// How long to scan for the device (10 s, as meshcore_py's scan).
    pub scan_timeout: Duration,
    /// How long connecting, pairing and reading the services may take (20 s).
    pub connect_timeout: Duration,
    /// How long one write may take before the link counts as dead (10 s, as
    /// in meshcore_py).
    pub write_timeout: Duration,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            address: None,
            pin: None,
            scan_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(20),
            write_timeout: Duration::from_secs(10),
        }
    }
}

impl fmt::Debug for BleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BleConfig")
            .field("address", &self.address)
            .field("pin", &self.pin.map(|_| "<redacted>"))
            .field("scan_timeout", &self.scan_timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("write_timeout", &self.write_timeout)
            .finish()
    }
}

impl BleConfig {
    /// Settings for a device address or a part of its name.
    #[must_use]
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: Some(address.into()),
            ..Self::default()
        }
    }

    /// Sets the PIN to pair with.
    #[must_use]
    pub const fn pin(mut self, pin: Option<u32>) -> Self {
        self.pin = pin;
        self
    }

    /// Sets how long to scan for the device.
    #[must_use]
    pub const fn scan_timeout(mut self, timeout: Duration) -> Self {
        self.scan_timeout = timeout;
        self
    }

    /// Sets how long connecting, pairing and reading the services may take.
    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Sets how long one write may take.
    #[must_use]
    pub const fn write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }
}

/// A MeshCore device in range, as [`scan`] finds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BleDevice {
    /// The name (`MeshCore-...`).
    pub name: String,
    /// The address (`AA:BB:CC:DD:EE:FF`).
    pub address: String,
    /// The signal strength in dBm, or `None` for a device this computer is
    /// connected to (it does not advertise while connected).
    pub rssi: Option<i16>,
}

/// Which device the settings ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Address(Address),
    /// A MeshCore device whose name contains this part (any if `None`).
    Name(Option<String>),
}

impl Target {
    fn of(config: &BleConfig) -> Result<Self> {
        match config.address.as_deref() {
            Some(text) if text.contains(':') => Address::from_str(text)
                .map(Self::Address)
                .map_err(|_| Error::invalid(format!("{text} is not a Bluetooth address"))),
            other => Ok(Self::Name(other.map(str::to_string))),
        }
    }

    fn matches(&self, address: Address, name: Option<&str>) -> bool {
        match self {
            Self::Address(wanted) => address == *wanted,
            Self::Name(part) => name.is_some_and(|name| {
                name.starts_with(NAME_PREFIX)
                    && part.as_deref().is_none_or(|part| name.contains(part))
            }),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(address) => write!(f, "{address}"),
            Self::Name(Some(part)) => write!(f, "{NAME_PREFIX} device with \"{part}\" in its name"),
            Self::Name(None) => write!(f, "{NAME_PREFIX} device"),
        }
    }
}

fn check_pin(pin: Option<u32>) -> Result<Option<u32>> {
    match pin {
        Some(pin) if pin > MAX_PIN => Err(Error::invalid("the BLE PIN has at most six digits")),
        pin => Ok(pin),
    }
}

/// The default adapter, if it is on.
async fn powered_adapter(session: &Session) -> Result<Adapter> {
    let adapter = session.default_adapter().await?;
    if !adapter.is_powered().await? {
        return Err(Error::DeviceNotFound(format!(
            "the Bluetooth adapter {} is off",
            adapter.name()
        )));
    }
    Ok(adapter)
}

/// Scans (LE only) until `timeout` or until `wanted` takes a device in range;
/// returns the address it took. Devices this computer is connected to count
/// as in range; they come first, with all devices BlueZ knows already.
async fn discover(
    adapter: &Adapter,
    timeout: Duration,
    mut wanted: impl FnMut(&BleDevice) -> bool,
) -> Result<Option<Address>> {
    adapter
        .set_discovery_filter(DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            ..DiscoveryFilter::default()
        })
        .await?;
    let events = adapter.discover_devices_with_changes().await?;
    let mut events = pin!(events);
    let deadline = crate::util::deadline_after(timeout);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.next()).await {
        let AdapterEvent::DeviceAdded(address) = event else {
            continue;
        };
        let Ok(device) = adapter.device(address) else {
            continue;
        };
        // Devices BlueZ knows from earlier but that are out of range have no
        // RSSI, and neither have connected ones.
        let rssi = device.rssi().await.ok().flatten();
        if rssi.is_none() && !device.is_connected().await.unwrap_or(false) {
            continue;
        }
        let name = device.name().await.ok().flatten().unwrap_or_default();
        let found = BleDevice {
            name,
            address: address.to_string(),
            rssi,
        };
        if wanted(&found) {
            return Ok(Some(address));
        }
    }
    Ok(None)
}

/// Scans for MeshCore devices in range for `timeout`. A device this computer
/// is connected to is listed without RSSI.
///
/// # Errors
///
/// [`Error::DeviceNotFound`] when the adapter is off, [`Error::Ble`] when
/// BlueZ cannot scan.
pub async fn scan(timeout: Duration) -> Result<Vec<BleDevice>> {
    let session = Session::new().await?;
    let adapter = powered_adapter(&session).await?;
    let target = Target::Name(None);
    let mut found: Vec<BleDevice> = Vec::new();
    discover(&adapter, timeout, |device| {
        if let Ok(address) = Address::from_str(&device.address)
            && target.matches(address, Some(&device.name))
        {
            match found
                .iter_mut()
                .find(|known| known.address == device.address)
            {
                Some(known) => *known = device.clone(),
                None => found.push(device.clone()),
            }
        }
        false
    })
    .await?;
    Ok(found)
}

/// Finds the device the settings ask for. A known address is used as is,
/// everything else needs the device in range (advertising or connected).
async fn find_device(adapter: &Adapter, target: &Target, timeout: Duration) -> Result<Device> {
    // The log texts are meshcore_py's, which scans only without an address.
    let by_name = matches!(target, Target::Name(_));
    if let Target::Address(address) = target {
        log::debug!(target: "meshcore", "Connecting using provided address");
        if adapter.device_addresses().await?.contains(address) {
            return Ok(adapter.device(*address)?);
        }
        log::debug!(target: "meshcore", "scanning for {target}");
    } else {
        log::info!(target: "meshcore", "Scanning for devices...");
    }
    let mut name = String::new();
    let found = discover(adapter, timeout, |device| {
        let wanted = Address::from_str(&device.address)
            .is_ok_and(|address| target.matches(address, Some(&device.name)));
        if wanted {
            name.clone_from(&device.name);
        }
        wanted
    })
    .await?;
    let Some(address) = found else {
        if by_name {
            log::warn!(target: "meshcore", "No MeshCore device found during scan.");
        }
        return Err(Error::DeviceNotFound(format!("no {target} in range")));
    };
    if by_name {
        // As bleak prints a device.
        log::info!(target: "meshcore", "Found device: {address}: {name}");
    }
    Ok(adapter.device(address)?)
}

/// Pairs with an agent that answers the device's passkey request with the PIN.
async fn pair(session: &Session, device: &Device, pin: u32) -> Result<()> {
    let agent = Agent {
        request_passkey: Some(Box::new(move |_| Box::pin(async move { Ok(pin) }))),
        request_pin_code: Some(Box::new(move |_| {
            Box::pin(async move { Ok(format!("{pin:06}")) })
        })),
        ..Agent::default()
    };
    // Registered only while pairing; the handle unregisters it when dropped.
    let _agent = session.register_agent(agent).await?;
    log::debug!(target: "meshcore", "Attempting BLE pairing with PIN");
    if let Err(err) = device.pair().await {
        log::error!(target: "meshcore", "BLE pairing failed: {err}");
        return Err(match err.kind {
            ErrorKind::AuthenticationFailed | ErrorKind::AuthenticationRejected => {
                Error::PairingFailed(format!(
                    "{} did not take the PIN, is it the right one? ({err})",
                    device.address()
                ))
            }
            _ => Error::Ble(err),
        });
    }
    log::info!(target: "meshcore", "BLE pairing successful");
    Ok(())
}

/// The error for a device another program is "connected" or "connecting" to.
fn in_use(device: &Device, how: &str) -> Error {
    Error::DeviceInUse(format!(
        "another program on this computer is {how} to {}",
        device.address()
    ))
}

/// Whether connecting failed because the link dropped meanwhile.
fn link_dropped(err: &Error) -> bool {
    matches!(
        err,
        Error::Ble(err)
            if err.kind == ErrorKind::Failed && LINK_DROPPED.contains(&err.message.as_str())
    )
}

/// Connects, or keeps the link BlueZ already has. A new link may drop without
/// the device being at fault, as right after another one to it went down, so
/// this tries a few times.
async fn connect(device: &Device) -> Result<()> {
    let mut tries = 1;
    loop {
        match connect_once(device).await {
            Err(err) if link_dropped(&err) && tries < CONNECT_TRIES => {
                log::debug!(target: "meshcore", "{err}, connecting once more");
                tries += 1;
                // BlueZ may still show the link for a moment.
                link_drops(device).await;
                tokio::time::sleep(RETRY_PAUSE).await;
            }
            result => return result,
        }
    }
}

async fn connect_once(device: &Device) -> Result<()> {
    let start = Instant::now();
    match device.connect().await.map_err(Error::Ble) {
        Err(Error::Ble(err)) if err.kind == ErrorKind::AlreadyConnected => Ok(()),
        Err(Error::Ble(err)) if err.kind == ErrorKind::InProgress => {
            Err(in_use(device, "connecting"))
        }
        Err(err) if link_dropped(&err) && start.elapsed() > DROP_TIME => Err(Error::Timeout),
        result => result,
    }
}

/// Connects, and pairs anew with the PIN when the device seems to have forgotten
/// the pairing, flashed anew: the link then drops each time.
async fn connect_or_repair(
    adapter: &Adapter,
    device: &Device,
    pin: Option<u32>,
    scan_timeout: Duration,
) -> Result<()> {
    let Err(err) = connect(device).await else {
        return Ok(());
    };
    if !link_dropped(&err) || !device.is_paired().await? {
        return Err(err);
    }
    let address = device.address();
    if pin.is_none() {
        return Err(Error::PairingFailed(format!(
            "BlueZ cannot connect to {address}, which may have forgotten the pairing; if it was \
             flashed anew, give its PIN to pair again"
        )));
    }
    log::info!(
        target: "meshcore",
        "BlueZ cannot connect to {address}, which may have forgotten the pairing, pairing again"
    );
    // Removing the device drops the pairing; BlueZ knows the device again
    // once it hears it.
    adapter.remove_device(address).await?;
    let wanted = address.to_string();
    if discover(adapter, scan_timeout, |found| found.address == wanted)
        .await?
        .is_none()
    {
        return Err(Error::DeviceNotFound(format!("{address} is not in range")));
    }
    connect(device).await
}

/// Whether BlueZ drops the device's link within [`LINK_DROP_WAIT`]. After a
/// program lets go of a device, BlueZ keeps showing the link as connected for
/// 2 s but refuses to use it.
async fn link_drops(device: &Device) -> bool {
    let dropped = async {
        let events = device.events().await?;
        let mut events = pin!(events);
        if !device.is_connected().await? {
            return Ok::<_, bluer::Error>(true);
        }
        while let Some(event) = events.next().await {
            if matches!(
                event,
                DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    };
    matches!(
        tokio::time::timeout(LINK_DROP_WAIT, dropped).await,
        Ok(Ok(true))
    )
}

/// Asks BlueZ to disconnect without waiting, for drop handlers. BlueZ keeps
/// a link no program uses (the device then stops advertising), so a link
/// given up without closing it is taken down this way. It needs the Tokio
/// runtime to go on running for a moment, which a program that exits right
/// away does not give it.
fn disconnect_in_background(device: Device) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            if let Err(err) = device.disconnect().await {
                log::debug!(target: "meshcore", "BLE disconnect failed: {err}");
            }
        });
    }
}

/// Takes down the link a transport is making when connecting is given up
/// halfway, as when a caller's timeout drops the future: BlueZ would finish
/// the connection anyway.
#[derive(Default)]
struct CancelGuard(Option<Device>);

impl CancelGuard {
    fn arm(&mut self, device: &Device) {
        self.0 = Some(device.clone());
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(device) = self.0.take() {
            disconnect_in_background(device);
        }
    }
}

/// The RX and TX characteristics of the device's Nordic UART service.
async fn uart_characteristics(device: &Device) -> Result<(Characteristic, Characteristic)> {
    for service in device.services().await? {
        if service.uuid().await? != UART_SERVICE_UUID {
            continue;
        }
        let (mut rx, mut tx) = (None, None);
        for characteristic in service.characteristics().await? {
            let uuid = characteristic.uuid().await?;
            if uuid == UART_RX_CHAR_UUID {
                rx = Some(characteristic);
            } else if uuid == UART_TX_CHAR_UUID {
                tx = Some(characteristic);
            }
        }
        if let (Some(rx), Some(tx)) = (rx, tx) {
            return Ok((rx, tx));
        }
    }
    Err(Error::DeviceNotFound(format!(
        "{} has no Nordic UART service, is it a MeshCore companion?",
        device.address()
    )))
}

/// Connects over BLE.
#[derive(Clone)]
pub struct BleTransport {
    config: BleConfig,
    /// The D-Bus session to BlueZ, made on the first open and kept for
    /// reconnecting.
    session: Arc<OnceCell<Session>>,
}

impl fmt::Debug for BleTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BleTransport")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BleTransport {
    /// A transport for the given settings.
    #[must_use]
    pub fn new(config: BleConfig) -> Self {
        Self {
            config,
            session: Arc::default(),
        }
    }

    /// The settings.
    #[must_use]
    pub const fn config(&self) -> &BleConfig {
        &self.config
    }

    /// Pairs if needed, finds the UART service and starts notifications.
    /// A link that was there before (`existing`) may belong to another
    /// program, which then gets the device's frames already.
    async fn start(
        &self,
        session: &Session,
        device: &Device,
        pin: Option<u32>,
        existing: bool,
    ) -> Result<Link> {
        let (rx, tx) = match uart_characteristics(device).await {
            Ok(characteristics) => characteristics,
            Err(err) => {
                // meshcore_py's text. A link that was there may be one BlueZ
                // is taking down, which the caller connects anew.
                if !existing && matches!(err, Error::DeviceNotFound(_)) {
                    log::error!(target: "meshcore", "Could not find UART service");
                }
                return Err(err);
            }
        };
        if existing && tx.notifying().await? == Some(true) {
            return Err(in_use(device, "connected"));
        }
        if !device.is_paired().await? {
            // MeshCore takes frames only over a paired link. Without a PIN,
            // the first write would leave pairing to the desktop's agent,
            // and a write cut off while that waits leaves BlueZ stuck.
            let Some(pin) = pin else {
                return Err(Error::PairingFailed(format!(
                    "{} is not paired with this computer, give its PIN",
                    device.address()
                )));
            };
            pair(session, device, pin).await?;
        }
        let notifications = match tx.notify().await {
            Ok(notifications) => notifications,
            // Another program took the notifications for itself (AcquireNotify).
            Err(err)
                if existing
                    && err.kind == ErrorKind::NotPermitted
                    && err.message == "Notify acquired" =>
            {
                return Err(in_use(device, "connected"));
            }
            Err(err) => return Err(err.into()),
        };
        let device_events = device.events().await?;
        let (sender, events) = mpsc::channel(LINK_CAPACITY);
        let (writing, writes) = watch::channel(Writing::Nothing);
        let reader_task = tokio::spawn(read_link(notifications, device_events, sender, writes));
        log::info!(target: "meshcore", "BLE Connection started");
        Ok(Link {
            info: device.address().to_string(),
            events,
            writer: Box::new(BleWriter {
                device: Some(device.clone()),
                rx,
                reader_task: Some(reader_task),
                writing,
                write_timeout: self.config.write_timeout,
            }),
        })
    }
}

impl Transport for BleTransport {
    fn open(&self) -> BoxFuture<'_, Result<Link>> {
        Box::pin(async move {
            let config = &self.config;
            let pin = check_pin(config.pin)?;
            let target = Target::of(config)?;
            let session = self.session.get_or_try_init(Session::new).await?;
            let adapter = powered_adapter(session).await?;
            let device = find_device(&adapter, &target, config.scan_timeout).await?;
            log::debug!(target: "meshcore", "connecting to {}", device.address());
            // Armed for a link this call makes, not for one that was there.
            let mut guard = CancelGuard::default();
            let setup = async {
                let had_link = device.is_connected().await?;
                if !had_link {
                    guard.arm(&device);
                }
                connect_or_repair(&adapter, &device, pin, config.scan_timeout).await?;
                match self.start(session, &device, pin, had_link).await {
                    // The link BlueZ had may be one it is taking down.
                    Err(err) if had_link && link_drops(&device).await => {
                        log::debug!(
                            target: "meshcore",
                            "BlueZ closed the link it had ({err}), connecting anew"
                        );
                        guard.arm(&device);
                        connect(&device).await?;
                        self.start(session, &device, pin, false).await
                    }
                    result => result,
                }
            };
            let result = tokio::time::timeout(config.connect_timeout, setup).await;
            guard.disarm();
            let error = match result {
                Ok(Ok(link)) => return Ok(link),
                Ok(Err(err)) => err,
                Err(_) => Error::Timeout,
            };
            // Do not leave a half set up link behind, but leave the link of
            // another program, or its attempt to make one, alone.
            if !matches!(error, Error::DeviceInUse(_))
                && let Err(err) = device.disconnect().await
            {
                log::debug!(target: "meshcore", "BLE disconnect failed: {err}");
            }
            Err(error)
        })
    }

    fn describe(&self) -> String {
        Target::of(&self.config).map_or_else(|_| "BLE".to_string(), |target| target.to_string())
    }
}

/// What the link reader waits for.
enum Input {
    Frame(Vec<u8>),
    Device(DeviceEvent),
}

/// What the writer of a link does, for the link's reader.
#[derive(Debug)]
enum Writing {
    Nothing,
    /// It writes a frame.
    Frame,
    /// It could not write one, which ends the link: why, and how long it may
    /// take until BlueZ tells that the device disconnected.
    Failed(String, Duration),
}

/// Forwards notified frames until the device disconnects or a frame cannot be
/// written, and reports the link closed. BlueZ ends a write the link drops with
/// and tells that the device disconnected in either order. A frame that is being
/// written when the device disconnects may have gone out, and only the end of the
/// write tells, so this waits for it. After a write that ended first, this waits
/// for the disconnect, as the frames from before may come until then.
async fn read_link(
    notifications: impl Stream<Item = Vec<u8>> + Send,
    device_events: impl Stream<Item = DeviceEvent> + Send,
    events: mpsc::Sender<LinkEvent>,
    mut writes: watch::Receiver<Writing>,
) {
    let inputs = stream::select(
        notifications.map(Input::Frame),
        device_events.map(Input::Device),
    );
    let mut inputs = pin!(inputs);
    let mut failed = pin!(write_failed(writes.clone()));
    let mut connected = true;
    loop {
        let input = if connected {
            until(failed.as_mut(), inputs.next()).await
        } else {
            // What came before the device disconnected.
            inputs.next().now_or_never()
        };
        match input.flatten() {
            Some(Input::Frame(frame)) => {
                log::trace!(target: "meshcore", "received a {} byte frame", frame.len());
                if events.send(LinkEvent::Frame(frame)).await.is_err() {
                    return;
                }
            }
            Some(Input::Device(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false)))) => {
                connected = false;
            }
            Some(Input::Device(_)) => {}
            None => break,
        }
    }
    let ended = writes.wait_for(|writing| !matches!(writing, Writing::Frame));
    let _ = tokio::time::timeout(LINK_END_WAIT, ended).await;
    let reason = match &*writes.borrow() {
        Writing::Failed(reason, _) => reason.clone(),
        _ => "ble_disconnect".to_string(),
    };
    log::debug!(target: "meshcore", "link closed: {reason}");
    let _ = events.send(LinkEvent::Closed(reason)).await;
}

/// Waits until a write has failed, and then as long as it may take until BlueZ
/// tells that the device disconnected.
async fn write_failed(mut writes: watch::Receiver<Writing>) {
    let mut wait = None;
    let failed = |writing: &Writing| {
        if let Writing::Failed(_, time) = writing {
            wait = Some(*time);
        }
        wait.is_some()
    };
    let _ = writes.wait_for(failed).await;
    match wait {
        Some(wait) => tokio::time::sleep(wait).await,
        // The writer is gone.
        None => std::future::pending().await,
    }
}

/// Whether a write failed because the link dropped before the device confirmed
/// it: the frame went out, as with a command that restarts the device. BlueZ
/// gives such a write the ATT error "unlikely".
fn unconfirmed(err: &bluer::Error) -> bool {
    err.kind == ErrorKind::Failed && err.message == "Operation failed with ATT error: 0x0e"
}

/// Whether a write failed because the link had dropped before.
fn not_connected(err: &bluer::Error) -> bool {
    err.kind == ErrorKind::Failed && err.message == "Not connected"
}

struct BleWriter {
    /// `None` once closed.
    device: Option<Device>,
    rx: Characteristic,
    reader_task: Option<JoinHandle<()>>,
    /// For the reader, which reports the link closed when a write fails.
    writing: watch::Sender<Writing>,
    write_timeout: Duration,
}

impl FrameWriter for BleWriter {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // A write that failed or stalled leaves the link unusable (a second
            // write racing a stalled one drops it anyway).
            if matches!(*self.writing.borrow(), Writing::Failed(..)) {
                return Err(Error::ConnectionLost);
            }
            self.writing.send_replace(Writing::Frame);
            // With response, as meshcore_py writes.
            let request = CharacteristicWriteRequest {
                op_type: WriteOp::Request,
                ..CharacteristicWriteRequest::default()
            };
            let write = self.rx.write_ext(frame, &request);
            let (reason, wait, result) = match tokio::time::timeout(self.write_timeout, write).await
            {
                Ok(Ok(())) => {
                    self.writing.send_replace(Writing::Nothing);
                    return Ok(());
                }
                Ok(Err(err)) => {
                    log::warn!(target: "meshcore", "BLE write failed: {err}");
                    let reason = format!("ble_write_failed: {err}");
                    if unconfirmed(&err) {
                        (reason, LINK_END_WAIT, Ok(()))
                    } else if not_connected(&err) {
                        (reason, LINK_END_WAIT, Err(Error::ConnectionLost))
                    } else {
                        (reason, Duration::ZERO, Err(Error::Ble(err)))
                    }
                }
                Err(_) => {
                    // meshcore_py's text, with the seconds as a float.
                    log::warn!(
                        target: "meshcore",
                        "BLE write timed out after {:?}s",
                        self.write_timeout.as_secs_f64()
                    );
                    let reason = "ble_write_timeout".to_string();
                    (reason, Duration::ZERO, Err(Error::ConnectionLost))
                }
            };
            self.writing.send_replace(Writing::Failed(reason, wait));
            result
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Some(task) = self.reader_task.take() {
                task.abort();
            }
            if let Some(device) = self.device.take()
                && let Err(err) = device.disconnect().await
            {
                log::debug!(target: "meshcore", "BLE disconnect failed: {err}");
            }
        })
    }
}

impl Drop for BleWriter {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
        // Dropped without closing, as when the runtime shuts down.
        if let Some(device) = self.device.take() {
            disconnect_in_background(device);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> Address {
        Address::from_str(text).unwrap()
    }

    #[test]
    fn text_with_a_colon_is_an_address() {
        let config = BleConfig::new("c0:ff:ee:00:11:22");
        assert_eq!(
            Target::of(&config).unwrap(),
            Target::Address(address("C0:FF:EE:00:11:22"))
        );
        assert!(matches!(
            Target::of(&BleConfig::new("C0:FF")),
            Err(Error::InvalidArgument(_))
        ));
        assert_eq!(
            Target::of(&BleConfig::new("Hamburg")).unwrap(),
            Target::Name(Some("Hamburg".into()))
        );
        assert_eq!(
            Target::of(&BleConfig::default()).unwrap(),
            Target::Name(None)
        );
    }

    #[test]
    fn names_match_like_meshcore_py() {
        let any = address("00:00:00:00:00:01");
        assert!(Target::Name(None).matches(any, Some("MeshCore-Hamburg")));
        assert!(!Target::Name(None).matches(any, Some("OnePlus Buds")));
        assert!(!Target::Name(None).matches(any, None));
        let part = Target::Name(Some("burg".into()));
        assert!(part.matches(any, Some("MeshCore-Hamburg")));
        assert!(!part.matches(any, Some("MeshCore-Berlin")));
        // An address matches whatever the name.
        assert!(Target::Address(any).matches(any, None));
        assert!(!Target::Address(any).matches(address("00:00:00:00:00:02"), Some("MeshCore-x")));
    }

    fn error(kind: ErrorKind, message: &str) -> bluer::Error {
        let message = message.to_string();
        bluer::Error { kind, message }
    }

    #[test]
    fn a_link_that_drops_while_it_is_made_is_told_apart() {
        let tell = |kind, message| link_dropped(&Error::Ble(error(kind, message)));
        for message in LINK_DROPPED {
            assert!(tell(ErrorKind::Failed, message));
            assert!(!tell(ErrorKind::NotReady, message));
        }
        assert!(!tell(ErrorKind::Failed, "le-connection-busy"));
        assert!(!link_dropped(&Error::Timeout));
    }

    #[test]
    fn writes_that_fail_with_the_link_are_told_apart() {
        let failed = |message| error(ErrorKind::Failed, message);
        assert!(unconfirmed(&failed(
            "Operation failed with ATT error: 0x0e"
        )));
        assert!(!unconfirmed(&failed(
            "Operation failed with ATT error: 0x03"
        )));
        assert!(!unconfirmed(&failed("Not connected")));
        assert!(not_connected(&failed("Not connected")));
        assert!(!not_connected(&failed(
            "Operation failed with ATT error: 0x0e"
        )));
    }

    /// The device disconnects, and BlueZ tells no more.
    fn gone() -> impl Stream<Item = DeviceEvent> + Send {
        let event = DeviceEvent::PropertyChanged(DeviceProperty::Connected(false));
        stream::iter([event]).chain(stream::pending())
    }

    #[tokio::test]
    async fn frames_from_before_the_device_disconnects_are_read() {
        let frames = stream::iter([vec![1], vec![2], vec![3]]).chain(stream::pending());
        let (sender, mut events) = mpsc::channel(8);
        let (_writing, writes) = watch::channel(Writing::Nothing);
        read_link(frames, gone(), sender, writes).await;
        let mut read = Vec::new();
        while let Some(event) = events.recv().await {
            read.push(event);
        }
        let mut expected = [1, 2, 3]
            .map(|frame| LinkEvent::Frame(vec![frame]))
            .to_vec();
        expected.push(LinkEvent::Closed("ble_disconnect".into()));
        assert_eq!(read, expected);
    }

    #[tokio::test(start_paused = true)]
    async fn the_link_is_lost_once_the_write_in_flight_is_over() {
        // BlueZ may tell that the device disconnected before it ends the write
        for ends in [true, false] {
            let (writing, writes) = watch::channel(Writing::Frame);
            let (sender, mut events) = mpsc::channel(8);
            let reader = read_link(stream::pending(), gone(), sender, writes);
            let reader = tokio::spawn(reader);
            tokio::time::sleep(LINK_END_WAIT / 2).await;
            assert!(events.try_recv().is_err());
            if ends {
                writing.send_replace(Writing::Failed("failed".into(), LINK_END_WAIT));
            }
            let lost = events.recv();
            let lost = tokio::time::timeout(LINK_END_WAIT / 4, lost).await;
            let reason = ends.then(|| LinkEvent::Closed("failed".into()));
            assert_eq!(lost.ok().flatten(), reason);
            // not held up for long by a write that does not end
            reader.await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_that_failed_ends_the_link_after_the_frames_from_before() {
        async fn after<T>(time: Duration, what: T) -> T {
            tokio::time::sleep(time).await;
            what
        }
        let quarter = LINK_END_WAIT / 4;
        // whether BlueZ may still tell that the device disconnected, whether it
        // does, and when the link is reported closed
        for (wait, told, closed) in [
            (LINK_END_WAIT, true, 2 * quarter),
            (LINK_END_WAIT, false, LINK_END_WAIT),
            (Duration::ZERO, false, Duration::ZERO),
        ] {
            let frame = stream::once(after(quarter, vec![1]));
            let gone = DeviceEvent::PropertyChanged(DeviceProperty::Connected(false));
            let gone = stream::once(after(2 * quarter, told.then_some(gone)));
            let gone = gone.filter_map(std::future::ready);
            let (_writing, writes) = watch::channel(Writing::Failed("failed".into(), wait));
            let (sender, mut events) = mpsc::channel(8);
            let start = Instant::now();
            let reader = read_link(
                frame.chain(stream::pending()),
                gone.chain(stream::pending()),
                sender,
                writes,
            );
            reader.await;
            assert_eq!(start.elapsed().as_millis(), closed.as_millis());
            if !wait.is_zero() {
                assert_eq!(events.recv().await, Some(LinkEvent::Frame(vec![1])));
            }
            let reason = LinkEvent::Closed("failed".into());
            assert_eq!(events.recv().await, Some(reason));
        }
    }

    #[test]
    fn pins_have_six_digits_at_most() {
        assert_eq!(check_pin(Some(123_456)).unwrap(), Some(123_456));
        assert_eq!(check_pin(None).unwrap(), None);
        assert!(matches!(
            check_pin(Some(1_000_000)),
            Err(Error::InvalidArgument(_))
        ));
    }

    #[test]
    fn config_builders() {
        let config = BleConfig::new("MeshCore-1")
            .pin(Some(123_456))
            .scan_timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(30))
            .write_timeout(Duration::from_secs(2));
        assert_eq!(config.pin, Some(123_456));
        assert_eq!(config.scan_timeout, Duration::from_secs(5));
        assert_eq!(config.connect_timeout, Duration::from_secs(30));
        assert_eq!(config.write_timeout, Duration::from_secs(2));
        let defaults = BleConfig::default();
        assert_eq!(defaults.write_timeout, Duration::from_secs(10));
        assert_eq!(defaults.address, None);
    }

    #[test]
    fn the_pin_stays_out_of_debug_output() {
        let config = BleConfig::new("MeshCore-1").pin(Some(123_456));
        let output = format!("{config:?} {:?}", BleTransport::new(config.clone()));
        assert!(output.contains("MeshCore-1"));
        assert!(!output.contains("123456"));
    }

    #[test]
    fn targets_describe_themselves() {
        assert_eq!(Target::Name(None).to_string(), "MeshCore device");
        assert_eq!(
            BleTransport::new(BleConfig::new("Hamburg")).describe(),
            "MeshCore device with \"Hamburg\" in its name"
        );
        assert_eq!(
            BleTransport::new(BleConfig::new("C0:FF:EE:00:11:22")).describe(),
            "C0:FF:EE:00:11:22"
        );
    }
}
