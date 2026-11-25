//! Device statistics and remote status.

/// Core statistics (`RESP_CODE_STATS`, type 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreStats {
    /// Battery voltage in millivolts.
    pub battery_mv: u16,
    /// Uptime in seconds.
    pub uptime_secs: u32,
    /// Error flags.
    pub errors: u16,
    /// Packets waiting to be sent.
    pub queue_len: u8,
}

/// Radio statistics (`RESP_CODE_STATS`, type 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadioStats {
    /// Noise floor in dBm.
    pub noise_floor: i16,
    /// RSSI of the last packet in dBm.
    pub last_rssi: i8,
    /// SNR of the last packet in dB.
    pub last_snr: f32,
    /// Total transmit airtime in seconds.
    pub tx_air_secs: u32,
    /// Total receive airtime in seconds.
    pub rx_air_secs: u32,
}

/// Packet statistics (`RESP_CODE_STATS`, type 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketStats {
    /// Packets received.
    pub recv: u32,
    /// Packets sent.
    pub sent: u32,
    /// Flood packets sent.
    pub flood_tx: u32,
    /// Direct packets sent.
    pub direct_tx: u32,
    /// Flood packets received.
    pub flood_rx: u32,
    /// Direct packets received.
    pub direct_rx: u32,
    /// Packets received with errors (firmware 1.13 and later).
    pub recv_errors: Option<u32>,
}

/// The status of a repeater or room server (`PUSH_CODE_STATUS_RESPONSE`, or a
/// binary status request).
#[derive(Debug, Clone, PartialEq)]
pub struct StatusResponse {
    /// The node's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The request tag, for answers to binary requests.
    pub tag: Option<u32>,
    /// Battery voltage in millivolts.
    pub bat: u16,
    /// Packets waiting to be sent.
    pub tx_queue_len: u16,
    /// Noise floor in dBm.
    pub noise_floor: i16,
    /// RSSI of the last packet in dBm.
    pub last_rssi: i16,
    /// Packets received.
    pub nb_recv: u32,
    /// Packets sent.
    pub nb_sent: u32,
    /// Total transmit airtime in seconds.
    pub airtime: u32,
    /// Uptime in seconds.
    pub uptime: u32,
    /// Flood packets sent.
    pub sent_flood: u32,
    /// Direct packets sent.
    pub sent_direct: u32,
    /// Flood packets received.
    pub recv_flood: u32,
    /// Direct packets received.
    pub recv_direct: u32,
    /// Error flags.
    pub full_evts: u16,
    /// SNR of the last packet in dB.
    pub last_snr: f32,
    /// Direct duplicates dropped.
    pub direct_dups: u16,
    /// Flood duplicates dropped.
    pub flood_dups: u16,
    /// Total receive airtime in seconds. A room server sends its counts of
    /// posts (low 16 bits) and of posts pushed to clients (high 16 bits) here.
    pub rx_airtime: u32,
    /// Packets received with errors (repeater firmware 1.12 and later; the
    /// zeros that fill up the answer of other nodes read as 0).
    pub recv_errors: Option<u32>,
}
