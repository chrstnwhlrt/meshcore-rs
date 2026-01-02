//! Message types for received and sent messages.

/// Text type indicating message format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum TextType {
    /// Plain text message.
    #[default]
    Plain = 0,
    /// Command message (CLI command to device).
    Command = 1,
    /// Signed message with signature.
    Signed = 2,
}

impl TextType {
    /// Parses text type from a byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Command,
            2 => Self::Signed,
            _ => Self::Plain,
        }
    }
}

/// Signal quality information for v3 messages.
///
/// Note: v3 message format only includes SNR, not RSSI.
/// The 2 bytes after SNR are reserved (always 0x00).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalQuality {
    /// Signal-to-noise ratio in dB (raw value divided by 4).
    pub snr: f32,
}

/// A received message from a contact (private message).
#[derive(Debug, Clone)]
pub struct ContactMessage {
    /// 6-byte public key prefix of the sender.
    pub sender_prefix: [u8; 6],
    /// Path length.
    pub path_len: i8,
    /// Text type.
    pub text_type: TextType,
    /// Sender's timestamp (Unix seconds).
    pub timestamp: u32,
    /// Message signature (if `text_type` is `Signed`).
    pub signature: Option<Vec<u8>>,
    /// Message text.
    pub text: String,
    /// Signal quality (only in v3 format).
    pub signal: Option<SignalQuality>,
}

/// A received message from a channel.
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    /// Channel index.
    pub channel_index: u8,
    /// Path length.
    pub path_len: i8,
    /// Text type.
    pub text_type: TextType,
    /// Sender's timestamp (Unix seconds).
    pub timestamp: u32,
    /// Message text.
    pub text: String,
    /// Signal quality (only in v3 format).
    pub signal: Option<SignalQuality>,
}
/// Acknowledgment received for a sent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acknowledgment {
    /// ACK code matching the expected ACK from the message send response.
    pub code: u32,
}
