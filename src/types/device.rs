//! Device information types.

use crate::types::contact::PublicKey;

/// Telemetry mode configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TelemetryMode {
    /// Environment telemetry mode (upper 2 bits).
    pub env: u8,
    /// Location telemetry mode (middle 2 bits).
    pub loc: u8,
    /// Base telemetry mode (lower 2 bits).
    pub base: u8,
}

impl TelemetryMode {
    /// Parses telemetry mode from a byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self {
            env: (byte >> 4) & 0x03,
            loc: (byte >> 2) & 0x03,
            base: byte & 0x03,
        }
    }

    /// Encodes telemetry mode to a byte.
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        ((self.env & 0x03) << 4) | ((self.loc & 0x03) << 2) | (self.base & 0x03)
    }
}

/// Radio configuration parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioConfig {
    /// Frequency in MHz.
    pub frequency_mhz: f64,
    /// Bandwidth in kHz.
    pub bandwidth_khz: f64,
    /// Spreading factor (6-12).
    pub spreading_factor: u8,
    /// Coding rate (5-8, representing 4/5 to 4/8).
    pub coding_rate: u8,
}

impl Default for RadioConfig {
    fn default() -> Self {
        Self {
            frequency_mhz: 868.0,
            bandwidth_khz: 125.0,
            spreading_factor: 7,
            coding_rate: 5,
        }
    }
}

/// Self device information returned after `AppStart`.
#[derive(Debug, Clone)]
pub struct SelfInfo {
    /// Advertisement type.
    pub advert_type: u8,
    /// Current TX power (dBm).
    pub tx_power: u8,
    /// Maximum TX power (dBm).
    pub max_tx_power: u8,
    /// Device public key.
    pub public_key: PublicKey,
    /// Device latitude.
    pub latitude: Option<f64>,
    /// Device longitude.
    pub longitude: Option<f64>,
    /// Multi-ack setting.
    pub multi_acks: u8,
    /// Advertisement location policy.
    pub advert_loc_policy: u8,
    /// Telemetry mode configuration.
    pub telemetry_mode: TelemetryMode,
    /// Manual contact addition setting.
    pub manual_add_contacts: bool,
    /// Radio configuration.
    pub radio: RadioConfig,
    /// Device name.
    pub name: String,
}

/// Device information returned by `DeviceQuery`.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Firmware version.
    pub firmware_version: u8,
    /// Maximum contacts (if firmware >= 3).
    pub max_contacts: Option<u16>,
    /// Maximum channels (if firmware >= 3).
    pub max_channels: Option<u8>,
    /// BLE PIN (if firmware >= 3).
    pub ble_pin: Option<u32>,
    /// Build identifier (if firmware >= 3).
    pub build: Option<String>,
    /// Device model (if firmware >= 3).
    pub model: Option<String>,
    /// Version string (if firmware >= 3).
    pub version: Option<String>,
}

/// Battery status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Battery voltage in millivolts.
    pub millivolts: u16,
    /// Used storage in KB, if available.
    pub used_kb: Option<u32>,
    /// Total storage in KB, if available.
    pub total_kb: Option<u32>,
}

/// Channel configuration.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Channel index (0-based).
    pub index: u8,
    /// Channel name (up to 32 bytes).
    pub name: String,
    /// Channel secret (16 bytes).
    pub secret: [u8; 16],
}
