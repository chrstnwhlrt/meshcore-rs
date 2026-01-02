//! Event system for async message handling.
//!
//! The event system provides a way to handle incoming messages and notifications
//! from the `MeshCore` device asynchronously.

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};

use crate::protocol::PacketType;
use crate::types::{
    Acknowledgment, BatteryStatus, Channel, ChannelMessage, Contact, ContactMessage, CoreStats,
    DeviceInfo, DeviceStatus, PacketStats, PublicKey, RadioStats, SelfInfo, Telemetry,
};

/// Statistics data variants.
#[derive(Debug, Clone)]
pub enum StatsData {
    /// Core statistics.
    Core(CoreStats),
    /// Radio statistics.
    Radio(RadioStats),
    /// Packet statistics.
    Packets(PacketStats),
}

/// Event types that can be dispatched.
#[derive(Debug, Clone)]
pub enum Event {
    /// Connection established.
    Connected,
    /// Connection lost.
    Disconnected,
    /// Command completed successfully.
    Ok,
    /// Command failed with error.
    Error { message: String },
    /// Self info received.
    SelfInfo(Box<SelfInfo>),
    /// Device info received.
    DeviceInfo(Box<DeviceInfo>),
    /// Battery status received.
    Battery(BatteryStatus),
    /// Contact received.
    Contact(Box<Contact>),
    /// Contact list started (contains expected contact count).
    ContactListStart { count: u32 },
    /// Contact list ended (contains last modification timestamp).
    ContactListEnd { last_modified: u32 },
    /// Private message received.
    ContactMessage(Box<ContactMessage>),
    /// Channel message received.
    ChannelMessage(Box<ChannelMessage>),
    /// Message was sent, waiting for ACK.
    MessageSent { expected_ack: u32, timeout_ms: u32 },
    /// ACK received.
    Ack(Acknowledgment),
    /// No more messages available.
    NoMoreMessages,
    /// Messages are waiting on the device.
    MessagesWaiting,
    /// Simple advertisement received (just public key, 0x80).
    Advertisement(PublicKey),
    /// New contact advertisement received (full contact data, 0x8A).
    NewContactAdvert(Box<Contact>),
    /// Status response received.
    StatusResponse(Box<DeviceStatus>),
    /// Current time received.
    CurrentTime(u32),
    /// Statistics response received.
    Stats(StatsData),
    /// Channel information received.
    ChannelInfo(Box<Channel>),
    /// Telemetry response received.
    TelemetryResponse(Box<Telemetry>),
    /// Login was successful.
    LoginSuccess,
    /// Login failed.
    LoginFailed,
    /// Private key received (64 bytes: seed + public key).
    PrivateKey([u8; 64]),
    /// Device is disabled.
    Disabled,
    /// Signature received (variable length).
    Signature(Vec<u8>),
    /// Contact URI received.
    ContactUri(String),
    /// Path update notification (contains public key of updated contact).
    PathUpdate(PublicKey),
    /// Raw binary data received.
    RawData(Vec<u8>),
    /// Log data received.
    LogData(String),
    /// Trace data received.
    TraceData(Vec<u8>),
    /// Custom variables received (comma-separated key:value pairs).
    CustomVars(String),
    /// Binary response received.
    BinaryResponse(Vec<u8>),
    /// Path discovery response received.
    PathDiscoveryResponse(Vec<u8>),
    /// Control data received.
    ControlData(Vec<u8>),
    /// Sign operation started, returns max data length.
    SignStarted { max_length: u32 },
    /// Raw/unknown packet received.
    Raw { packet_type: u8, data: Vec<u8> },
}

impl Event {
    /// Returns the associated packet type if applicable.
    #[must_use]
    pub const fn packet_type(&self) -> Option<PacketType> {
        match self {
            Self::Ok => Some(PacketType::Ok),
            Self::Error { .. } => Some(PacketType::Error),
            Self::SelfInfo(_) => Some(PacketType::SelfInfo),
            Self::DeviceInfo(_) => Some(PacketType::DeviceInfo),
            Self::Battery(_) => Some(PacketType::Battery),
            Self::Contact(_) => Some(PacketType::Contact),
            Self::ContactListStart { .. } => Some(PacketType::ContactStart),
            Self::ContactListEnd { .. } => Some(PacketType::ContactEnd),
            Self::ContactMessage(_) => Some(PacketType::ContactMsgRecv),
            Self::ChannelMessage(_) => Some(PacketType::ChannelMsgRecv),
            Self::MessageSent { .. } => Some(PacketType::MsgSent),
            Self::Ack(_) => Some(PacketType::Ack),
            Self::NoMoreMessages => Some(PacketType::NoMoreMsgs),
            Self::MessagesWaiting => Some(PacketType::MessagesWaiting),
            Self::Advertisement(_) => Some(PacketType::Advertisement),
            Self::NewContactAdvert(_) => Some(PacketType::PushNewAdvert),
            Self::StatusResponse(_) => Some(PacketType::StatusResponse),
            Self::CurrentTime(_) => Some(PacketType::CurrentTime),
            Self::Stats(_) => Some(PacketType::Stats),
            Self::ChannelInfo(_) => Some(PacketType::ChannelInfo),
            Self::TelemetryResponse(_) => Some(PacketType::TelemetryResponse),
            Self::LoginSuccess => Some(PacketType::LoginSuccess),
            Self::LoginFailed => Some(PacketType::LoginFailed),
            Self::PrivateKey(_) => Some(PacketType::PrivateKey),
            Self::Disabled => Some(PacketType::Disabled),
            Self::Signature(_) => Some(PacketType::Signature),
            Self::ContactUri(_) => Some(PacketType::ContactUri),
            Self::PathUpdate(_) => Some(PacketType::PathUpdate),
            Self::RawData(_) => Some(PacketType::RawData),
            Self::LogData(_) => Some(PacketType::LogData),
            Self::TraceData(_) => Some(PacketType::TraceData),
            Self::CustomVars(_) => Some(PacketType::CustomVars),
            Self::BinaryResponse(_) => Some(PacketType::BinaryResponse),
            Self::PathDiscoveryResponse(_) => Some(PacketType::PathDiscoveryResponse),
            Self::ControlData(_) => Some(PacketType::ControlData),
            Self::SignStarted { .. } => Some(PacketType::SignStart),
            Self::Connected | Self::Disconnected | Self::Raw { .. } => None,
        }
    }
}

/// A subscription to events.
pub struct Subscription {
    receiver: broadcast::Receiver<Event>,
}

impl Subscription {
    /// Receives the next event.
    ///
    /// # Errors
    ///
    /// Returns an error if the channel is closed.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Subscription filter for specific event types.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Filter by packet types.
    pub packet_types: Option<Vec<PacketType>>,
    /// Filter for specific ACK code.
    pub ack_code: Option<u32>,
}

impl EventFilter {
    /// Creates a filter for specific packet types.
    #[must_use]
    pub const fn packet_types(types: Vec<PacketType>) -> Self {
        Self {
            packet_types: Some(types),
            ack_code: None,
        }
    }

    /// Creates a filter for a specific ACK code.
    #[must_use]
    pub fn ack(code: u32) -> Self {
        Self {
            packet_types: Some(vec![PacketType::Ack]),
            ack_code: Some(code),
        }
    }

    /// Checks if an event matches this filter.
    #[must_use]
    pub fn matches(&self, event: &Event) -> bool {
        // Check packet type filter
        if let Some(ref types) = self.packet_types {
            if let Some(pkt_type) = event.packet_type() {
                if !types.contains(&pkt_type) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Check ACK code filter
        if let Some(expected_code) = self.ack_code {
            if let Event::Ack(ack) = event {
                if ack.code != expected_code {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

struct EventDispatcherInner {
    sender: broadcast::Sender<Event>,
}

/// Dispatches events to subscribers.
#[derive(Clone)]
pub struct EventDispatcher {
    inner: Arc<EventDispatcherInner>,
    event_tx: mpsc::Sender<Event>,
}

impl EventDispatcher {
    /// Creates a new event dispatcher.
    #[must_use]
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<Event>) {
        let (sender, _) = broadcast::channel(capacity);
        let (event_tx, event_rx) = mpsc::channel(capacity);

        let inner = Arc::new(EventDispatcherInner { sender });

        (Self { inner, event_tx }, event_rx)
    }

    /// Dispatches an event to all subscribers.
    pub fn dispatch(&self, event: Event) {
        // Broadcast to all subscribers (ignore send errors - no receivers is fine)
        let _ = self.inner.sender.send(event);
    }

    /// Queues an event for processing.
    ///
    /// # Errors
    ///
    /// Returns an error if the event channel is closed.
    pub async fn queue(&self, event: Event) -> Result<(), mpsc::error::SendError<Event>> {
        self.event_tx.send(event).await
    }

    /// Subscribes to events with an optional filter.
    ///
    /// Note: The filter parameter is reserved for future use.
    /// Currently, filtering is done via `wait_for`.
    #[must_use]
    pub fn subscribe(&self, _filter: Option<EventFilter>) -> Subscription {
        let receiver = self.inner.sender.subscribe();
        Subscription { receiver }
    }

    /// Waits for an event matching the filter with timeout.
    ///
    /// # Errors
    ///
    /// Returns `None` if the timeout expires or the channel is closed.
    pub async fn wait_for(
        &self,
        filter: EventFilter,
        timeout: std::time::Duration,
    ) -> Option<Event> {
        let mut subscription = self.subscribe(None);

        tokio::select! {
            biased;
            result = async {
                loop {
                    if let Some(event) = subscription.recv().await {
                        if filter.matches(&event) {
                            return Some(event);
                        }
                    } else {
                        return None;
                    }
                }
            } => result,
            () = tokio::time::sleep(timeout) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_dispatch() {
        let (dispatcher, _rx) = EventDispatcher::new(16);
        let mut sub = dispatcher.subscribe(None);

        dispatcher.dispatch(Event::Connected);

        let event = tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv())
            .await
            .unwrap();

        assert!(matches!(event, Some(Event::Connected)));
    }

    #[test]
    fn test_event_filter() {
        let filter = EventFilter::packet_types(vec![PacketType::Ok, PacketType::Error]);

        assert!(filter.matches(&Event::Ok));
        assert!(filter.matches(&Event::Error {
            message: "test".into()
        }));
        assert!(!filter.matches(&Event::Connected));
    }

    #[test]
    fn test_ack_filter() {
        let filter = EventFilter::ack(12345);

        assert!(filter.matches(&Event::Ack(Acknowledgment { code: 12345 })));
        assert!(!filter.matches(&Event::Ack(Acknowledgment { code: 99999 })));
        assert!(!filter.matches(&Event::Ok));
    }
}
