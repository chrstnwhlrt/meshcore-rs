//! Received and sent messages.

use crate::protocol::path::PathLen;

/// The text type of a message (`TXT_TYPE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextType {
    /// A plain text message.
    #[default]
    Plain,
    /// A CLI command or its reply (`TXT_TYPE_CLI_DATA`).
    CliData,
    /// A post a room server passes on, marked with its author
    /// (`TXT_TYPE_SIGNED_PLAIN`).
    SignedPlain,
    /// An explicit CLI command (`TXT_TYPE_CLI_COMMAND`).
    CliCommand,
    /// A type this library does not know.
    Other(u8),
}

impl TextType {
    /// Maps the type byte.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Plain,
            1 => Self::CliData,
            2 => Self::SignedPlain,
            3 => Self::CliCommand,
            other => Self::Other(other),
        }
    }

    /// The type byte.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Plain => 0,
            Self::CliData => 1,
            Self::SignedPlain => 2,
            Self::CliCommand => 3,
            Self::Other(code) => code,
        }
    }
}

/// A private message (`RESP_CODE_CONTACT_MSG_RECV`, `_V3`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContactMessage {
    /// The sender's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// Hops of a flooded message, or [`PathLen::NONE`] for a direct one.
    pub path_len: PathLen,
    /// The text type.
    pub txt_type: TextType,
    /// When the sender sent it, by its clock.
    pub sender_timestamp: u32,
    /// For a room server's post ([`TextType::SignedPlain`]): the first 4
    /// bytes of its author's public key (meshcore_py calls it `signature`).
    pub signature: Option<[u8; 4]>,
    /// The text.
    pub text: String,
    /// SNR in dB of the received packet (v3 frames only, which the device
    /// sends once [`MeshCore::send_device_query`](crate::MeshCore::send_device_query)
    /// asked for protocol version 3).
    pub snr: Option<f32>,
}

/// A group channel message (`RESP_CODE_CHANNEL_MSG_RECV`, `_V3`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMessage {
    /// The channel's index on the device.
    pub channel_idx: u8,
    /// Hops of a flooded message, or [`PathLen::NONE`] for a direct one.
    pub path_len: PathLen,
    /// The text type.
    pub txt_type: TextType,
    /// When the sender sent it, by its clock.
    pub sender_timestamp: u32,
    /// The text, usually `"sender: message"`.
    pub text: String,
    /// SNR in dB of the received packet (v3 frames only, which the device
    /// sends once [`MeshCore::send_device_query`](crate::MeshCore::send_device_query)
    /// asked for protocol version 3).
    pub snr: Option<f32>,
    /// Hash of timestamp and text, computed when channel log decryption is
    /// on, to find the packet in the RX log (meshcore_py reports it for v3
    /// frames only).
    pub txt_hash: Option<u32>,
    /// What the RX log recorded for this message's packet, when channel log
    /// decryption is on and the packet was logged.
    pub rx_info: Option<ChannelRxInfo>,
    /// The frame was a v3 frame (affects meshcore_py's field layout).
    pub(crate) v3: bool,
}

/// What the RX log recorded about a channel message's packet.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRxInfo {
    /// The path the packet took.
    pub path: Vec<u8>,
    /// RSSI in dBm.
    pub rssi: i8,
    /// SNR in dB.
    pub snr: f32,
    /// When the packet was received (Unix seconds, host clock).
    pub recv_time: u64,
    /// The sender's attempt counter.
    pub attempt: u8,
}

/// A binary group datagram (`RESP_CODE_CHANNEL_DATA_RECV`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelData {
    /// SNR in dB of the received packet.
    pub snr: f32,
    /// The channel's index on the device.
    pub channel_idx: u8,
    /// Hops of a flooded datagram, or [`PathLen::NONE`] for a direct one.
    pub path_len: PathLen,
    /// The application's data type.
    pub data_type: u16,
    /// The payload.
    pub payload: Vec<u8>,
}

/// A message fetched with [`MeshCore::get_msg`](crate::MeshCore::get_msg).
#[derive(Debug, Clone, PartialEq)]
pub enum ReceivedMessage {
    /// A private message.
    Contact(ContactMessage),
    /// A channel message.
    Channel(ChannelMessage),
    /// A channel datagram.
    ChannelData(ChannelData),
}

/// The device's reply to a send command (`RESP_CODE_SENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentInfo {
    /// True when the device flooded the packet, false for a direct send.
    pub flood: bool,
    /// The ACK code to expect, or the tag the response will carry.
    pub expected_ack: u32,
    /// How long the device suggests to wait, in milliseconds.
    pub suggested_timeout: u32,
}

/// A delivery confirmation (`PUSH_CODE_SEND_CONFIRMED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ack {
    /// The ACK code, as [`SentInfo::expected_ack`].
    pub code: u32,
    /// The round trip time in milliseconds.
    pub trip_time: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_type_codes() {
        for code in 0..=255u8 {
            assert_eq!(TextType::from_code(code).code(), code);
        }
        assert_eq!(TextType::from_code(3), TextType::CliCommand);
    }
}
