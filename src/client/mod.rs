//! The client: connection handling, events, state and the request machinery.
//! The commands are in the submodules.

mod binary;
mod contacts;
mod device;
mod messaging;

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError, Weak};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;

pub use binary::RequestOptions;
pub use messaging::{
    FloodScope, MAX_CHANNEL_DATA_LENGTH, MAX_TEXT_LENGTH, RetryOptions, TraceRequest,
    parse_trace_path,
};

use crate::error::{Error, Result};
use crate::event::{Dispatcher, ErrorInfo, Event, EventKind, Subscription};
use crate::protocol::frame::DEVICE_MAX_FRAME_SIZE;
use crate::state::State;
use crate::transport::{FrameWriter, LinkEvent, Transport};
use crate::types::{Contact, PublicKey, SelfInfo};

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
    /// How many events a subscriber may fall behind before it loses some (16
    /// at least).
    pub event_capacity: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(15),
            auto_reconnect: false,
            max_reconnect_attempts: 3,
            event_capacity: 1024,
        }
    }
}

/// How long `APP_START` may take when connecting (as meshcore_py).
const APPSTART_TIMEOUT: Duration = Duration::from_secs(2);

struct LinkHandle {
    writer: Box<dyn FrameWriter>,
    reader_task: JoinHandle<()>,
    generation: u64,
}

#[derive(Default)]
struct Background {
    auto_fetch: Option<JoinHandle<()>>,
    auto_update: Option<JoinHandle<()>>,
    reconnect: Option<JoinHandle<()>>,
}

impl Background {
    /// Stops what belongs to the current connection: message fetching and
    /// reconnecting. The automatic contact update is a setting and stays, as
    /// in meshcore_py.
    fn stop_connection_tasks(&mut self) {
        for task in [self.auto_fetch.take(), self.reconnect.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }

    fn abort_all(&mut self) {
        for task in [
            self.auto_fetch.take(),
            self.auto_update.take(),
            self.reconnect.take(),
        ]
        .into_iter()
        .flatten()
        {
            task.abort();
        }
    }
}

struct Inner {
    transport: Box<dyn Transport>,
    options: Options,
    /// In nanoseconds (u64 holds 584 years; longer counts as no limit).
    default_timeout_ns: AtomicU64,
    dispatcher: Dispatcher,
    state: Arc<StdMutex<State>>,
    /// One command at a time: replies are matched to the command in flight.
    cmd_lock: Mutex<()>,
    /// One remote request at a time: the firmware tracks one pending request
    /// of each kind and drops the answer to one that the next replaced.
    mesh_lock: Mutex<()>,
    link: Mutex<Option<LinkHandle>>,
    connected: AtomicBool,
    /// Counts every link opened and every close on purpose: a reconnect
    /// stands down once it moved on since the link was lost.
    generation: AtomicU64,
    connection_info: StdMutex<Option<String>>,
    auto_update_contacts: AtomicBool,
    background: StdMutex<Background>,
    /// Wakes the automatic message fetching once a new link's session started.
    fetch_kick: Arc<Notify>,
    factory_reset_token: StdMutex<Option<String>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        lock(&self.background).abort_all();
        if let Ok(mut link) = self.link.try_lock()
            && let Some(link) = link.take()
        {
            link.reader_task.abort();
        }
    }
}

fn lock<T>(mutex: &StdMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A client for a MeshCore companion radio.
///
/// Cloning is cheap; all clones share one connection. Commands are async
/// methods named as in meshcore_py; they wait for the device's reply and
/// return it typed. Only one command is in flight at a time.
#[derive(Clone)]
pub struct MeshCore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for MeshCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshCore")
            .field("transport", &self.inner.transport.describe())
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl MeshCore {
    /// A client for a transport; [`connect`](Self::connect) opens the link.
    #[must_use]
    pub fn new(transport: impl Transport, options: Options) -> Self {
        let timeout_ns = u64::try_from(options.default_timeout.as_nanos()).unwrap_or(u64::MAX);
        Self {
            inner: Arc::new(Inner {
                transport: Box::new(transport),
                dispatcher: Dispatcher::new(options.event_capacity),
                options,
                default_timeout_ns: AtomicU64::new(timeout_ns),
                state: Arc::new(StdMutex::new(State::new())),
                cmd_lock: Mutex::new(()),
                mesh_lock: Mutex::new(()),
                link: Mutex::new(None),
                connected: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                connection_info: StdMutex::new(None),
                auto_update_contacts: AtomicBool::new(false),
                background: StdMutex::new(Background::default()),
                fetch_kick: Arc::new(Notify::new()),
                factory_reset_token: StdMutex::new(None),
            }),
        }
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

    /// A client for a WiFi companion (not connected yet).
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
    /// connecting fails otherwise, or the device does not answer.
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
    /// up is closed first; automatic message fetching goes on, as in
    /// meshcore_py, and reads what came while no link was up.
    ///
    /// # Errors
    ///
    /// The link cannot be opened, or the device does not answer `APP_START`
    /// (the client is disconnected then, as by [`disconnect`](Self::disconnect)).
    pub async fn connect(&self) -> Result<SelfInfo> {
        // Closes a link that is up, and makes a reconnect under way stand
        // down, which would open a second link.
        self.close_link().await;
        if let Some(task) = lock(&self.inner.background).reconnect.take() {
            task.abort();
        }
        let (info, _) = open_link(&self.inner, None).await?;
        self.inner.dispatcher.dispatch(Event::Connected {
            info,
            reconnected: false,
        });
        match self.send_appstart_timeout(APPSTART_TIMEOUT).await {
            Ok(info) => {
                kick_auto_fetch(&self.inner);
                Ok(info)
            }
            Err(err) => {
                self.disconnect().await?;
                Err(err)
            }
        }
    }

    /// Closes the link, stops automatic message fetching and reconnecting.
    /// Automatic contact updates stay on and work again after the next
    /// [`connect`](Self::connect).
    ///
    /// Over BLE, BlueZ takes the link down 2 s after it is asked to, and this
    /// waits for that, so the device is free for other apps when it returns.
    /// A caller may stop waiting earlier (with a timeout): the client is
    /// disconnected all the same and BlueZ finishes on its own.
    ///
    /// # Errors
    ///
    /// Currently none; kept fallible for transports that may fail closing.
    pub async fn disconnect(&self) -> Result<()> {
        lock(&self.inner.background).stop_connection_tasks();
        self.close_link().await;
        Ok(())
    }

    /// Takes the link down and reports it as `manual_disconnect`. A reconnect
    /// under way stands down, even one whose task is busy on another thread
    /// and stops only later.
    async fn close_link(&self) {
        let link = {
            let mut slot = self.inner.link.lock().await;
            self.inner.dispatcher.set_link(0);
            self.inner.generation.fetch_add(1, Ordering::SeqCst);
            slot.take()
        };
        // Disconnected before the transport closes, which may take a while.
        if self.inner.connected.swap(false, Ordering::SeqCst) {
            self.inner.dispatcher.dispatch(Event::Disconnected {
                reason: "manual_disconnect".into(),
            });
        }
        if let Some(mut link) = link {
            link.reader_task.abort();
            link.writer.close().await;
        }
    }

    /// True while the link is up.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Where the current or last link goes: serial port, `host:port` or the
    /// BLE address.
    #[must_use]
    pub fn connection_info(&self) -> Option<String> {
        lock(&self.inner.connection_info).clone()
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
    /// setters.
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

    /// True when the contacts were not read yet, or adverts or path updates
    /// arrived since they were.
    #[must_use]
    pub fn contacts_dirty(&self) -> bool {
        self.state().contacts_dirty
    }

    /// The `lastmod` of the last contact list, for incremental reads.
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
    /// (any length, case ignored).
    #[must_use]
    pub fn get_contact_by_key_prefix(&self, prefix: &str) -> Option<Contact> {
        let prefix = prefix.trim().to_lowercase();
        if prefix.is_empty() {
            return None;
        }
        self.state()
            .contacts
            .iter()
            .find(|c| c.public_key.to_hex().starts_with(&prefix))
            .cloned()
    }

    /// Puts a contact into the cache (replacing one with the same key).
    pub fn cache_contact(&self, contact: Contact) {
        self.state().upsert_contact(contact);
    }

    /// Removes a contact from the cache and returns it.
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

    /// Whether the contacts are read again (incrementally) on every advert
    /// and path update.
    #[must_use]
    pub fn auto_update_contacts(&self) -> bool {
        self.inner.auto_update_contacts.load(Ordering::SeqCst)
    }

    /// Turns automatic contact updates on or off.
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
                if !matches!(event, Event::Advertisement(_) | Event::PathUpdate(_)) {
                    continue;
                }
                let Some(inner) = weak.upgrade() else { break };
                let client = Self { inner };
                if client.auto_update_contacts()
                    && let Err(err) = client.ensure_contacts(true).await
                {
                    log::warn!(target: "meshcore", "could not update contacts: {err}");
                }
            }
        }));
    }

    /// Reads the contacts when none are cached, or (with `follow`) when they
    /// changed; incremental from the last `lastmod`. Returns whether it read.
    ///
    /// # Errors
    ///
    /// The contact list could not be read.
    pub async fn ensure_contacts(&self, follow: bool) -> Result<bool> {
        let (empty, dirty, lastmod) = {
            let state = self.state();
            (
                state.contacts.is_empty(),
                state.contacts_dirty,
                state.contacts_lastmod,
            )
        };
        if empty || (follow && dirty) {
            self.get_contacts(lastmod).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Fetches queued messages whenever the device reports some (they arrive
    /// as message events), once right away, and again after every
    /// [`connect`](Self::connect) and automatic reconnect: the device reports
    /// only messages that arrive while an app is connected. Must be called
    /// inside a Tokio runtime.
    pub fn start_auto_message_fetching(&self) {
        let weak = Arc::downgrade(&self.inner);
        let kick = Arc::clone(&self.inner.fetch_kick);
        let mut sub = self.subscribe();
        let task = tokio::spawn(async move {
            let fetch = |weak: &Weak<Inner>| {
                let inner = weak.upgrade();
                async move {
                    let Some(inner) = inner else { return false };
                    let client = Self { inner };
                    loop {
                        match client.get_msg().await {
                            Ok(Some(_)) => tokio::time::sleep(Duration::from_millis(100)).await,
                            Ok(None) => break,
                            Err(err) => {
                                log::debug!(target: "meshcore", "stopped fetching messages: {err}");
                                break;
                            }
                        }
                    }
                    true
                }
            };
            if !fetch(&weak).await {
                return;
            }
            while next_fetch_cause(&mut sub, &kick).await {
                if !fetch(&weak).await {
                    break;
                }
            }
        });
        if let Some(old) = lock(&self.inner.background).auto_fetch.replace(task) {
            old.abort();
        }
    }

    /// Stops fetching messages automatically.
    pub fn stop_auto_message_fetching(&self) {
        if let Some(task) = lock(&self.inner.background).auto_fetch.take() {
            task.abort();
        }
    }

    // --------------------------------------------------------------- plumbing

    /// Sends a frame; returns the generation of the link it went out on.
    async fn send_frame(&self, frame: &[u8]) -> Result<u64> {
        // The firmware cuts a longer command short (serial) or drops it (BLE).
        if frame.len() > DEVICE_MAX_FRAME_SIZE {
            return Err(Error::invalid(format!(
                "command of {} bytes, the device takes at most {DEVICE_MAX_FRAME_SIZE}",
                frame.len()
            )));
        }
        let mut link = self.inner.link.lock().await;
        let link = link.as_mut().ok_or(Error::NotConnected)?;
        link.writer.write_frame(frame).await?;
        Ok(link.generation)
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
        let _guard = self.inner.cmd_lock.lock().await;
        self.request_locked(frame, timeout, extract).await
    }

    async fn request_locked<T>(
        &self,
        frame: &[u8],
        timeout: Duration,
        extract: impl FnMut(&Event) -> Option<T>,
    ) -> Result<T> {
        let mut sub = self.subscribe();
        let link = self.send_frame(frame).await?;
        wait_reply(&mut sub, link, timeout, extract).await
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
        let _guard = self.inner.cmd_lock.lock().await;
        self.send_frame(frame).await.map(|_| ())
    }
}

/// Opens the transport and starts the task reading it; returns the link's
/// description and generation. A reconnect passes the generation of the link
/// that was lost: when the client was connected or disconnected since, the
/// new link is closed again and the result is [`Error::NotConnected`].
async fn open_link(inner: &Arc<Inner>, lost: Option<u64>) -> Result<(String, u64)> {
    let link = inner.transport.open().await?;
    let mut slot = inner.link.lock().await;
    if lost.is_some_and(|lost| inner.generation.load(Ordering::SeqCst) != lost) {
        drop(slot);
        let mut writer = link.writer;
        writer.close().await;
        return Err(Error::NotConnected);
    }
    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    // Spawned while the slot is held: a link that closes at once is noticed
    // only after it is stored.
    let reader_task = spawn_reader(Arc::downgrade(inner), link.events, generation);
    let old = slot.replace(LinkHandle {
        writer: link.writer,
        reader_task,
        generation,
    });
    inner.connected.store(true, Ordering::SeqCst);
    inner.dispatcher.set_link(generation);
    drop(slot);
    // Only when two connects raced: close the link that lost.
    if let Some(mut old) = old {
        old.reader_task.abort();
        old.writer.close().await;
    }
    *lock(&inner.connection_info) = Some(link.info.clone());
    Ok((link.info, generation))
}

fn spawn_reader(
    weak: Weak<Inner>,
    mut events: mpsc::Receiver<LinkEvent>,
    generation: u64,
) -> JoinHandle<()> {
    let (state, dispatcher) = match weak.upgrade() {
        Some(inner) => (Arc::clone(&inner.state), inner.dispatcher.clone()),
        None => return tokio::spawn(async {}),
    };
    tokio::spawn(async move {
        let reason = loop {
            match events.recv().await {
                Some(LinkEvent::Frame(frame)) => {
                    let produced = lock(&state).handle_frame(&frame);
                    for event in produced {
                        dispatcher.dispatch(event);
                    }
                }
                Some(LinkEvent::Closed(reason)) => break reason,
                None => break "link closed".to_string(),
            }
        };
        if let Some(inner) = weak.upgrade() {
            link_lost(&inner, generation, reason).await;
        }
    })
}

/// The link with `generation` dropped: reconnect or report it.
async fn link_lost(inner: &Arc<Inner>, generation: u64, reason: String) {
    let link = {
        let mut slot = inner.link.lock().await;
        if slot.as_ref().map(|l| l.generation) != Some(generation) {
            return;
        }
        inner.dispatcher.set_link(0);
        slot.take()
    };
    // Closed outside the lock: closing a BLE link waits for BlueZ.
    if let Some(mut link) = link {
        link.writer.close().await;
    }
    if !inner.connected.swap(false, Ordering::SeqCst) {
        return;
    }
    log::info!(target: "meshcore", "connection lost: {reason}");
    if inner.options.auto_reconnect && inner.options.max_reconnect_attempts > 0 {
        let weak = Arc::downgrade(inner);
        let task = tokio::spawn(reconnect(weak, generation, reason));
        if let Some(old) = lock(&inner.background).reconnect.replace(task) {
            old.abort();
        }
    } else {
        inner.dispatcher.dispatch(Event::Disconnected { reason });
    }
}

/// Reconnects after the link with generation `lost` dropped, and stands down
/// when the client was connected or disconnected meanwhile. It reports while
/// it holds the slot, so its events come before those of a close that takes
/// the new link.
async fn reconnect(weak: Weak<Inner>, lost: u64, reason: String) {
    let attempts = weak
        .upgrade()
        .map_or(0, |i| i.options.max_reconnect_attempts);
    for attempt in 1..=attempts {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(inner) = weak.upgrade() else { return };
        if inner.generation.load(Ordering::SeqCst) != lost {
            return;
        }
        log::debug!(target: "meshcore", "reconnecting ({attempt}/{attempts})");
        match open_link(&inner, Some(lost)).await {
            Ok((info, generation)) => {
                let client = MeshCore { inner };
                // Every new link starts with APP_START, as in meshcore_py: it
                // stops a contact list the device may still be sending and
                // refreshes the self info.
                if let Err(err) = client.send_appstart_timeout(APPSTART_TIMEOUT).await {
                    log::warn!(target: "meshcore", "Reconnect callback failed: {err}");
                }
                let slot = client.inner.link.lock().await;
                if slot
                    .as_ref()
                    .is_some_and(|link| link.generation == generation)
                {
                    client.inner.dispatcher.dispatch(Event::Connected {
                        info,
                        reconnected: true,
                    });
                    kick_auto_fetch(&client.inner);
                }
                return;
            }
            Err(_) if inner.generation.load(Ordering::SeqCst) != lost => return,
            Err(err) => log::debug!(target: "meshcore", "reconnect failed: {err}"),
        }
    }
    let Some(inner) = weak.upgrade() else { return };
    let _slot = inner.link.lock().await;
    if inner.generation.load(Ordering::SeqCst) == lost {
        log::info!(target: "meshcore", "giving up reconnecting after: {reason}");
        inner.dispatcher.dispatch(Event::Disconnected {
            reason: "reconnect_failed".into(),
        });
    }
}

/// Waits for the reply `extract` accepts to a command that went out on the
/// link with generation `link`. The device answers on that link, so losing it
/// ends the wait at once, reconnect or not; an error reply ends it too.
async fn wait_reply<T>(
    sub: &mut Subscription,
    link: u64,
    timeout: Duration,
    mut extract: impl FnMut(&Event) -> Option<T>,
) -> Result<T> {
    let deadline = crate::util::deadline_after(timeout);
    loop {
        let event = tokio::time::timeout_at(deadline, sub.recv_on_link(link))
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
/// response, a login result, a trace). The device pushes it to whichever link
/// is up, so the wait goes on through a reconnect and ends only when the
/// connection is reported gone. Error replies seen meanwhile belong to other
/// commands and are passed over.
async fn wait_response<T>(
    sub: &mut Subscription,
    timeout: Duration,
    mut extract: impl FnMut(&Event) -> Option<T>,
) -> Result<T> {
    let deadline = crate::util::deadline_after(timeout);
    loop {
        let event = match tokio::time::timeout_at(deadline, sub.recv()).await {
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
    let running = lock(&inner.background)
        .auto_fetch
        .as_ref()
        .is_some_and(|task| !task.is_finished());
    if running {
        inner.fetch_kick.notify_one();
    }
}

/// Waits until the automatic message fetching has to read the queue: the
/// device reports waiting messages, or a new link's session started.
/// `false` once the client is gone.
async fn next_fetch_cause(sub: &mut Subscription, kick: &Notify) -> bool {
    enum Wake {
        Kick,
        Event(Option<Event>),
    }
    loop {
        let wake = {
            let mut kicked = pin!(kick.notified());
            let mut event = pin!(sub.recv());
            poll_fn(|cx| {
                if kicked.as_mut().poll(cx).is_ready() {
                    return Poll::Ready(Wake::Kick);
                }
                event.as_mut().poll(cx).map(Wake::Event)
            })
            .await
        };
        match wake {
            Wake::Kick | Wake::Event(Some(Event::MessagesWaiting)) => return true,
            Wake::Event(Some(_)) => {}
            Wake::Event(None) => return false,
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
