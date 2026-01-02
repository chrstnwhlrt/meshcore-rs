//! Command opcodes for the `MeshCore` protocol.
//!
//! Commands are sent to the device to perform actions or request data.
//! Each command starts with an opcode byte, optionally followed by parameters.

/// Command opcodes sent to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CommandOpcode {
    // Basic device commands
    /// Initialize connection, returns `SelfInfo`.
    AppStart = 0x01,
    /// Send a private message (followed by subtype).
    SendMessage = 0x02,
    /// Send a channel message.
    SendChannelMsg = 0x03,
    /// Get contact list.
    GetContacts = 0x04,
    /// Get current device time.
    GetTime = 0x05,
    /// Set device time.
    SetTime = 0x06,
    /// Send advertisement.
    SendAdvert = 0x07,
    /// Set device name.
    SetName = 0x08,
    /// Update contact.
    UpdateContact = 0x09,
    /// Get next waiting message.
    GetMessage = 0x0A,
    /// Set radio parameters.
    SetRadio = 0x0B,
    /// Set TX power.
    SetTxPower = 0x0C,
    /// Reset path for a contact.
    ResetPath = 0x0D,
    /// Set device coordinates.
    SetCoords = 0x0E,
    /// Remove a contact.
    RemoveContact = 0x0F,
    /// Share contact (generate URI).
    ShareContact = 0x10,
    /// Export contact.
    ExportContact = 0x11,
    /// Import contact.
    ImportContact = 0x12,
    /// Reboot device.
    Reboot = 0x13,
    /// Get battery status.
    GetBattery = 0x14,
    /// Set tuning parameters.
    SetTuning = 0x15,
    /// Query device info.
    DeviceQuery = 0x16,
    /// Export private key.
    ExportPrivateKey = 0x17,
    /// Import private key.
    ImportPrivateKey = 0x18,
    /// Send login request.
    SendLogin = 0x1A,
    /// Send status request.
    SendStatusReq = 0x1B,
    /// Send logout.
    SendLogout = 0x1D,
    /// Get channel info.
    GetChannel = 0x1F,
    /// Set channel.
    SetChannel = 0x20,
    /// Start signature.
    SignStart = 0x21,
    /// Sign data chunk.
    SignData = 0x22,
    /// Finish signature.
    SignFinish = 0x23,
    /// Send trace path request.
    SendTrace = 0x24,
    /// Set device PIN.
    SetDevicePin = 0x25,
    /// Set other parameters (`manual_add`, `telemetry_mode`, etc.).
    SetOtherParams = 0x26,
    /// Get/send telemetry.
    Telemetry = 0x27,
    /// Get custom variables.
    GetCustomVars = 0x28,
    /// Set custom variable.
    SetCustomVar = 0x29,
    /// Binary request.
    BinaryReq = 0x32,
    /// Path discovery.
    PathDiscovery = 0x34,
    /// Set flood scope.
    SetFloodScope = 0x36,
    /// Send control data.
    SendControlData = 0x37,
    /// Get statistics.
    GetStats = 0x38,
}

impl From<CommandOpcode> for u8 {
    fn from(cmd: CommandOpcode) -> Self {
        cmd as Self
    }
}

/// Message send subtypes (used with `SendMessage` command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageType {
    /// Private message to a contact.
    Private = 0x00,
    /// Command to a contact.
    Command = 0x01,
}

impl From<MessageType> for u8 {
    fn from(msg: MessageType) -> Self {
        msg as Self
    }
}

/// Binary request types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BinaryReqType {
    /// Request device status.
    Status = 0x01,
    /// Keep-alive ping/heartbeat.
    KeepAlive = 0x02,
    /// Request telemetry data (Cayenne LPP format).
    Telemetry = 0x03,
    /// Request min/max/avg measurements.
    Mma = 0x04,
    /// Request access control list.
    Acl = 0x05,
    /// Request neighbours list.
    Neighbours = 0x06,
}

impl BinaryReqType {
    /// Returns true if this request type expects a response.
    #[must_use]
    pub const fn expects_response(&self) -> bool {
        !matches!(self, Self::KeepAlive)
    }
}

impl From<BinaryReqType> for u8 {
    fn from(req: BinaryReqType) -> Self {
        req as Self
    }
}

/// Statistics types for `GetStats` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StatsType {
    /// Core statistics (battery, uptime, errors, queue).
    Core = 0x00,
    /// Radio statistics (noise floor, RSSI, airtime).
    Radio = 0x01,
    /// Packet statistics (sent/received counts).
    Packets = 0x02,
}

impl From<StatsType> for u8 {
    fn from(stats: StatsType) -> Self {
        stats as Self
    }
}

/// Control data types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlDataType {
    /// Node discovery request.
    NodeDiscoverReq = 0x80,
}

impl From<ControlDataType> for u8 {
    fn from(ctrl: ControlDataType) -> Self {
        ctrl as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_opcode_values() {
        assert_eq!(CommandOpcode::AppStart as u8, 0x01);
        assert_eq!(CommandOpcode::SendMessage as u8, 0x02);
        assert_eq!(CommandOpcode::GetContacts as u8, 0x04);
        assert_eq!(CommandOpcode::GetBattery as u8, 0x14);
        assert_eq!(CommandOpcode::DeviceQuery as u8, 0x16);
        assert_eq!(CommandOpcode::BinaryReq as u8, 0x32);
    }

    #[test]
    fn test_message_type_values() {
        assert_eq!(MessageType::Private as u8, 0x00);
        assert_eq!(MessageType::Command as u8, 0x01);
    }

    #[test]
    fn test_stats_type_values() {
        assert_eq!(StatsType::Core as u8, 0x00);
        assert_eq!(StatsType::Radio as u8, 0x01);
        assert_eq!(StatsType::Packets as u8, 0x02);
    }

    #[test]
    fn test_binary_req_type_values() {
        assert_eq!(BinaryReqType::Status as u8, 0x01);
        assert_eq!(BinaryReqType::KeepAlive as u8, 0x02);
        assert_eq!(BinaryReqType::Telemetry as u8, 0x03);
    }

    #[test]
    fn test_command_from_conversion() {
        let cmd: u8 = CommandOpcode::AppStart.into();
        assert_eq!(cmd, 0x01);
    }
}
