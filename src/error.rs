//! Error types for the meshcore library.

use thiserror::Error;

/// The main error type for meshcore operations.
#[derive(Debug, Error)]
pub enum Error {
    /// Serial port error.
    #[error("serial port error: {0}")]
    Serial(#[from] tokio_serial::Error),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Frame encoding/decoding error.
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),

    /// Protocol error from the device.
    #[error("protocol error: {message}")]
    Protocol { message: String },

    /// Command timed out waiting for response.
    #[error("command timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    /// Connection is not established.
    #[error("not connected")]
    NotConnected,

    /// Invalid public key format.
    #[error("invalid public key: {reason}")]
    InvalidPublicKey { reason: String },

    /// Invalid coordinates.
    #[error("invalid coordinates: {reason}")]
    InvalidCoordinates { reason: String },

    /// Channel send error.
    #[error("channel send error")]
    ChannelSend,

    /// Channel receive error.
    #[error("channel closed")]
    ChannelClosed,
}

/// Frame-specific errors.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Frame too short to contain header and length.
    #[error("frame too short: need at least 3 bytes, got {0}")]
    TooShort(usize),

    /// Frame payload exceeds maximum size.
    #[error("frame too large: {size} bytes exceeds maximum {max}")]
    TooLarge { size: usize, max: usize },

    /// Incomplete frame data.
    #[error("incomplete frame: expected {expected} bytes, got {got}")]
    Incomplete { expected: usize, got: usize },
}

/// Result type alias for meshcore operations.
pub type Result<T> = std::result::Result<T, Error>;
