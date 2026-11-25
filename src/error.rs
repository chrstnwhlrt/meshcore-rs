//! Error types.

use std::fmt;

/// An error code the device answers a command with (`RESP_CODE_ERR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The command is not supported (`ERR_CODE_UNSUPPORTED_CMD`, 1).
    UnsupportedCmd,
    /// The contact, channel or entry was not found (`ERR_CODE_NOT_FOUND`, 2).
    NotFound,
    /// A table is full or the packet could not be queued (`ERR_CODE_TABLE_FULL`, 3).
    TableFull,
    /// The device is in the wrong state for the command (`ERR_CODE_BAD_STATE`, 4).
    BadState,
    /// Reading or writing the file system failed (`ERR_CODE_FILE_IO_ERROR`, 5).
    FileIoError,
    /// An argument was out of range (`ERR_CODE_ILLEGAL_ARG`, 6).
    IllegalArg,
    /// A code this library does not know.
    Unknown(u8),
}

impl ErrorCode {
    /// Maps the code byte of an error frame.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::UnsupportedCmd,
            2 => Self::NotFound,
            3 => Self::TableFull,
            4 => Self::BadState,
            5 => Self::FileIoError,
            6 => Self::IllegalArg,
            other => Self::Unknown(other),
        }
    }

    /// The code byte as the device sends it.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::UnsupportedCmd => 1,
            Self::NotFound => 2,
            Self::TableFull => 3,
            Self::BadState => 4,
            Self::FileIoError => 5,
            Self::IllegalArg => 6,
            Self::Unknown(code) => code,
        }
    }

    /// The firmware's name for the code, as meshcore_py reports it in
    /// `code_string`, or `None` for an unknown code.
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        match self {
            Self::UnsupportedCmd => Some("ERR_CODE_UNSUPPORTED_CMD"),
            Self::NotFound => Some("ERR_CODE_NOT_FOUND"),
            Self::TableFull => Some("ERR_CODE_TABLE_FULL"),
            Self::BadState => Some("ERR_CODE_BAD_STATE"),
            Self::FileIoError => Some("ERR_CODE_FILE_IO_ERROR"),
            Self::IllegalArg => Some("ERR_CODE_ILLEGAL_ARG"),
            Self::Unknown(_) => None,
        }
    }

    /// The firmware's name for the code, or `"ERR_CODE_UNKNOWN"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self.name() {
            Some(name) => name,
            None => "ERR_CODE_UNKNOWN",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.code(), self.as_str())
    }
}

/// The error type of this crate.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An I/O error on the transport.
    Io(std::io::Error),
    /// The serial port could not be opened or configured.
    Serial(tokio_serial::Error),
    /// A Bluetooth error from BlueZ (feature `ble`, Linux).
    #[cfg(all(feature = "ble", target_os = "linux"))]
    Ble(bluer::Error),
    /// There is no device to connect to: no matching MeshCore device is in BLE
    /// range, the device lacks the UART service, or the adapter is off.
    DeviceNotFound(String),
    /// Another program on this computer has a BLE connection to the device and
    /// gets its frames; using that connection too would mix both programs'
    /// frames.
    DeviceInUse(String),
    /// BLE pairing failed: the device did not take the PIN, or it needs one
    /// that was not given (it is not paired, or it forgot the pairing).
    PairingFailed(String),
    /// There is no connection to the device.
    NotConnected,
    /// The connection was lost before the reply came (also when the transport
    /// took it for dead before sending).
    ConnectionLost,
    /// No reply came in time.
    Timeout,
    /// The device answered with an error code.
    Device(ErrorCode),
    /// The device has the requested feature disabled (private key export or
    /// import).
    Disabled,
    /// The remote node rejected the login.
    LoginFailed,
    /// A reply could not be read or was not the one expected.
    Protocol(String),
    /// An argument was rejected before anything was sent (data too long to
    /// sign only after [`MeshCore::sign_start`](crate::MeshCore::sign_start),
    /// whose answer gives the limit).
    InvalidArgument(String),
}

impl Error {
    /// The device error code, if the device answered with one.
    #[must_use]
    pub const fn device_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Device(code) => Some(*code),
            _ => None,
        }
    }

    /// True for [`Error::Timeout`].
    #[must_use]
    pub const fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Serial(err) => write!(f, "serial port error: {err}"),
            #[cfg(all(feature = "ble", target_os = "linux"))]
            Self::Ble(err) => write!(f, "Bluetooth error: {err}"),
            Self::DeviceNotFound(message) => write!(f, "device not found: {message}"),
            Self::DeviceInUse(message) => write!(f, "device in use: {message}"),
            Self::PairingFailed(message) => write!(f, "pairing failed: {message}"),
            Self::NotConnected => f.write_str("not connected"),
            Self::ConnectionLost => f.write_str("connection lost"),
            Self::Timeout => f.write_str("timed out waiting for a reply"),
            Self::Device(code) => write!(f, "device error {code}"),
            Self::Disabled => f.write_str("feature disabled on the device"),
            Self::LoginFailed => f.write_str("login failed"),
            Self::Protocol(message) => write!(f, "protocol error: {message}"),
            Self::InvalidArgument(message) => write!(f, "invalid argument: {message}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Serial(err) => Some(err),
            #[cfg(all(feature = "ble", target_os = "linux"))]
            Self::Ble(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<tokio_serial::Error> for Error {
    fn from(err: tokio_serial::Error) -> Self {
        Self::Serial(err)
    }
}

#[cfg(all(feature = "ble", target_os = "linux"))]
impl From<bluer::Error> for Error {
    fn from(err: bluer::Error) -> Self {
        Self::Ble(err)
    }
}

/// Result alias of this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_round_trip() {
        for code in 0..=255u8 {
            assert_eq!(ErrorCode::from_code(code).code(), code);
        }
        assert_eq!(ErrorCode::from_code(6), ErrorCode::IllegalArg);
        assert_eq!(ErrorCode::IllegalArg.as_str(), "ERR_CODE_ILLEGAL_ARG");
        assert_eq!(ErrorCode::from_code(42).name(), None);
    }

    #[test]
    fn device_error_display_shows_code_and_name() {
        let err = Error::Device(ErrorCode::IllegalArg);
        assert_eq!(err.to_string(), "device error 6 (ERR_CODE_ILLEGAL_ARG)");
        assert_eq!(err.device_code(), Some(ErrorCode::IllegalArg));
    }

    #[test]
    fn connection_errors_say_what_went_wrong() {
        let in_use = Error::DeviceInUse("another program is connected".into());
        assert_eq!(
            in_use.to_string(),
            "device in use: another program is connected"
        );
        let pairing = Error::PairingFailed("it did not take the PIN".into());
        assert_eq!(
            pairing.to_string(),
            "pairing failed: it did not take the PIN"
        );
    }
}
