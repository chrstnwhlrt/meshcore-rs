//! Packet type definitions for the `MeshCore` protocol.
//!
//! Packet types are the first byte of a received message and indicate
//! what kind of data follows.

/// Response and push notification packet types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PacketType {
    // Command responses (0x00-0x1F)
    /// Command executed successfully.
    Ok = 0x00,
    /// Command failed with error.
    Error = 0x01,
    /// Start of contact list.
    ContactStart = 0x02,
    /// Contact data.
    Contact = 0x03,
    /// End of contact list.
    ContactEnd = 0x04,
    /// Self device information.
    SelfInfo = 0x05,
    /// Message was sent (with ack code).
    MsgSent = 0x06,
    /// Received a contact message.
    ContactMsgRecv = 0x07,
    /// Received a channel message.
    ChannelMsgRecv = 0x08,
    /// Current device time.
    CurrentTime = 0x09,
    /// No more messages available.
    NoMoreMsgs = 0x0A,
    /// Contact URI/share data.
    ContactUri = 0x0B,
    /// Battery status.
    Battery = 0x0C,
    /// Device information.
    DeviceInfo = 0x0D,
    /// Private key export.
    PrivateKey = 0x0E,
    /// Feature is disabled.
    Disabled = 0x0F,
    /// Contact message with SNR/RSSI (v3).
    ContactMsgRecvV3 = 0x10,
    /// Channel message with SNR/RSSI (v3).
    ChannelMsgRecvV3 = 0x11,
    /// Channel information.
    ChannelInfo = 0x12,
    /// Signature start.
    SignStart = 0x13,
    /// Signature data.
    Signature = 0x14,
    /// Custom variables.
    CustomVars = 0x15,
    /// Statistics response.
    Stats = 0x18,

    // Special command responses (0x32-0x37)
    /// Binary request response.
    BinaryReq = 0x32,
    /// Factory reset.
    FactoryReset = 0x33,
    /// Path discovery.
    PathDiscovery = 0x34,
    /// Set flood scope.
    SetFloodScope = 0x36,
    /// Send control data.
    SendControlData = 0x37,

    // Push notifications (0x80-0x8F)
    /// Advertisement from another device.
    Advertisement = 0x80,
    /// Path update notification.
    PathUpdate = 0x81,
    /// Acknowledgment received.
    Ack = 0x82,
    /// Messages are waiting.
    MessagesWaiting = 0x83,
    /// Raw data received.
    RawData = 0x84,
    /// Login successful.
    LoginSuccess = 0x85,
    /// Login failed.
    LoginFailed = 0x86,
    /// Status response.
    StatusResponse = 0x87,
    /// Log data.
    LogData = 0x88,
    /// Trace data.
    TraceData = 0x89,
    /// New advertisement push.
    PushNewAdvert = 0x8A,
    /// Telemetry response.
    TelemetryResponse = 0x8B,
    /// Binary response.
    BinaryResponse = 0x8C,
    /// Path discovery response.
    PathDiscoveryResponse = 0x8D,
    /// Control data.
    ControlData = 0x8E,
}

impl PacketType {
    /// Attempts to parse a packet type from a byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x00 => Some(Self::Ok),
            0x01 => Some(Self::Error),
            0x02 => Some(Self::ContactStart),
            0x03 => Some(Self::Contact),
            0x04 => Some(Self::ContactEnd),
            0x05 => Some(Self::SelfInfo),
            0x06 => Some(Self::MsgSent),
            0x07 => Some(Self::ContactMsgRecv),
            0x08 => Some(Self::ChannelMsgRecv),
            0x09 => Some(Self::CurrentTime),
            0x0A => Some(Self::NoMoreMsgs),
            0x0B => Some(Self::ContactUri),
            0x0C => Some(Self::Battery),
            0x0D => Some(Self::DeviceInfo),
            0x0E => Some(Self::PrivateKey),
            0x0F => Some(Self::Disabled),
            0x10 => Some(Self::ContactMsgRecvV3),
            0x11 => Some(Self::ChannelMsgRecvV3),
            0x12 => Some(Self::ChannelInfo),
            0x13 => Some(Self::SignStart),
            0x14 => Some(Self::Signature),
            0x15 => Some(Self::CustomVars),
            0x18 => Some(Self::Stats),
            0x32 => Some(Self::BinaryReq),
            0x33 => Some(Self::FactoryReset),
            0x34 => Some(Self::PathDiscovery),
            0x36 => Some(Self::SetFloodScope),
            0x37 => Some(Self::SendControlData),
            0x80 => Some(Self::Advertisement),
            0x81 => Some(Self::PathUpdate),
            0x82 => Some(Self::Ack),
            0x83 => Some(Self::MessagesWaiting),
            0x84 => Some(Self::RawData),
            0x85 => Some(Self::LoginSuccess),
            0x86 => Some(Self::LoginFailed),
            0x87 => Some(Self::StatusResponse),
            0x88 => Some(Self::LogData),
            0x89 => Some(Self::TraceData),
            0x8A => Some(Self::PushNewAdvert),
            0x8B => Some(Self::TelemetryResponse),
            0x8C => Some(Self::BinaryResponse),
            0x8D => Some(Self::PathDiscoveryResponse),
            0x8E => Some(Self::ControlData),
            _ => None,
        }
    }

    /// Returns true if this is a push notification (unsolicited message).
    #[must_use]
    pub const fn is_push(&self) -> bool {
        (*self as u8) >= 0x80
    }

    /// Returns true if this is a response to a command.
    #[must_use]
    pub const fn is_response(&self) -> bool {
        !self.is_push()
    }
}

impl From<PacketType> for u8 {
    fn from(pkt: PacketType) -> Self {
        pkt as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_type_from_byte() {
        assert_eq!(PacketType::from_byte(0x00), Some(PacketType::Ok));
        assert_eq!(PacketType::from_byte(0x80), Some(PacketType::Advertisement));
        assert_eq!(PacketType::from_byte(0xFF), None);
    }

    #[test]
    fn test_is_push() {
        assert!(!PacketType::Ok.is_push());
        assert!(!PacketType::SelfInfo.is_push());
        assert!(PacketType::Advertisement.is_push());
        assert!(PacketType::Ack.is_push());
    }
}
