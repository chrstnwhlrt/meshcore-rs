//! Statistics types for device monitoring.

/// Statistics type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StatsType {
    /// Core device statistics.
    Core = 0,
    /// Radio statistics.
    Radio = 1,
    /// Packet statistics.
    Packets = 2,
}

impl StatsType {
    /// Parses stats type from a byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Core),
            1 => Some(Self::Radio),
            2 => Some(Self::Packets),
            _ => None,
        }
    }
}

/// Core device statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStats {
    /// Battery voltage in millivolts.
    pub battery_mv: u16,
    /// Uptime in seconds.
    pub uptime_secs: u32,
    /// Error count.
    pub errors: u16,
    /// TX queue length.
    pub queue_len: u8,
}

/// Radio statistics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioStats {
    /// Noise floor in dBm.
    pub noise_floor: i16,
    /// Last RSSI in dBm.
    pub rssi: i8,
    /// Last SNR in dB (raw value divided by 4).
    pub snr: f32,
    /// Total TX airtime in seconds.
    pub tx_airtime_secs: u32,
    /// Total RX airtime in seconds.
    pub rx_airtime_secs: u32,
}

/// Packet statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketStats {
    /// Total packets received.
    pub received: u32,
    /// Total packets sent.
    pub sent: u32,
    /// Flood packets transmitted.
    pub flood_tx: u32,
    /// Direct packets transmitted.
    pub direct_tx: u32,
    /// Flood packets received.
    pub flood_rx: u32,
    /// Direct packets received.
    pub direct_rx: u32,
}

/// Full device status (from binary status request or status response).
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
    /// 6-byte public key prefix.
    pub pubkey_prefix: [u8; 6],
    /// Battery voltage in millivolts.
    pub battery_mv: u16,
    /// TX queue length.
    pub tx_queue_len: u16,
    /// Noise floor in dBm.
    pub noise_floor: i16,
    /// Last RSSI in dBm.
    pub last_rssi: i16,
    /// Number of packets received.
    pub packets_received: u32,
    /// Number of packets sent.
    pub packets_sent: u32,
    /// Total airtime in seconds.
    pub airtime_secs: u32,
    /// Uptime in seconds.
    pub uptime_secs: u32,
    /// Flood packets sent.
    pub sent_flood: u32,
    /// Direct packets sent.
    pub sent_direct: u32,
    /// Flood packets received.
    pub recv_flood: u32,
    /// Direct packets received.
    pub recv_direct: u32,
    /// Full events count.
    pub full_events: u16,
    /// Last SNR in dB.
    pub last_snr: f32,
    /// Direct duplicate count.
    pub direct_dups: u16,
    /// Flood duplicate count.
    pub flood_dups: u16,
    /// RX airtime in seconds.
    pub rx_airtime_secs: u32,
}
