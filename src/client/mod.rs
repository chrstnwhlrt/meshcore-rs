//! The client: connection handling, events, state and the request machinery.
//! The commands are in the submodules.

mod binary;
mod contacts;
mod device;
mod messaging;

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use tokio::sync::{Mutex, Notify, mpsc, oneshot, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;

pub use binary::RequestOptions;
pub use messaging::{
    FloodScope, MAX_CHANNEL_DATA_LENGTH, MAX_TEXT_LENGTH, RetryOptions, TraceRequest,
    parse_trace_path,
};

use crate::error::{Error, Result};
use crate::event::{Dispatcher, ErrorInfo, Event, EventKind, Subscription};
use crate::protocol::frame::DEVICE_MAX_FRAME_SIZE;
use crate::state::{Change, State};
use crate::transport::{FrameWriter, Link, LinkEvent, Transport};
use crate::types::{Contact, PublicKey, SelfInfo, SentInfo};
use crate::util::until;

/// Client settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// How long commands wait for the device's reply (meshcore_py: 15 s;
    /// `Duration::MAX` waits without limit).
    pub default_timeout: Duration,
    /// Reconnect when the link drops.
    pub auto_reconnect: bool,
    /// How often to try reconnecting, one second apart.
    pub max_reconnect_attempts: u32,
    /// How many events a subscriber may fall behind before it loses some
    /// (1024 at least, as the commands take their replies from the events too,
    /// and 1048576 at most).
    pub event_capacity: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(15),
            auto_reconnect: false,
            max_reconnect_attempts: 3,
            event_capacity: Dispatcher::MIN_CAPACITY,
        }
    }
}

/// How long `APP_START` may take when connecting (as meshcore_py).
const APPSTART_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a link may take to close before a new one is opened anyway. BlueZ
/// takes a link down 2 s after it is asked to.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// How long a disconnect lets the command in flight finish. A reply comes
/// within this time, unless the device does not answer.
const LAST_REPLY_WAIT: Duration = Duration::from_millis(500);

struct LinkHandle {
    generation: u64,
    /// A disconnect is taking it down: no more commands go out on it.
    leaving: bool,
    reader_task: JoinHandle<()>,
    /// Frames for the task that writes them. It owns the writer, so a write
    /// goes on when its command is given up, and holds nothing up when the
    /// link is taken down.
    frames: mpsc::Sender<Outgoing>,
    /// That task closes the link once this is dropped.
    _keep_open: oneshot::Sender<()>,
    /// Its sender goes once the link is closed.
    closed: watch::Receiver<()>,
}

/// A frame to write, and where to tell how that went.
struct Outgoing {
    frame: Vec<u8>,
    written: oneshot::Sender<Result<()>>,
}

/// An attempt to open a link, which may be over already.
struct Opening {
    task: AbortHandle,
    /// Its sender goes with the end of the attempt.
    ended: watch::Receiver<()>,
}

/// What has to end before a link is opened, as a serial port can be open only
/// once: the closing of a link, or an attempt to open one that was stopped.
#[derive(Clone)]
struct Ending {
    /// Its sender goes with the end.
    ended: watch::Receiver<()>,
    /// Until when to wait for it.
    until: Instant,
}

impl Ending {
    async fn wait(mut self) {
        let _ = tokio::time::timeout_at(self.until, self.ended.changed()).await;
    }
}

/// The state of the connection. Its events are dispatched while it is locked,
/// so they come in the order of the changes they report.
#[derive(Default)]
struct Connection {
    link: Option<LinkHandle>,
    opening: Option<Opening>,
    endings: Vec<Ending>,
    /// Counts every link opened and every close on purpose: a connect or
    /// reconnect under way stands down once it moved on.
    generation: u64,
    /// Whether the last event about the connection was [`Event::Connected`].
    /// A lost link is not reported while the client reconnects.
    reported_up: bool,
    /// Where the current or last link goes.
    info: Option<String>,
}

impl Connection {
    /// Whether the link with `generation` is up.
    fn has_link(&self, generation: u64) -> bool {
        self.link
            .as_ref()
            .is_some_and(|link| link.generation == generation)
    }

    /// Takes the link down; its task closes it. The sender of what this
    /// returns goes once it is closed.
    fn close_link(&mut self) -> Option<watch::Receiver<()>> {
        let link = self.link.take()?;
        link.reader_task.abort();
        self.add_ending(link.closed.clone());
        Some(link.closed)
    }

    /// Stops the attempt to open a link.
    fn stop_opening(&mut self) {
        if let Some(opening) = self.opening.take() {
            opening.task.abort();
            self.add_ending(opening.ended);
        }
    }

    fn add_ending(&mut self, ended: watch::Receiver<()>) {
        self.endings
            .retain(|ending| ending.ended.has_changed().is_ok());
        self.endings.push(Ending {
            ended,
            until: Instant::now() + CLOSE_WAIT,
        });
    }

    fn report_up(&mut self, dispatcher: &Dispatcher, reconnected: bool) {
        self.reported_up = true;
        dispatcher.dispatch(Event::Connected {
            info: self.info.clone().unwrap_or_default(),
            reconnected,
        });
    }

    /// Reports the connection gone, unless that was reported already.
    fn report_gone(&mut self, dispatcher: &Dispatcher, reason: impl Into<String>) {
        if std::mem::take(&mut self.reported_up) {
            dispatcher.dispatch(Event::Disconnected {
                reason: reason.into(),
            });
        }
    }
}

/// The automatic message fetching.
struct Fetching {
    task: JoinHandle<()>,
    /// Wakes the task once a new link's session started.
    kick: Arc<Notify>,
    /// Once this is dropped, the task ends, after a message it is fetching.
    _go_on: watch::Sender<()>,
}

#[derive(Default)]
struct Background {
    auto_fetch: Option<Fetching>,
    auto_update: Option<JoinHandle<()>>,
    reconnect: Option<JoinHandle<()>>,
}

impl Background {
    fn abort_all(&mut self) {
        let auto_fetch = self.auto_fetch.take().map(|fetching| fetching.task);
        for task in [auto_fetch, self.auto_update.take(), self.reconnect.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

struct Inner {
    transport: Arc<dyn Transport>,
    options: Options,
    /// In nanoseconds (u64 holds 584 years; longer counts as no limit).
    default_timeout_ns: AtomicU64,
    dispatcher: Dispatcher,
    /// Locked after `connection` where both are.
    state: StdMutex<State>,
    /// One command at a time: replies are matched to the command in flight.
    cmd_lock: Mutex<()>,
    /// One remote request at a time, as in meshcore_py: the firmware keeps one
    /// pending request, which the next one replaces whatever its kind.
    mesh_lock: Mutex<()>,
    /// One change of the `CMD_SET_OTHER_PARAMS` settings at a time: a single
    /// setter reads the others and writes them back.
    settings_lock: Mutex<()>,
    /// One signing session at a time: the device has one, and the chunks of
    /// two would mix in it.
    sign_lock: Mutex<()>,
    connection: StdMutex<Connection>,
    auto_update_contacts: AtomicBool,
    /// Locked after `connection` where both are.
    background: StdMutex<Background>,
    factory_reset_token: StdMutex<Option<String>>,
}

impl Inner {
    /// Stops the background tasks and lets go of the link, also of one that is
    /// being opened.
    fn stop(&self) {
        lock(&self.background).abort_all();
        let mut connection = lock(&self.connection);
        connection.generation += 1;
        connection.stop_opening();
        connection.close_link();
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stops the client with the last [`MeshCore`] outside its own tasks: one that
/// waits for the device would keep it alive until the answer comes.
struct StopGuard(Weak<Inner>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            inner.stop();
        }
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A client for a MeshCore companion radio.
///
/// Cloning is cheap; all clones share one connection, which goes with the last
/// of them ([`disconnect`](Self::disconnect) also waits until it is down).
/// Commands are async methods named as in meshcore_py; they wait for the
/// device's reply and return it typed. Only one command is in flight at a time,
/// so one whose future is kept without polling it holds up the others. A reply
/// does not say which command it answers: when it comes only after its command
/// was given up (at its timeout, or its future dropped), it can pass for the
/// reply to the next one, as in meshcore_py.
#[derive(Clone)]
pub struct MeshCore {
    inner: Arc<Inner>,
    /// `None` in the client's own tasks.
    _stop: Option<Arc<StopGuard>>,
}

impl std::fmt::Debug for MeshCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshCore")
            .field("transport", &self.inner.transport.describe())
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

/// A command's turn: frames go out only in one, so replies can be matched to
/// the command in flight. When it ends, also because the command is given up,
/// the reader expects no reply to it any more.
struct Turn<'a> {
    client: &'a MeshCore,
    _lock: tokio::sync::MutexGuard<'a, ()>,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.client.state().end_command();
    }
}

impl Turn<'_> {
    /// Sends a frame; returns the generation of the link it went out on and
    /// the events from right before. `prepare` tells the reader what the reply
    /// is for, when there is a link to send on.
    async fn send(
        &self,
        frame: &[u8],
        prepare: impl FnOnce(&mut State) + Send,
    ) -> Result<(u64, Subscription)> {
        // The firmware cuts a longer command short (serial, Ethernet) or drops
        // it (BLE, WiFi).
        if frame.len() > DEVICE_MAX_FRAME_SIZE {
            return Err(Error::invalid(format!(
                "command of {} bytes, the device takes at most {DEVICE_MAX_FRAME_SIZE}",
                frame.len()
            )));
        }
        let (frames, generation, events) = {
            let connection = lock(&self.client.inner.connection);
            let link = connection.link.as_ref().filter(|link| !link.leaving);
            let link = link.ok_or(Error::NotConnected)?;
            prepare(&mut self.client.state());
            // Subscribed here, the events start on this link: they hold no
            // report that an earlier one is gone.
            (
                link.frames.clone(),
                link.generation,
                self.client.subscribe(),
            )
        };
        let (written, outcome) = oneshot::channel();
        let frame = frame.to_vec();
        // The other ends go when the link is taken down.
        let sent = frames.send(Outgoing { frame, written }).await;
        sent.map_err(|_| Error::ConnectionLost)?;
        outcome.await.map_err(|_| Error::ConnectionLost)??;
        Ok((generation, events))
    }

    /// Sends a command and waits for the reply `extract` accepts; returns it
    /// and the events from then on.
    async fn request<T>(
        &self,
        frame: &[u8],
        timeout: Duration,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<(T, Subscription)> {
        let (link, mut events) = self.send(frame, |_| ()).await?;
        let reply = wait_reply(link, &mut events, timeout, extract).await?;
        Ok((reply, events))
    }

    /// Sends a command that changes what the client holds. The reader notes the
    /// change when the device's answer comes, whenever this future is polled
    /// next; without one, it is noted when the turn ends that the answer is not
    /// known.
    async fn request_change<T>(
        &self,
        frame: &[u8],
        change: Change,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<T> {
        let expect = |state: &mut State| state.change = Some(change);
        let (link, mut events) = self.send(frame, expect).await?;
        let timeout = self.client.default_timeout();
        wait_reply(link, &mut events, timeout, extract).await
    }
}

impl MeshCore {
    /// A client for a transport; [`connect`](Self::connect) opens the link.
    #[must_use]
    pub fn new(transport: impl Transport, options: Options) -> Self {
        let timeout_ns = u64::try_from(options.default_timeout.as_nanos()).unwrap_or(u64::MAX);
        let inner = Arc::new(Inner {
            transport: Arc::new(transport),
            dispatcher: Dispatcher::new(options.event_capacity),
            options,
            default_timeout_ns: AtomicU64::new(timeout_ns),
            state: StdMutex::new(State::new()),
            cmd_lock: Mutex::new(()),
            mesh_lock: Mutex::new(()),
            settings_lock: Mutex::new(()),
            sign_lock: Mutex::new(()),
            connection: StdMutex::default(),
            auto_update_contacts: AtomicBool::new(false),
            background: StdMutex::new(Background::default()),
            factory_reset_token: StdMutex::new(None),
        });
        let stop = Arc::new(StopGuard(Arc::downgrade(&inner)));
        Self {
            inner,
            _stop: Some(stop),
        }
    }

    /// The client for one of its own tasks.
    const fn in_background(inner: Arc<Inner>) -> Self {
        Self { inner, _stop: None }
    }

    /// A client for a serial port with default settings (not connected yet).
    #[must_use]
    pub fn serial(port: impl Into<String>) -> Self {
        Self::new(
            crate::transport::serial::SerialTransport::new(
                crate::transport::serial::SerialConfig::new(port),
            ),
            Options::default(),
        )
    }

    /// A client for a WiFi or Ethernet companion (not connected yet).
    #[must_use]
    pub fn tcp(host: impl Into<String>, port: u16) -> Self {
        Self::new(
            crate::transport::tcp::TcpTransport::new(host, port),
            Options::default(),
        )
    }

    /// Connects over a serial port, as meshcore_py's `create_serial`: when the
    /// device does not answer `APP_START`, it retries with DTR set (if DTR was
    /// not configured), then with DTR inverted.
    ///
    /// On boards with the usual ESP32 auto-reset circuit, such as the LilyGO
    /// T-LoRa V2.1-1.6, a try with DTR off holds the chip in reset while RTS
    /// is on, and Linux turns both lines on when it opens a port. That try
    /// gets no answer, and the board starts again when the port closes,
    /// without the messages waiting in its queue. If it has no real-time
    /// clock, its time is lost with firmware before 1.18 and goes on from
    /// where it stopped with 1.18. The try with DTR set changes nothing on
    /// Linux, where DTR is on already; meshcore_py's tries have the same
    /// effects. [`connect`](Self::connect) on a client made with
    /// [`new`](Self::new) from a [`SerialTransport`](crate::SerialTransport)
    /// tries once, with the lines as configured.
    ///
    /// # Errors
    ///
    /// The port cannot be opened, or the device does not answer.
    pub async fn create_serial(
        config: crate::transport::serial::SerialConfig,
        options: Options,
    ) -> Result<Self> {
        use crate::transport::serial::SerialTransport;
        // Only a node that does not answer is retried; a port that cannot be
        // opened fails at once. The log texts are meshcore_py's.
        let mut config = config;
        let mut client = Self::new(SerialTransport::new(config.clone()), options.clone());
        let mut result = client.connect().await;
        if client.no_answer(&result) && config.dtr.is_none() {
            log::info!(target: "meshcore", "No response from meshcore node, trying to force DTR = True");
            config.dtr = Some(true);
            tokio::time::sleep(Duration::from_secs(1)).await;
            client = Self::new(SerialTransport::new(config.clone()), options.clone());
            result = client.connect().await;
        }
        if client.no_answer(&result) {
            let dtr = !config.dtr.unwrap_or(false);
            log::info!(
                target: "meshcore",
                "No response from meshcore node, trying to invert dtr (now {})",
                if dtr { "True" } else { "False" }
            );
            config.dtr = Some(dtr);
            tokio::time::sleep(Duration::from_secs(1)).await;
            client = Self::new(SerialTransport::new(config), options);
            result = client.connect().await;
        }
        if client.no_answer(&result) {
            log::error!(target: "meshcore", "No response from meshcore node, disconnecting");
            log::error!(target: "meshcore", "Are you sure your node is a serial companion ?");
        }
        result.map(|_| client)
    }

    /// Whether connecting failed after the link was up: the node did not
    /// answer `APP_START`, where meshcore_py's `connect` gives `None`. Only
    /// for a client that was never connected before.
    fn no_answer(&self, result: &Result<SelfInfo>) -> bool {
        result.is_err() && self.connection_info().is_some()
    }

    /// Logs meshcore_py's message for a node that did not answer.
    fn report_no_answer(&self, result: &Result<SelfInfo>) {
        if self.no_answer(result) {
            log::error!(target: "meshcore", "No response from meshcore node, disconnecting");
        }
    }

    /// A client for a BLE companion (not connected yet).
    #[cfg(all(feature = "ble", target_os = "linux"))]
    #[must_use]
    pub fn ble(config: crate::transport::ble::BleConfig) -> Self {
        Self::new(
            crate::transport::ble::BleTransport::new(config),
            Options::default(),
        )
    }

    /// Connects over BLE (meshcore_py's `create_ble`): finds the device,
    /// pairs it with the PIN if it is not paired yet, and starts the session.
    ///
    /// # Errors
    ///
    /// [`Error::DeviceNotFound`] when no matching device is in range,
    /// [`Error::DeviceInUse`] when another program on this computer is
    /// connected to it, [`Error::PairingFailed`] when the device does not
    /// take the PIN or needs one that was not given, [`Error::Ble`] when
    /// connecting fails otherwise, [`Error::Timeout`] when it takes too long
    /// or the device does not answer.
    #[cfg(all(feature = "ble", target_os = "linux"))]
    pub async fn create_ble(
        config: crate::transport::ble::BleConfig,
        options: Options,
    ) -> Result<Self> {
        let client = Self::new(crate::transport::ble::BleTransport::new(config), options);
        let result = client.connect().await;
        client.report_no_answer(&result);
        result.map(|_| client)
    }

    /// Connects over TCP (meshcore_py's `create_tcp`).
    ///
    /// # Errors
    ///
    /// The connection fails or the device does not answer.
    pub async fn create_tcp(host: impl Into<String>, port: u16, options: Options) -> Result<Self> {
        let client = Self::new(
            crate::transport::tcp::TcpTransport::new(host, port),
            options,
        );
        let result = client.connect().await;
        client.report_no_answer(&result);
        result.map(|_| client)
    }

    /// Opens the link and starts the session with `APP_START`. A link that is
    /// up is closed first, at once, so a reply on its way is lost; automatic
    /// message fetching goes on, as in meshcore_py, and reads what came while
    /// no link was up. Given up (its future dropped) while it opens the link,
    /// it leaves the client disconnected; given up later, it leaves the link
    /// up.
    ///
    /// # Errors
    ///
    /// The link cannot be opened, or the device does not answer `APP_START`
    /// (the client is disconnected then, as by [`disconnect`](Self::disconnect)).
    /// [`Error::NotConnected`] or [`Error::ConnectionLost`] when a
    /// `disconnect` or another `connect` came in meanwhile, which counts then.
    pub async fn connect(&self) -> Result<SelfInfo> {
        let (before, closed) = self.take_down(None, false);
        let _ = tokio::time::timeout(CLOSE_WAIT, closed).await;
        let generation = open_link(&Arc::downgrade(&self.inner), before, false).await?;
        match self.send_appstart_timeout(APPSTART_TIMEOUT).await {
            Ok(info) => {
                kick_auto_fetch(&self.inner);
                Ok(info)
            }
            Err(err) => {
                let (_, closed) = self.take_down(Some(generation), true);
                closed.await;
                Err(err)
            }
        }
    }

    /// Closes the link, stops automatic message fetching and reconnecting.
    /// Automatic contact updates stay on and work again after the next
    /// [`connect`](Self::connect). A command in flight has half a second to
    /// finish before: a message the device is handing out would be lost. Those
    /// that wait for their turn are not sent any more.
    ///
    /// The link is down when this returns: a serial port can be opened again,
    /// and a BLE device is free for other apps, for which this waits the 2 s
    /// BlueZ takes. A caller may stop waiting earlier (with a timeout): the
    /// client is disconnected all the same and the link closes on its own. A
    /// BLE link that was still being opened goes down in the background.
    ///
    /// # Errors
    ///
    /// Currently none; kept fallible for transports that may fail closing.
    pub async fn disconnect(&self) -> Result<()> {
        let in_use = self.leave_link();
        let Some(generation) = in_use.filter(|_| self.inner.cmd_lock.try_lock().is_err()) else {
            let (_, closed) = self.take_down(None, true);
            closed.await;
            return Ok(());
        };
        // In a task of its own: the caller may stop polling.
        let client = Self::in_background(Arc::clone(&self.inner));
        let task = tokio::spawn(async move {
            let in_flight = client.inner.cmd_lock.lock();
            let _ = tokio::time::timeout(LAST_REPLY_WAIT, in_flight).await;
            let (_, closed) = client.take_down(Some(generation), true);
            closed.await;
        });
        if let Err(err) = task.await
            && let Ok(panic) = err.try_into_panic()
        {
            std::panic::resume_unwind(panic);
        }
        Ok(())
    }

    /// Lets no more commands go out on the link that is up; returns its
    /// generation.
    fn leave_link(&self) -> Option<u64> {
        let mut connection = lock(&self.inner.connection);
        let link = connection.link.as_mut()?;
        link.leaving = true;
        Some(link.generation)
    }

    /// Takes the link down, stops an attempt to open one, and reports the
    /// connection gone (`manual_disconnect`). With `opened`, all that only when
    /// the client was not connected or disconnected since that generation's
    /// link was opened. Returns the generation from then on and what waits
    /// until the link is closed, and 5 s at most for what was to end before.
    fn take_down(
        &self,
        opened: Option<u64>,
        stop_fetching: bool,
    ) -> (u64, impl Future<Output = ()> + use<>) {
        let mut connection = lock(&self.inner.connection);
        let mut closing = None;
        if opened.is_none_or(|generation| generation == connection.generation) {
            {
                // The automatic contact update is a setting and stays, as in
                // meshcore_py.
                let mut background = lock(&self.inner.background);
                if stop_fetching {
                    background.auto_fetch = None;
                }
                if let Some(task) = background.reconnect.take() {
                    task.abort();
                }
            }
            connection.generation += 1;
            self.inner.dispatcher.set_link(0);
            connection.report_gone(&self.inner.dispatcher, "manual_disconnect");
            connection.stop_opening();
            closing = connection.close_link();
        }
        let endings = connection.endings.clone();
        let closed = async move {
            if let Some(mut closing) = closing {
                let _ = closing.changed().await;
            }
            for ending in endings {
                ending.wait().await;
            }
        };
        (connection.generation, closed)
    }

    /// True while the link is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        lock(&self.inner.connection).link.is_some()
    }

    /// Where the current or last link goes: serial port, `host:port` or the
    /// BLE address.
    #[must_use]
    pub fn connection_info(&self) -> Option<String> {
        lock(&self.inner.connection).info.clone()
    }

    // ---------------------------------------------------------------- events

    /// Subscribes to all events dispatched from now on.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        self.inner.dispatcher.subscribe()
    }

    /// Waits for the next event of a kind; `None` on timeout
    /// (`None` timeout: the default timeout).
    pub async fn wait_for_event(
        &self,
        kind: EventKind,
        timeout: Option<Duration>,
    ) -> Option<Event> {
        self.wait_for(timeout, |event| event.kind() == kind).await
    }

    /// Waits for the next event matching `filter` (meshcore_py's
    /// `attribute_filters`, e.g. `|e| e.tag() == Some(tag)`); `None` on timeout.
    pub async fn wait_for(
        &self,
        timeout: Option<Duration>,
        filter: impl Fn(&Event) -> bool,
    ) -> Option<Event> {
        let mut sub = self.subscribe();
        wait_event(
            &mut sub,
            timeout.unwrap_or_else(|| self.default_timeout()),
            filter,
        )
        .await
    }

    // ----------------------------------------------------------------- state

    /// The default command timeout.
    #[must_use]
    pub fn default_timeout(&self) -> Duration {
        Duration::from_nanos(self.inner.default_timeout_ns.load(Ordering::Relaxed))
    }

    /// Changes the default command timeout.
    pub fn set_default_timeout(&self, timeout: Duration) {
        let ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        self.inner.default_timeout_ns.store(ns, Ordering::Relaxed);
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    /// The device's own information from the last `APP_START`, updated by the
    /// setters (`None` before the first one and after a factory reset).
    #[must_use]
    pub fn self_info(&self) -> Option<SelfInfo> {
        self.state().self_info.clone()
    }

    /// The device clock from the last [`get_time`](Self::get_time).
    #[must_use]
    pub fn time(&self) -> u32 {
        self.state().time
    }

    /// The cached contacts, in the order the device listed them.
    #[must_use]
    pub fn contacts(&self) -> Vec<Contact> {
        self.state().contacts.clone()
    }

    /// A cached contact by key.
    #[must_use]
    pub fn contact(&self, key: &PublicKey) -> Option<Contact> {
        self.state()
            .contacts
            .iter()
            .find(|c| c.public_key == *key)
            .cloned()
    }

    /// True when the contacts may have changed on the device: no list was read
    /// yet, the last one lacked a change the device had reported or left the
    /// cache with other contacts than the device holds, or since it started an
    /// advert or path update arrived, a link was opened (the device reports
    /// those only while an app is connected), or a command that changes a
    /// contact got no answer or was refused for one the cache holds.
    #[must_use]
    pub fn contacts_dirty(&self) -> bool {
        self.state().contacts_dirty
    }

    /// The `lastmod` of the last contact list read that brought contacts, for
    /// incremental reads; 0 when the next one has to be read in full.
    #[must_use]
    pub fn contacts_lastmod(&self) -> u32 {
        self.state().contacts_lastmod
    }

    /// A cached contact by name, ignoring case.
    #[must_use]
    pub fn get_contact_by_name(&self, name: &str) -> Option<Contact> {
        let wanted = name.to_lowercase();
        self.state()
            .contacts
            .iter()
            .find(|c| c.name.to_lowercase() == wanted)
            .cloned()
    }

    /// The first cached contact whose key starts with the hex `prefix`
    /// (any length, case ignored; an empty prefix finds none).
    #[must_use]
    pub fn get_contact_by_key_prefix(&self, prefix: &str) -> Option<Contact> {
        let prefix = prefix.to_lowercase();
        if prefix.is_empty() {
            return None;
        }
        self.state()
            .contacts
            .iter()
            .find(|c| c.public_key.to_hex().starts_with(&prefix))
            .cloned()
    }

    /// Puts a contact into the cache (replacing one with the same key). One
    /// the device does not hold goes with the next full list.
    pub fn cache_contact(&self, contact: Contact) {
        self.state().upsert_contact(contact);
    }

    /// Removes a contact from the cache and returns it. One the device holds
    /// comes back with the next full list.
    #[allow(clippy::must_use_candidate)] // removing is the point; the value is a bonus
    pub fn uncache_contact(&self, key: &PublicKey) -> Option<Contact> {
        self.state().remove_contact(key)
    }

    /// Contacts the device heard but did not add (`NEW_CONTACT` events).
    #[must_use]
    pub fn pending_contacts(&self) -> Vec<Contact> {
        self.state().pending_contacts.clone()
    }

    /// Takes a pending contact out of the list.
    #[allow(clippy::must_use_candidate)] // removing is the point; the value is a bonus
    pub fn pop_pending_contact(&self, key: &PublicKey) -> Option<Contact> {
        let mut state = self.state();
        let index = state
            .pending_contacts
            .iter()
            .position(|c| c.public_key == *key)?;
        Some(state.pending_contacts.remove(index))
    }

    /// Forgets all pending contacts.
    pub fn flush_pending_contacts(&self) {
        self.state().pending_contacts.clear();
    }

    /// Whether channel messages in the RX log are decrypted (with the secrets
    /// of channels read by [`get_channel`](Self::get_channel) or set by
    /// [`set_channel`](Self::set_channel)), and channel messages get path and
    /// signal from the log. Off by default.
    pub fn set_decrypt_channel_logs(&self, decrypt: bool) {
        self.state().parser.decrypt_channels = decrypt;
    }

    /// Whether channel secrets read by [`get_channel`](Self::get_channel) or
    /// set by [`set_channel`](Self::set_channel) are kept to name and decrypt
    /// RX log packets (on by default). Turning it off drops the secrets held
    /// and the channel log.
    pub fn set_keep_channel_secrets(&self, keep: bool) {
        self.state().parser.set_keep_channel_secrets(keep);
    }

    /// Whether channel secrets are kept.
    #[must_use]
    pub fn keep_channel_secrets(&self) -> bool {
        self.state().parser.keep_channel_secrets()
    }

    /// Whether adverts and path updates make the client read the contacts
    /// again (incrementally). Once a list was read, a new link does so too.
    #[must_use]
    pub fn auto_update_contacts(&self) -> bool {
        self.inner.auto_update_contacts.load(Ordering::SeqCst)
    }

    /// Turns automatic contact updates on or off. Turning them on must happen
    /// inside a Tokio runtime: it starts the task that follows the events.
    pub fn set_auto_update_contacts(&self, enabled: bool) {
        self.inner
            .auto_update_contacts
            .store(enabled, Ordering::SeqCst);
        let mut background = lock(&self.inner.background);
        if !enabled {
            if let Some(task) = background.auto_update.take() {
                task.abort();
            }
            return;
        }
        if background
            .auto_update
            .as_ref()
            .is_some_and(|t| !t.is_finished())
        {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        let mut sub = self.subscribe();
        background.auto_update = Some(tokio::spawn(async move {
            while let Some(event) = sub.recv().await {
                // The answer to APP_START, with which every link starts.
                let started = matches!(event, Event::SelfInfo(_));
                if !started && !matches!(event, Event::Advertisement(_) | Event::PathUpdate(_)) {
                    continue;
                }
                let Some(inner) = weak.upgrade() else { break };
                let client = Self::in_background(inner);
                // As in meshcore_py, the first read waits for an advert.
                if started && !client.state().contacts_listed {
                    continue;
                }
                if client.auto_update_contacts()
                    && let Err(err) = client.ensure_contacts(true).await
                {
                    log::warn!(target: "meshcore", "could not update contacts: {err}");
                }
            }
        }));
    }

    /// Reads the contact list when none was read yet or the cache is empty, or
    /// (with `follow`) when the contacts may have changed
    /// ([`contacts_dirty`](Self::contacts_dirty)); incremental from the last
    /// `lastmod`, and in full when the cache is empty or the incremental list
    /// lacks a contact that may have changed (as one that did so in the second
    /// of that `lastmod`, or after the device's clock was set back) or leaves
    /// the cache with another number of contacts than the device holds.
    /// Returns whether it read.
    ///
    /// # Errors
    ///
    /// The contact list could not be read.
    pub async fn ensure_contacts(&self, follow: bool) -> Result<bool> {
        let (missing, dirty, lastmod) = {
            let state = self.state();
            let empty = state.contacts.is_empty();
            (
                !state.contacts_listed || empty,
                state.contacts_dirty,
                if empty { 0 } else { state.contacts_lastmod },
            )
        };
        if missing || (follow && dirty) {
            self.get_contacts(lastmod).await?;
            // It did not bring the cache up to date.
            if lastmod > 0 && self.contacts_lastmod() == 0 {
                self.get_contacts(0).await?;
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Fetches queued messages whenever the device reports some (they arrive
    /// as message events, so subscribe before), once right away, and again
    /// after every [`connect`](Self::connect) and automatic reconnect: the
    /// device reports only messages that arrive while an app is connected.
    /// Must be called inside a Tokio runtime.
    pub fn start_auto_message_fetching(&self) {
        let weak = Arc::downgrade(&self.inner);
        let mut sub = self.subscribe();
        let kick = Arc::new(Notify::new());
        let (go_on, mut stopped) = watch::channel(());
        let task = tokio::spawn({
            let kick = Arc::clone(&kick);
            async move {
                while fetch_queued(&weak, &mut stopped).await {
                    let cause = until(stopped.changed(), next_fetch_cause(&mut sub, &kick)).await;
                    if cause != Some(true) {
                        break;
                    }
                }
            }
        });
        lock(&self.inner.background).auto_fetch = Some(Fetching {
            task,
            kick,
            _go_on: go_on,
        });
    }

    /// Stops fetching messages automatically, after one that is being fetched.
    pub fn stop_auto_message_fetching(&self) {
        lock(&self.inner.background).auto_fetch = None;
    }

    // --------------------------------------------------------------- plumbing

    /// Waits until it is a command's turn.
    async fn turn(&self) -> Turn<'_> {
        let lock = self.inner.cmd_lock.lock().await;
        Turn {
            client: self,
            _lock: lock,
        }
    }

    /// Sends a command and waits for the reply `extract` accepts. Unless
    /// `extract` takes it, an error reply ends the wait with
    /// [`Error::Device`], and a reply that could not be read with
    /// [`Error::Protocol`].
    async fn request<T>(
        &self,
        frame: &[u8],
        timeout: Duration,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<T> {
        let (reply, _) = self.request_followed(frame, timeout, extract).await?;
        Ok(reply)
    }

    /// As [`request`](Self::request), and returns the events from the reply on
    /// too: what the command sets off comes with them.
    async fn request_followed<T>(
        &self,
        frame: &[u8],
        timeout: Duration,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<(T, Subscription)> {
        let turn = self.turn().await;
        turn.request(frame, timeout, extract).await
    }

    /// Sends a command that changes what the client holds
    /// ([`Turn::request_change`]).
    async fn request_change<T>(
        &self,
        frame: &[u8],
        change: Change,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<T> {
        let turn = self.turn().await;
        turn.request_change(frame, change, extract).await
    }

    /// Sends a command the device answers with `MSG_SENT`.
    async fn request_sent(&self, frame: &[u8]) -> Result<(SentInfo, Subscription)> {
        self.request_followed(
            frame,
            self.default_timeout(),
            reply!(Event::MessageSent(sent) => *sent),
        )
        .await
    }

    /// Sends a command whose reply is `OK` (returning its optional value).
    async fn request_ok(&self, frame: &[u8]) -> Result<Option<u32>> {
        self.request(frame, self.default_timeout(), |event| match event {
            Event::Ok { value } => Some(*value),
            _ => None,
        })
        .await
    }

    /// Sends a command the device does not answer.
    async fn send_only(&self, frame: &[u8]) -> Result<()> {
        let turn = self.turn().await;
        turn.send(frame, |_| ()).await.map(drop)
    }
}

/// Opens a link and makes it the client's; returns its generation. When the
/// client is connected or disconnected after the generation `before`, this
/// stands down with [`Error::NotConnected`]. A reconnect reports the link only
/// later.
async fn open_link(weak: &Weak<Inner>, before: u64, reconnect: bool) -> Result<u64> {
    // In a task of its own, which a newer connect or disconnect can stop: the
    // caller may be one nobody polls at the moment.
    let mut task = {
        let inner = weak.upgrade().ok_or(Error::NotConnected)?;
        let mut connection = lock(&inner.connection);
        if connection.generation != before {
            return Err(Error::NotConnected);
        }
        let transport = Arc::clone(&inner.transport);
        let weak = Weak::clone(weak);
        let (opening, ended) = watch::channel(());
        let task = AbortOnDrop(tokio::spawn(async move {
            let link = transport.open().await?;
            let inner = weak.upgrade();
            take_link(inner.as_ref(), link, before, reconnect, opening)
        }));
        connection.opening = Some(Opening {
            task: task.0.abort_handle(),
            ended,
        });
        task
    };
    match (&mut task.0).await {
        Ok(result) => result,
        Err(err) => match err.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            Err(_) => Err(Error::NotConnected),
        },
    }
}

/// A task that stops when nobody waits for it any more.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Makes a link the client's and starts its tasks, for [`open_link`]. The
/// attempt ends with `opening`: once a link that comes too late is closed.
fn take_link(
    inner: Option<&Arc<Inner>>,
    mut link: Link,
    before: u64,
    reconnect: bool,
    opening: watch::Sender<()>,
) -> Result<u64> {
    let wanted = inner
        .map(|inner| (inner, lock(&inner.connection)))
        .filter(|(_, connection)| connection.generation == before);
    let Some((inner, mut connection)) = wanted else {
        tokio::spawn(async move {
            link.writer.close().await;
            drop(opening);
        });
        return Err(Error::NotConnected);
    };
    connection.generation += 1;
    let generation = connection.generation;
    connection.info = Some(link.info);
    // What changed while no link was up was not reported.
    lock(&inner.state).contacts_changed();
    if !reconnect {
        connection.report_up(&inner.dispatcher, false);
    }
    // Spawned while the connection is locked: a link that closes at once is
    // noticed only after it is stored.
    let reader_task = spawn_reader(inner, link.events, generation);
    let (frames, outgoing) = mpsc::channel(1);
    let (keep_open, close) = oneshot::channel();
    let (closing, closed) = watch::channel(());
    tokio::spawn(write_link(link.writer, outgoing, close, closing));
    connection.link = Some(LinkHandle {
        generation,
        leaving: false,
        reader_task,
        frames,
        _keep_open: keep_open,
        closed,
    });
    inner.dispatcher.set_link(generation);
    Ok(generation)
}

/// Writes the frames for a link until it is taken down, and closes it then.
async fn write_link(
    mut writer: Box<dyn FrameWriter>,
    mut outgoing: mpsc::Receiver<Outgoing>,
    mut close: oneshot::Receiver<()>,
    closing: watch::Sender<()>,
) {
    while let Some(Some(Outgoing { frame, written })) = until(&mut close, outgoing.recv()).await {
        // A command given up before its turn is not sent any more.
        if written.is_closed() {
            continue;
        }
        let Some(result) = until(&mut close, writer.write_frame(&frame)).await else {
            break;
        };
        let _ = written.send(result);
    }
    // The commands that wait end now, however long closing takes.
    drop(outgoing);
    writer.close().await;
    drop(closing);
}

fn spawn_reader(
    inner: &Arc<Inner>,
    mut events: mpsc::Receiver<LinkEvent>,
    generation: u64,
) -> JoinHandle<()> {
    let weak = Arc::downgrade(inner);
    tokio::spawn(async move {
        let reason = loop {
            match events.recv().await {
                Some(LinkEvent::Frame(frame)) => {
                    let Some(inner) = weak.upgrade() else { return };
                    // The connection stays locked, so none of the link's frames
                    // is handled once it was taken down.
                    let connection = lock(&inner.connection);
                    if !connection.has_link(generation) {
                        return;
                    }
                    let produced = lock(&inner.state).handle_frame(&frame);
                    for event in produced {
                        inner.dispatcher.dispatch(event);
                    }
                }
                Some(LinkEvent::Closed(reason)) => break reason,
                None => break "link closed".to_string(),
            }
        };
        if let Some(inner) = weak.upgrade() {
            link_lost(&inner, generation, reason);
        }
    })
}

/// The link with `generation` dropped: closes it, and reconnects or reports it.
fn link_lost(inner: &Arc<Inner>, generation: u64, reason: String) {
    let mut connection = lock(&inner.connection);
    if !connection.has_link(generation) {
        return;
    }
    inner.dispatcher.set_link(0);
    connection.close_link();
    log::info!(target: "meshcore", "connection lost: {reason}");
    if inner.options.auto_reconnect && inner.options.max_reconnect_attempts > 0 {
        let task = tokio::spawn(reconnect(Arc::downgrade(inner), generation, reason));
        if let Some(old) = lock(&inner.background).reconnect.replace(task) {
            old.abort();
        }
    } else {
        connection.report_gone(&inner.dispatcher, reason);
    }
}

/// Reconnects after the link with generation `lost` dropped, and stands down
/// when the client was connected or disconnected meanwhile. It does not keep
/// the client alive while it waits or opens a link.
async fn reconnect(weak: Weak<Inner>, lost: u64, reason: String) {
    let attempts = weak
        .upgrade()
        .map_or(0, |i| i.options.max_reconnect_attempts);
    for attempt in 1..=attempts {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let endings = {
            let Some(inner) = weak.upgrade() else { return };
            let connection = lock(&inner.connection);
            if connection.generation != lost {
                return;
            }
            connection.endings.clone()
        };
        for ending in endings {
            ending.wait().await;
        }
        log::debug!(target: "meshcore", "reconnecting ({attempt}/{attempts})");
        let generation = match open_link(&weak, lost, true).await {
            Ok(generation) => generation,
            Err(err) => {
                log::debug!(target: "meshcore", "reconnect failed: {err}");
                continue;
            }
        };
        let Some(inner) = weak.upgrade() else { return };
        let client = MeshCore::in_background(inner);
        // Every new link starts with APP_START, as in meshcore_py: it stops a
        // contact list the device may still be sending and refreshes the self
        // info. A failure is logged in words of its own: meshcore_py's
        // "Reconnect callback failed" comes only from an exception, which its
        // APP_START does not raise.
        if let Err(err) = client.send_appstart_timeout(APPSTART_TIMEOUT).await {
            log::warn!(
                target: "meshcore",
                "could not start the session after reconnecting: {err}"
            );
        }
        let mut connection = lock(&client.inner.connection);
        if connection.has_link(generation) {
            connection.report_up(&client.inner.dispatcher, true);
            kick_auto_fetch(&client.inner);
        }
        return;
    }
    let Some(inner) = weak.upgrade() else { return };
    let mut connection = lock(&inner.connection);
    if connection.generation == lost {
        log::info!(target: "meshcore", "giving up reconnecting after: {reason}");
        connection.report_gone(&inner.dispatcher, "reconnect_failed");
    }
}

/// Waits for the reply `extract` accepts to a command that went out on the
/// link with generation `link`. The device answers on that link, so losing it
/// ends the wait at once, reconnect or not; an error reply ends it too.
async fn wait_reply<T>(
    link: u64,
    events: &mut Subscription,
    timeout: Duration,
    mut extract: impl FnMut(&Event) -> Option<T>,
) -> Result<T> {
    let deadline = crate::util::deadline_after(timeout);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv_on_link(link))
            .await
            .map_err(|_| Error::Timeout)??;
        if let Some(value) = extract(&event) {
            return Ok(value);
        }
        match event {
            Event::Error(info) => return Err(error_from_info(info)),
            Event::Disconnected { .. } => return Err(Error::ConnectionLost),
            _ => {}
        }
    }
}

/// Waits for an answer from the mesh that `extract` accepts (a remote node's
/// response, a login result, a trace) among the events since the request. The
/// device pushes it to whichever link is up, so the wait goes on through a
/// reconnect and ends only when the connection is reported gone. Error replies
/// seen meanwhile belong to other commands and are passed over.
async fn wait_response<T>(
    mut events: Subscription,
    timeout: Duration,
    mut extract: impl FnMut(&Event) -> Option<T>,
) -> Result<T> {
    let deadline = crate::util::deadline_after(timeout);
    loop {
        let event = match tokio::time::timeout_at(deadline, events.recv()).await {
            Err(_) => return Err(Error::Timeout),
            Ok(None) => return Err(Error::ConnectionLost),
            Ok(Some(event)) => event,
        };
        if let Some(value) = extract(&event) {
            return Ok(value);
        }
        if let Event::Disconnected { .. } = event {
            return Err(Error::ConnectionLost);
        }
    }
}

/// Makes the automatic message fetching, when it runs, read the device's
/// queue after a new link's session started: the device reports only
/// messages that arrive while an app is connected.
fn kick_auto_fetch(inner: &Inner) {
    if let Some(fetching) = &lock(&inner.background).auto_fetch {
        fetching.kick.notify_one();
    }
}

/// Reads the device's message queue for the automatic message fetching;
/// `false` once the client is gone or the fetching was stopped (the sender of
/// `stopped` goes then).
async fn fetch_queued(weak: &Weak<Inner>, stopped: &mut watch::Receiver<()>) -> bool {
    let Some(inner) = weak.upgrade() else {
        return false;
    };
    let client = MeshCore::in_background(inner);
    while stopped.has_changed().is_ok() {
        match client.get_msg().await {
            Ok(Some(_)) => {
                let pause = tokio::time::sleep(Duration::from_millis(100));
                until(stopped.changed(), pause).await;
            }
            Ok(None) => return true,
            Err(err) => {
                log::debug!(target: "meshcore", "stopped fetching messages: {err}");
                return true;
            }
        }
    }
    false
}

/// Waits until the automatic message fetching has to read the queue: the
/// device reports waiting messages, or a new link's session started.
/// `false` once the client is gone.
async fn next_fetch_cause(sub: &mut Subscription, kick: &Notify) -> bool {
    loop {
        match until(kick.notified(), sub.recv()).await {
            None | Some(Some(Event::MessagesWaiting)) => return true,
            Some(Some(_)) => {}
            Some(None) => return false,
        }
    }
}

async fn wait_event(
    sub: &mut Subscription,
    timeout: Duration,
    filter: impl Fn(&Event) -> bool,
) -> Option<Event> {
    let deadline = crate::util::deadline_after(timeout);
    loop {
        match tokio::time::timeout_at(deadline, sub.recv()).await {
            Ok(Some(event)) if filter(&event) => return Some(event),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

/// The payload meshcore_py logs for a command that failed with `err`: the
/// device's error code, or `no_event_received` for anything else (a timeout
/// or a link that dropped, which meshcore_py only sees as missing replies).
fn python_payload(err: &Error) -> String {
    match err {
        Error::Device(code) => match code.name() {
            Some(name) => format!("{{'error_code': {}, 'code_string': '{name}'}}", code.code()),
            None => format!("{{'error_code': {}}}", code.code()),
        },
        _ => "{'reason': 'no_event_received'}".to_string(),
    }
}

fn error_from_info(info: ErrorInfo) -> Error {
    match (info.code, info.reason) {
        (Some(code), _) => Error::Device(code),
        (None, Some(reason)) => Error::Protocol(reason),
        (None, None) => Error::protocol("error reply without a code"),
    }
}

/// Takes the matching variant out of an event, cloning its payload.
macro_rules! reply {
    ($pattern:pat $(if $guard:expr)? => $value:expr) => {
        |event: &$crate::event::Event| match event {
            $pattern $(if $guard)? => Some($value),
            _ => None,
        }
    };
}
pub(crate) use reply;
