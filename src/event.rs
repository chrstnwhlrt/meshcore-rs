//! Events the client dispatches for the frames the device sends and for the
//! state of the connection.

use std::sync::Arc;

use tokio::sync::{broadcast, watch};

use crate::error::{Error, ErrorCode, Result};
use crate::types::{
    Ack, AclResponse, AdvertPath, AutoAddConfig, BatteryStatus, BinaryResponse, ChannelData,
    ChannelInfo, ChannelMessage, Contact, ContactList, ContactMessage, ControlData, CoreStats,
    CustomVars, DefaultFloodScope, DeviceInfo, DiscoverResponse, FreqRange, LoginFailed,
    LoginSuccess, MmaResponse, NeighboursResponse, PacketStats, PathDiscovery, PrivateKey,
    PublicKey, RadioStats, RawData, RxLogData, SelfInfo, SentInfo, StatusResponse,
    TelemetryResponse, TraceData, TuningParams,
};
use crate::util::until;

/// An error the device answered with, or a reply that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorInfo {
    /// The device's error code (`RESP_CODE_ERR`).
    pub code: Option<ErrorCode>,
    /// Why a reply could not be read: `invalid_frame_length` when it is too
    /// short (meshcore_py's reason where it checks the length), or
    /// `unknown_stats_type: N`.
    pub reason: Option<String>,
    /// The code of the reply that could not be read.
    pub packet_type: Option<u8>,
}

impl ErrorInfo {
    /// A reply with this code that could not be read.
    pub(crate) fn unreadable(packet_type: u8, reason: impl Into<String>) -> Self {
        Self {
            code: None,
            reason: Some(reason.into()),
            packet_type: Some(packet_type),
        }
    }
}

/// An event. Every frame from the device becomes one (some two), except an
/// empty frame and a push that cannot be read, which are only logged; a reply
/// that cannot be read becomes an [`Event::Error`]. The connection reports
/// its state changes too.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Event {
    /// The connection is up (`connected`).
    Connected {
        /// Where the connection goes: serial port, `host:port` or BLE address.
        info: String,
        /// True after an automatic reconnect.
        reconnected: bool,
    },
    /// The connection is down (`disconnected`). A lost link is not reported
    /// while the client reconnects.
    Disconnected {
        /// Why.
        reason: String,
    },
    /// A command succeeded (`command_ok`).
    Ok {
        /// The 32-bit value of a longer frame (meshcore_py reads it too; the
        /// firmware sends none).
        value: Option<u32>,
    },
    /// A command failed (`command_error`).
    Error(ErrorInfo),
    /// The device's own information (`self_info`).
    SelfInfo(Box<SelfInfo>),
    /// Firmware information (`device_info`).
    DeviceInfo(Box<DeviceInfo>),
    /// Battery and storage (`battery_info`).
    Battery(BatteryStatus),
    /// The device's clock (`time_update`).
    CurrentTime(u32),
    /// Custom variables (`custom_vars`).
    CustomVars(CustomVars),
    /// A contact list begins; the count is the total, not the filtered number.
    ContactsStart {
        /// How many contacts the device holds.
        count: u32,
    },
    /// One contact of a list, or the answer to `get_contact_by_key` (`next_contact`).
    NextContact(Box<Contact>),
    /// A contact list ended (`contacts`).
    Contacts(Arc<ContactList>),
    /// An advert from a node the device did not add as a contact (`new_contact`).
    NewContact(Box<Contact>),
    /// An exported contact (`contact_uri`).
    ContactUri(String),
    /// The device dropped a contact to make room (`contact_deleted`).
    ContactDeleted(PublicKey),
    /// The contact table is full (`contacts_full`).
    ContactsFull,
    /// The device sent a message or request (`message_sent`).
    MessageSent(SentInfo),
    /// A private message (`contact_message`).
    ContactMessage(Box<ContactMessage>),
    /// A channel message (`channel_message`).
    ChannelMessage(Box<ChannelMessage>),
    /// A channel datagram (`channel_data`).
    ChannelData(Box<ChannelData>),
    /// No more messages are queued (`no_more_messages`).
    NoMoreMessages,
    /// Messages are queued on the device (`messages_waiting`).
    MessagesWaiting,
    /// A contact advertised (`advertisement`).
    Advertisement(PublicKey),
    /// A contact's path changed (`path_update`).
    PathUpdate(PublicKey),
    /// A message was delivered (`acknowledgement`).
    Ack(Ack),
    /// Raw data (`raw_data`).
    RawData(Box<RawData>),
    /// A login succeeded (`login_success`).
    LoginSuccess(LoginSuccess),
    /// A login failed (`login_failed`).
    LoginFailed(LoginFailed),
    /// A node's status (`status_response`).
    StatusResponse(Box<StatusResponse>),
    /// A received packet (`rx_log_data`).
    RxLog(Box<RxLogData>),
    /// A trace result (`trace_data`).
    TraceData(Box<TraceData>),
    /// Telemetry (`telemetry_response`).
    TelemetryResponse(Box<TelemetryResponse>),
    /// A raw answer to a binary or anonymous request, a sensor's telemetry
    /// push, or an answer the device could not match (`binary_response`).
    BinaryResponse(Box<BinaryResponse>),
    /// A min/max/average answer (`mma_response`).
    MmaResponse(Box<MmaResponse>),
    /// An access control list (`acl_response`).
    AclResponse(Box<AclResponse>),
    /// A neighbours list (`neighbours_response`).
    NeighboursResponse(Box<NeighboursResponse>),
    /// Core statistics (`stats_core`).
    StatsCore(CoreStats),
    /// Radio statistics (`stats_radio`).
    StatsRadio(RadioStats),
    /// Packet statistics (`stats_packets`).
    StatsPackets(PacketStats),
    /// A channel (`channel_info`).
    ChannelInfo(Box<ChannelInfo>),
    /// A path discovery result (`path_response`).
    PathResponse(Box<PathDiscovery>),
    /// The device's private key (`private_key`).
    PrivateKey(Box<PrivateKey>),
    /// The feature is disabled (`disabled`).
    Disabled,
    /// Radio tuning (`tuning_params`).
    TuningParams(TuningParams),
    /// Control data (`control_data`).
    ControlData(Box<ControlData>),
    /// An answer to a node discovery (`discover_response`).
    DiscoverResponse(Box<DiscoverResponse>),
    /// A signing session started (`sign_start`).
    SignStart {
        /// The most bytes that can be signed.
        max_length: u32,
    },
    /// A signature (`signature`).
    Signature(Vec<u8>),
    /// Frequencies the device may repeat on (`allowed_repeat_freq`).
    AllowedRepeatFreq(Vec<FreqRange>),
    /// The default flood scope, or `None` when unset (`default_flood_scope`).
    DefaultFloodScope(Option<DefaultFloodScope>),
    /// The auto-add configuration (`autoadd_config`).
    AutoAddConfig(AutoAddConfig),
    /// The path an advert arrived over (`advert_path`).
    AdvertPath(Box<AdvertPath>),
    /// The reply to a CLI command (`cli_reply`).
    CliReply(String),
    /// A frame with a code this library does not know.
    Unknown {
        /// The code byte.
        code: u8,
        /// The rest of the frame.
        data: Vec<u8>,
    },
}

/// The kind of an event, without its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum EventKind {
    Connected,
    Disconnected,
    Ok,
    Error,
    SelfInfo,
    DeviceInfo,
    Battery,
    CurrentTime,
    CustomVars,
    ContactsStart,
    NextContact,
    Contacts,
    NewContact,
    ContactUri,
    ContactDeleted,
    ContactsFull,
    MessageSent,
    ContactMessage,
    ChannelMessage,
    ChannelData,
    NoMoreMessages,
    MessagesWaiting,
    Advertisement,
    PathUpdate,
    Ack,
    RawData,
    LoginSuccess,
    LoginFailed,
    StatusResponse,
    RxLog,
    TraceData,
    TelemetryResponse,
    BinaryResponse,
    MmaResponse,
    AclResponse,
    NeighboursResponse,
    StatsCore,
    StatsRadio,
    StatsPackets,
    ChannelInfo,
    PathResponse,
    PrivateKey,
    Disabled,
    TuningParams,
    ControlData,
    DiscoverResponse,
    SignStart,
    Signature,
    AllowedRepeatFreq,
    DefaultFloodScope,
    AutoAddConfig,
    AdvertPath,
    CliReply,
    Unknown,
}

impl EventKind {
    /// meshcore_py's `EventType` value (`"contact_message"`, `"acknowledgement"`, ...).
    /// `"contacts_start"` and `"unknown"` are this library's own: meshcore_py
    /// dispatches nothing for those frames.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Ok => "command_ok",
            Self::Error => "command_error",
            Self::SelfInfo => "self_info",
            Self::DeviceInfo => "device_info",
            Self::Battery => "battery_info",
            Self::CurrentTime => "time_update",
            Self::CustomVars => "custom_vars",
            Self::ContactsStart => "contacts_start",
            Self::NextContact => "next_contact",
            Self::Contacts => "contacts",
            Self::NewContact => "new_contact",
            Self::ContactUri => "contact_uri",
            Self::ContactDeleted => "contact_deleted",
            Self::ContactsFull => "contacts_full",
            Self::MessageSent => "message_sent",
            Self::ContactMessage => "contact_message",
            Self::ChannelMessage => "channel_message",
            Self::ChannelData => "channel_data",
            Self::NoMoreMessages => "no_more_messages",
            Self::MessagesWaiting => "messages_waiting",
            Self::Advertisement => "advertisement",
            Self::PathUpdate => "path_update",
            Self::Ack => "acknowledgement",
            Self::RawData => "raw_data",
            Self::LoginSuccess => "login_success",
            Self::LoginFailed => "login_failed",
            Self::StatusResponse => "status_response",
            Self::RxLog => "rx_log_data",
            Self::TraceData => "trace_data",
            Self::TelemetryResponse => "telemetry_response",
            Self::BinaryResponse => "binary_response",
            Self::MmaResponse => "mma_response",
            Self::AclResponse => "acl_response",
            Self::NeighboursResponse => "neighbours_response",
            Self::StatsCore => "stats_core",
            Self::StatsRadio => "stats_radio",
            Self::StatsPackets => "stats_packets",
            Self::ChannelInfo => "channel_info",
            Self::PathResponse => "path_response",
            Self::PrivateKey => "private_key",
            Self::Disabled => "disabled",
            Self::TuningParams => "tuning_params",
            Self::ControlData => "control_data",
            Self::DiscoverResponse => "discover_response",
            Self::SignStart => "sign_start",
            Self::Signature => "signature",
            Self::AllowedRepeatFreq => "allowed_repeat_freq",
            Self::DefaultFloodScope => "default_flood_scope",
            Self::AutoAddConfig => "autoadd_config",
            Self::AdvertPath => "advert_path",
            Self::CliReply => "cli_reply",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Event {
    /// The kind of the event.
    #[must_use]
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::Connected { .. } => EventKind::Connected,
            Self::Disconnected { .. } => EventKind::Disconnected,
            Self::Ok { .. } => EventKind::Ok,
            Self::Error(_) => EventKind::Error,
            Self::SelfInfo(_) => EventKind::SelfInfo,
            Self::DeviceInfo(_) => EventKind::DeviceInfo,
            Self::Battery(_) => EventKind::Battery,
            Self::CurrentTime(_) => EventKind::CurrentTime,
            Self::CustomVars(_) => EventKind::CustomVars,
            Self::ContactsStart { .. } => EventKind::ContactsStart,
            Self::NextContact(_) => EventKind::NextContact,
            Self::Contacts(_) => EventKind::Contacts,
            Self::NewContact(_) => EventKind::NewContact,
            Self::ContactUri(_) => EventKind::ContactUri,
            Self::ContactDeleted(_) => EventKind::ContactDeleted,
            Self::ContactsFull => EventKind::ContactsFull,
            Self::MessageSent(_) => EventKind::MessageSent,
            Self::ContactMessage(_) => EventKind::ContactMessage,
            Self::ChannelMessage(_) => EventKind::ChannelMessage,
            Self::ChannelData(_) => EventKind::ChannelData,
            Self::NoMoreMessages => EventKind::NoMoreMessages,
            Self::MessagesWaiting => EventKind::MessagesWaiting,
            Self::Advertisement(_) => EventKind::Advertisement,
            Self::PathUpdate(_) => EventKind::PathUpdate,
            Self::Ack(_) => EventKind::Ack,
            Self::RawData(_) => EventKind::RawData,
            Self::LoginSuccess(_) => EventKind::LoginSuccess,
            Self::LoginFailed(_) => EventKind::LoginFailed,
            Self::StatusResponse(_) => EventKind::StatusResponse,
            Self::RxLog(_) => EventKind::RxLog,
            Self::TraceData(_) => EventKind::TraceData,
            Self::TelemetryResponse(_) => EventKind::TelemetryResponse,
            Self::BinaryResponse(_) => EventKind::BinaryResponse,
            Self::MmaResponse(_) => EventKind::MmaResponse,
            Self::AclResponse(_) => EventKind::AclResponse,
            Self::NeighboursResponse(_) => EventKind::NeighboursResponse,
            Self::StatsCore(_) => EventKind::StatsCore,
            Self::StatsRadio(_) => EventKind::StatsRadio,
            Self::StatsPackets(_) => EventKind::StatsPackets,
            Self::ChannelInfo(_) => EventKind::ChannelInfo,
            Self::PathResponse(_) => EventKind::PathResponse,
            Self::PrivateKey(_) => EventKind::PrivateKey,
            Self::Disabled => EventKind::Disabled,
            Self::TuningParams(_) => EventKind::TuningParams,
            Self::ControlData(_) => EventKind::ControlData,
            Self::DiscoverResponse(_) => EventKind::DiscoverResponse,
            Self::SignStart { .. } => EventKind::SignStart,
            Self::Signature(_) => EventKind::Signature,
            Self::AllowedRepeatFreq(_) => EventKind::AllowedRepeatFreq,
            Self::DefaultFloodScope(_) => EventKind::DefaultFloodScope,
            Self::AutoAddConfig(_) => EventKind::AutoAddConfig,
            Self::AdvertPath(_) => EventKind::AdvertPath,
            Self::CliReply(_) => EventKind::CliReply,
            Self::Unknown { .. } => EventKind::Unknown,
        }
    }

    /// True for [`Event::Error`].
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// The tag of answers to tagged requests (meshcore_py's `tag` attribute).
    #[must_use]
    pub fn tag(&self) -> Option<u32> {
        match self {
            Self::BinaryResponse(r) => Some(r.tag),
            Self::StatusResponse(r) => r.tag,
            Self::TelemetryResponse(r) => r.tag,
            Self::MmaResponse(r) => Some(r.tag),
            Self::AclResponse(r) => Some(r.tag),
            Self::NeighboursResponse(r) => r.tag,
            Self::TraceData(t) => Some(t.tag),
            Self::DiscoverResponse(d) => Some(d.tag),
            _ => None,
        }
    }

    /// The 6-byte key prefix of the node an event is about: meshcore_py's
    /// `pubkey_prefix` attribute, and also for path discovery answers,
    /// contacts, adverts, path updates and deleted contacts, whose events in
    /// meshcore_py carry the key under another name.
    #[must_use]
    pub fn pubkey_prefix(&self) -> Option<[u8; 6]> {
        match self {
            Self::ContactMessage(m) => Some(m.pubkey_prefix),
            Self::StatusResponse(r) => Some(r.pubkey_prefix),
            Self::TelemetryResponse(r) => Some(r.pubkey_prefix),
            Self::MmaResponse(r) => Some(r.pubkey_prefix),
            Self::AclResponse(r) => Some(r.pubkey_prefix),
            Self::NeighboursResponse(r) => Some(r.pubkey_prefix),
            Self::PathResponse(p) => Some(p.pubkey_prefix),
            Self::LoginSuccess(l) => l.pubkey_prefix,
            Self::LoginFailed(l) => l.pubkey_prefix,
            Self::NextContact(c) | Self::NewContact(c) => Some(c.public_key.prefix()),
            Self::Advertisement(k) | Self::PathUpdate(k) | Self::ContactDeleted(k) => {
                Some(k.prefix())
            }
            _ => None,
        }
    }
}

/// A subscription to all events.
#[derive(Debug)]
pub struct Subscription {
    receiver: broadcast::Receiver<Event>,
    /// The generation of the link that is up (0: none).
    link: watch::Receiver<u64>,
}

impl Subscription {
    /// The next event; `None` once the client is gone.
    ///
    /// A subscriber that falls more than the event capacity behind loses the
    /// oldest events (logged as a warning).
    pub async fn recv(&mut self) -> Option<Event> {
        next_event(&mut self.receiver).await
    }

    /// The next event while the link with `generation` is up, then
    /// [`Error::ConnectionLost`]. Events from before the link dropped still
    /// come first: the reader dispatches them before it reports the drop.
    pub(crate) async fn recv_on_link(&mut self, generation: u64) -> Result<Event> {
        let gone = self.link.wait_for(|current| *current != generation);
        until(gone, next_event(&mut self.receiver))
            .await
            .flatten()
            .ok_or(Error::ConnectionLost)
    }

    /// The next event if one is queued, without waiting.
    pub fn try_recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => return Some(event),
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    log::warn!(target: "meshcore", "event subscriber fell behind, {n} events lost");
                }
                Err(_) => return None,
            }
        }
    }
}

async fn next_event(receiver: &mut broadcast::Receiver<Event>) -> Option<Event> {
    loop {
        match receiver.recv().await {
            Ok(event) => return Some(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                log::warn!(target: "meshcore", "event subscriber fell behind, {n} events lost");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}

/// Fans events out to subscribers.
#[derive(Debug)]
pub(crate) struct Dispatcher {
    sender: broadcast::Sender<Event>,
    link: watch::Sender<u64>,
}

impl Dispatcher {
    /// How many events a command may fall behind. A list of 350 contacts comes
    /// in at once over TCP, and a task that waits for it runs only now and
    /// then in between.
    pub(crate) const MIN_CAPACITY: usize = 1024;

    /// The buffer is allocated as a whole, so it has a limit.
    const MAX_CAPACITY: usize = 1 << 20;

    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.clamp(Self::MIN_CAPACITY, Self::MAX_CAPACITY);
        let (sender, _) = broadcast::channel(capacity);
        let (link, _) = watch::channel(0);
        Self { sender, link }
    }

    /// Records the link that is up now (`0`: none), ending the waits bound
    /// to the one before.
    pub(crate) fn set_link(&self, generation: u64) {
        self.link.send_replace(generation);
    }

    pub(crate) fn dispatch(&self, event: Event) {
        log::trace!(target: "meshcore", "dispatching {}", event.kind());
        // No subscribers is fine.
        let _ = self.sender.send(event);
    }

    pub(crate) fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.sender.subscribe(),
            link: self.link.subscribe(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_get_events_dispatched_after_subscribing() {
        let dispatcher = Dispatcher::new(16);
        dispatcher.dispatch(Event::Ok { value: None });
        let mut sub = dispatcher.subscribe();
        dispatcher.dispatch(Event::MessagesWaiting);
        assert_eq!(sub.recv().await.unwrap().kind(), EventKind::MessagesWaiting);
        assert!(sub.try_recv().is_none());
    }

    #[tokio::test]
    async fn link_bound_waits_end_when_the_link_drops() {
        let dispatcher = Dispatcher::new(16);
        dispatcher.set_link(1);
        let mut sub = dispatcher.subscribe();
        dispatcher.dispatch(Event::MessagesWaiting);
        dispatcher.set_link(0);
        // what came before the drop is still delivered
        assert_eq!(
            sub.recv_on_link(1).await.unwrap().kind(),
            EventKind::MessagesWaiting
        );
        assert!(matches!(
            sub.recv_on_link(1).await,
            Err(Error::ConnectionLost)
        ));
        // a new link does not revive the old one
        dispatcher.set_link(2);
        assert!(matches!(
            sub.recv_on_link(1).await,
            Err(Error::ConnectionLost)
        ));
        dispatcher.dispatch(Event::NoMoreMessages);
        assert_eq!(
            sub.recv_on_link(2).await.unwrap().kind(),
            EventKind::NoMoreMessages
        );
    }

    #[test]
    fn any_capacity_will_do() {
        for capacity in [0, usize::MAX] {
            Dispatcher::new(capacity).dispatch(Event::MessagesWaiting);
        }
    }

    #[test]
    fn kinds_have_meshcore_py_names() {
        assert_eq!(EventKind::Ack.as_str(), "acknowledgement");
        assert_eq!(Event::NoMoreMessages.kind().as_str(), "no_more_messages");
        assert_eq!(EventKind::RxLog.to_string(), "rx_log_data");
    }

    #[test]
    fn tags_and_key_prefixes_of_events() {
        let key = PublicKey::new([7; 32]);
        let advert = Event::Advertisement(key);
        assert_eq!(advert.pubkey_prefix(), Some([7; 6]));
        assert_eq!(advert.tag(), None);
        let answer = Event::BinaryResponse(Box::new(BinaryResponse {
            tag: 9,
            data: Vec::new(),
        }));
        assert_eq!((answer.tag(), answer.pubkey_prefix()), (Some(9), None));
        assert!(Event::Error(ErrorInfo::unreadable(6, "invalid_frame_length")).is_error());
        assert!(!advert.is_error());
    }
}
