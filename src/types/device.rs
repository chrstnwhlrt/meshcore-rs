//! The companion device's own information and settings.

use std::fmt;

use crate::protocol::path::PathLen;
use crate::types::contact::PublicKey;

/// The device's own information (`RESP_CODE_SELF_INFO`, the reply to `APP_START`).
#[derive(Debug, Clone, PartialEq)]
pub struct SelfInfo {
    /// The advert type the node identifies as (`ADV_TYPE_CHAT` for a companion).
    pub adv_type: u8,
    /// The current TX power in dBm.
    pub tx_power: i8,
    /// The highest TX power the radio allows, in dBm.
    pub max_tx_power: u8,
    /// The device's public key.
    pub public_key: PublicKey,
    /// Latitude in millionths of a degree.
    pub latitude_e6: i32,
    /// Longitude in millionths of a degree.
    pub longitude_e6: i32,
    /// Extra ACK transmissions.
    pub multi_acks: u8,
    /// Whether adverts carry the location.
    pub adv_loc_policy: u8,
    /// Telemetry mode for base data (0 deny, 1 by contact flags, 2 allow all).
    pub telemetry_mode_base: u8,
    /// Telemetry mode for the location.
    pub telemetry_mode_loc: u8,
    /// Telemetry mode for environment sensors.
    pub telemetry_mode_env: u8,
    /// Whether new contacts are added only manually.
    pub manual_add_contacts: bool,
    /// Radio frequency in MHz.
    pub radio_freq: f64,
    /// Radio bandwidth in kHz.
    pub radio_bw: f64,
    /// Spreading factor.
    pub radio_sf: u8,
    /// Coding rate (5-8 for 4/5 to 4/8).
    pub radio_cr: u8,
    /// The node's name.
    pub name: String,
}

impl SelfInfo {
    /// Latitude in degrees.
    #[must_use]
    pub fn latitude(&self) -> f64 {
        f64::from(self.latitude_e6) / 1e6
    }

    /// Longitude in degrees.
    #[must_use]
    pub fn longitude(&self) -> f64 {
        f64::from(self.longitude_e6) / 1e6
    }

    /// The settings `CMD_SET_OTHER_PARAMS` writes, as they are now.
    #[must_use]
    pub const fn other_params(&self) -> OtherParams {
        OtherParams {
            manual_add_contacts: self.manual_add_contacts,
            telemetry_mode_base: self.telemetry_mode_base,
            telemetry_mode_loc: self.telemetry_mode_loc,
            telemetry_mode_env: self.telemetry_mode_env,
            adv_loc_policy: self.adv_loc_policy,
            multi_acks: self.multi_acks,
        }
    }

    /// Takes the settings as the device took them: two bits per telemetry
    /// mode, as [`OtherParams::telemetry_mode_byte`] sends them.
    pub(crate) const fn apply_other_params(&mut self, params: OtherParams) {
        self.manual_add_contacts = params.manual_add_contacts;
        self.telemetry_mode_base = params.telemetry_mode_base & 0b11;
        self.telemetry_mode_loc = params.telemetry_mode_loc & 0b11;
        self.telemetry_mode_env = params.telemetry_mode_env & 0b11;
        self.adv_loc_policy = params.adv_loc_policy;
        self.multi_acks = params.multi_acks;
    }
}

/// The settings `CMD_SET_OTHER_PARAMS` writes in one go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OtherParams {
    /// Add new contacts only manually.
    pub manual_add_contacts: bool,
    /// Telemetry mode for base data (2 bits).
    pub telemetry_mode_base: u8,
    /// Telemetry mode for the location (2 bits).
    pub telemetry_mode_loc: u8,
    /// Telemetry mode for environment sensors (2 bits).
    pub telemetry_mode_env: u8,
    /// Whether adverts carry the location.
    pub adv_loc_policy: u8,
    /// Extra ACK transmissions.
    pub multi_acks: u8,
}

impl OtherParams {
    /// The telemetry modes packed into one byte as the firmware takes them.
    #[must_use]
    pub const fn telemetry_mode_byte(&self) -> u8 {
        (self.telemetry_mode_base & 0b11)
            | ((self.telemetry_mode_loc & 0b11) << 2)
            | ((self.telemetry_mode_env & 0b11) << 4)
    }
}

/// Firmware information (`RESP_CODE_DEVICE_INFO`, the reply to `DEVICE_QUERY`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// The firmware's protocol version code (`FIRMWARE_VER_CODE`).
    pub fw_ver: u8,
    /// The most contacts the device stores (code 3 and later).
    pub max_contacts: Option<u16>,
    /// The most channels the device stores (code 3 and later).
    pub max_channels: Option<u8>,
    /// The BLE PIN (code 3 and later).
    pub ble_pin: Option<u32>,
    /// The firmware build date (code 3 and later).
    pub fw_build: Option<String>,
    /// The board model (code 3 and later).
    pub model: Option<String>,
    /// The firmware version string (code 3 and later).
    pub ver: Option<String>,
    /// Whether the device repeats packets (code 9 and later).
    pub repeat: Option<bool>,
    /// The path hash mode the device uses (code 10 and later).
    pub path_hash_mode: Option<u8>,
}

/// Battery and storage (`RESP_CODE_BATT_AND_STORAGE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Battery voltage in millivolts (meshcore_py: `level`).
    pub level: u16,
    /// Used storage in KiB.
    pub used_kb: Option<u32>,
    /// Total storage in KiB.
    pub total_kb: Option<u32>,
}

/// A channel (`RESP_CODE_CHANNEL_INFO`).
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    /// The channel's index on the device.
    pub channel_idx: u8,
    /// The channel's name.
    pub channel_name: String,
    /// The channel's 128-bit secret.
    pub channel_secret: [u8; 16],
}

impl ChannelInfo {
    /// The channel hash group packets carry: the first byte of the secret's SHA-256.
    #[must_use]
    pub fn channel_hash(&self) -> u8 {
        use sha2::Digest;
        sha2::Sha256::digest(self.channel_secret)[0]
    }

    /// True when the channel slot is unused (no name, all-zero secret).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.channel_name.is_empty() && self.channel_secret == [0; 16]
    }
}

impl fmt::Debug for ChannelInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelInfo")
            .field("channel_idx", &self.channel_idx)
            .field("channel_name", &self.channel_name)
            .field("channel_secret", &"<redacted>")
            .finish()
    }
}

/// The secret of a channel named `#name`: the first 16 bytes of the name's SHA-256.
#[must_use]
pub fn channel_secret_from_name(name: &str) -> [u8; 16] {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(name.as_bytes());
    let mut secret = [0u8; 16];
    secret.copy_from_slice(&digest[..16]);
    secret
}

/// Radio tuning (`RESP_CODE_TUNING_PARAMS`): both values are scaled by 1000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuningParams {
    /// The RX delay base, times 1000.
    pub rx_delay: u32,
    /// The airtime factor, times 1000.
    pub airtime_factor: u32,
}

/// Which adverts the device adds as contacts by itself (`RESP_CODE_AUTOADD_CONFIG`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoAddConfig {
    /// The flags, see [`autoadd_flags`].
    pub config: u8,
    /// The hop limit, when sent: adverts that came over fewer hops are added
    /// (1: direct ones only, 0: no limit).
    pub max_hops: Option<u8>,
}

/// Bits of [`AutoAddConfig::config`].
pub mod autoadd_flags {
    /// Overwrite the oldest non-favourite contact when the table is full.
    pub const OVERWRITE_OLDEST: u8 = 0x01;
    /// Add chat nodes in manual mode.
    pub const CHAT: u8 = 0x02;
    /// Add repeaters in manual mode.
    pub const REPEATER: u8 = 0x04;
    /// Add room servers in manual mode.
    pub const ROOM_SERVER: u8 = 0x08;
    /// Add sensors in manual mode.
    pub const SENSOR: u8 = 0x10;
}

/// A frequency range the device may repeat on (`RESP_ALLOWED_REPEAT_FREQ`), in kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreqRange {
    /// The lower bound in kHz.
    pub min: u32,
    /// The upper bound in kHz.
    pub max: u32,
}

/// The default flood scope (`RESP_CODE_DEFAULT_FLOOD_SCOPE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultFloodScope {
    /// The scope's name, e.g. `#hamburg`.
    pub name: String,
    /// The scope's transport key.
    pub key: [u8; 16],
}

/// The path an advert arrived over (`RESP_CODE_ADVERT_PATH`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertPath {
    /// When the advert was received, by the device's clock.
    pub timestamp: u32,
    /// The path length byte.
    pub path_len: PathLen,
    /// The path.
    pub path: Vec<u8>,
}

/// The device's private key as `CMD_EXPORT_PRIVATE_KEY` returns it (64 bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateKey([u8; 64]);

impl PrivateKey {
    /// Wraps the 64 key bytes.
    #[must_use]
    pub const fn new(bytes: [u8; 64]) -> Self {
        Self(bytes)
    }

    /// The key bytes, as `CMD_IMPORT_PRIVATE_KEY` takes them.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }

    /// The key as hex.
    #[must_use]
    pub fn to_hex(&self) -> String {
        crate::hex::encode(self.0)
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateKey(<redacted>)")
    }
}

/// Custom variables (`RESP_CODE_CUSTOM_VARS`) in the order the device lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomVars(Vec<(String, String)>);

impl CustomVars {
    /// Parses `name:value,name:value`. A pair without `:` is skipped; a value
    /// may itself contain `:`. A repeated name keeps its first position and
    /// takes the later value, as a Python dict does.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut vars = Self::default();
        if text.is_empty() {
            return vars;
        }
        for pair in text.split(',') {
            if let Some((name, value)) = pair.split_once(':') {
                vars.insert(name.to_string(), value.to_string());
            }
        }
        vars
    }

    /// The value of a variable.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Sets a variable, keeping its position if it exists.
    pub fn insert(&mut self, name: String, value: String) {
        if let Some(entry) = self.0.iter_mut().find(|(key, _)| *key == name) {
            entry.1 = value;
        } else {
            self.0.push((name, value));
        }
    }

    /// The variables in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// The number of variables.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when there are no variables.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_vars_keep_order_and_skip_bad_pairs() {
        let vars = CustomVars::parse("gps:1,bad,url:http://x,gps:0,");
        let pairs: Vec<_> = vars.iter().collect();
        assert_eq!(pairs, vec![("gps", "0"), ("url", "http://x")]);
        assert_eq!(vars.get("url"), Some("http://x"));
        assert!(CustomVars::parse("").is_empty());
    }

    #[test]
    fn hashtag_channel_secret() {
        // sha256("#test")[..16], as meshcore_py computes it
        let secret = channel_secret_from_name("#test");
        assert_eq!(
            crate::hex::encode(secret),
            "9cd8fcf22a47333b591d96a2b848b73f"
        );
    }

    #[test]
    fn telemetry_mode_byte() {
        let params = OtherParams {
            telemetry_mode_base: 2,
            telemetry_mode_loc: 1,
            telemetry_mode_env: 3,
            ..OtherParams::default()
        };
        assert_eq!(params.telemetry_mode_byte(), 0b11_01_10);
    }

    #[test]
    fn secrets_stay_out_of_debug_output() {
        let key = PrivateKey::new([0xaa; 64]);
        assert!(!format!("{key:?}").contains("aa"));
        let channel = ChannelInfo {
            channel_idx: 1,
            channel_name: "x".into(),
            channel_secret: [0xbb; 16],
        };
        assert!(!format!("{channel:?}").contains("187"));
        assert!(!format!("{channel:?}").contains("bb"));
    }
}
