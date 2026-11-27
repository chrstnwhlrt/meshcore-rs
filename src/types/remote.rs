//! Answers from remote nodes and other pushes.

use crate::lpp::{LppReading, LppType, LppValue, Telemetry};
use crate::protocol::path::PathLen;

/// A successful login to a repeater, room server or sensor (`PUSH_CODE_LOGIN_SUCCESS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginSuccess {
    /// The permissions byte (bit 0: admin; a room server sends 2 for read-only).
    pub permissions: Option<u8>,
    /// The server's 6-byte key prefix.
    pub pubkey_prefix: Option<[u8; 6]>,
    /// The server's clock (firmware 1.7 and later).
    pub server_timestamp: Option<u32>,
    /// The ACL permissions (firmware 1.7.3 and later).
    pub acl_permissions: Option<u8>,
    /// The server's firmware level (firmware 1.9 and later).
    pub fw_ver_level: Option<u8>,
}

impl LoginSuccess {
    /// True when the admin bit is set.
    #[must_use]
    pub const fn is_admin(&self) -> bool {
        match self.permissions {
            Some(perms) => perms & 1 == 1,
            None => false,
        }
    }
}

/// A rejected login (`PUSH_CODE_LOGIN_FAIL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginFailed {
    /// The server's 6-byte key prefix.
    pub pubkey_prefix: Option<[u8; 6]>,
}

/// Telemetry of a node (`PUSH_CODE_TELEMETRY_RESPONSE`, or a binary request).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryResponse {
    /// The node's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The request tag, for answers to binary requests.
    pub tag: Option<u32>,
    /// The readings.
    pub telemetry: Telemetry,
}

/// A raw answer to a binary or anonymous request (`PUSH_CODE_BINARY_RESPONSE`).
///
/// From firmware 1.18 on, every answer the device cannot match comes this way
/// too: a sensor's telemetry pushes, with the subscription's push tag, and an
/// answer to a request the next has replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryResponse {
    /// The tag: the `expected_ack` of the request's [`SentInfo`](crate::SentInfo).
    pub tag: u32,
    /// The answer after the tag, with the zeros that fill the cipher's last
    /// 16-byte block.
    pub data: Vec<u8>,
}

/// One series of a min/max/average answer.
#[derive(Debug, Clone, PartialEq)]
pub struct MmaEntry {
    /// The LPP channel.
    pub channel: u8,
    /// The LPP type.
    pub lpp_type: LppType,
    /// The minimum.
    pub min: LppValue,
    /// The maximum.
    pub max: LppValue,
    /// The average.
    pub avg: LppValue,
}

/// The answer to a min/max/average request (`BinaryReqType::Mma`).
#[derive(Debug, Clone, PartialEq)]
pub struct MmaResponse {
    /// The node's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The request tag.
    pub tag: u32,
    /// The sensor's clock when it answered.
    pub timestamp: Option<u32>,
    /// The series.
    pub entries: Vec<MmaEntry>,
    /// Bytes from the first series that could not be decoded.
    pub undecoded: Vec<u8>,
}

/// One entry of an access control list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclEntry {
    /// The client's 6-byte key prefix.
    pub key: [u8; 6],
    /// The client's permissions.
    pub perm: u8,
}

/// The answer to an access control list request (`BinaryReqType::Acl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AclResponse {
    /// The node's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The request tag.
    pub tag: u32,
    /// The entries.
    pub entries: Vec<AclEntry>,
}

/// What to ask a repeater for in a neighbours request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighboursQuery {
    /// How many neighbours to return (meshcore_py default 255).
    pub count: u8,
    /// How many to skip.
    pub offset: u16,
    /// Sort order: 0 newest first, 1 oldest first, 2 strongest first, 3 weakest first.
    pub order_by: u8,
    /// Key prefix length per neighbour (meshcore_py default 4).
    pub pubkey_prefix_length: u8,
}

impl Default for NeighboursQuery {
    fn default() -> Self {
        Self {
            count: 255,
            offset: 0,
            order_by: 0,
            pubkey_prefix_length: 4,
        }
    }
}

/// A neighbour of a repeater.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    /// The neighbour's key prefix.
    pub pubkey: Vec<u8>,
    /// Seconds since the neighbour was last heard.
    pub secs_ago: i32,
    /// SNR in dB.
    pub snr: f32,
}

/// The answer to a neighbours request (`BinaryReqType::Neighbours`).
#[derive(Debug, Clone, PartialEq)]
pub struct NeighboursResponse {
    /// The repeater's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The request tag (none for [`MeshCore::fetch_all_neighbours`](crate::MeshCore::fetch_all_neighbours)).
    pub tag: Option<u32>,
    /// The key prefix length asked for.
    pub pubkey_prefix_length: u8,
    /// How many neighbours the repeater has.
    pub neighbours_count: i16,
    /// How many are in this answer.
    pub results_count: i16,
    /// The neighbours.
    pub neighbours: Vec<Neighbour>,
}

/// A repeater's firmware version, name and owner (`BinaryReqType::OwnerInfo`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerInfo {
    /// The firmware version.
    pub firmware_version: String,
    /// The node's name.
    pub name: String,
    /// The owner info (may hold newlines).
    pub owner: String,
}

/// A sensor's answer to a subscription (`BinaryReqType::Subscribe`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorSubscription {
    /// How long the subscription lasts in seconds (0: refused).
    pub expiry_secs: u16,
    /// The region the pushes will be scoped to.
    pub scope: String,
}

/// A repeater's name and owner info (anonymous `Owner` request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonOwner {
    /// The repeater's name.
    pub name: String,
    /// The owner info.
    pub owner: String,
}

/// The result of a path discovery (`PUSH_CODE_PATH_DISCOVERY_RESPONSE`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDiscovery {
    /// The node's 6-byte key prefix.
    pub pubkey_prefix: [u8; 6],
    /// The path from here to the node.
    pub out_path_len: PathLen,
    /// The out path.
    pub out_path: Vec<u8>,
    /// The path from the node to here.
    pub in_path_len: PathLen,
    /// The in path.
    pub in_path: Vec<u8>,
}

/// One hop of a trace.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceHop {
    /// The hop's hash (1, 2, 4 or 8 bytes).
    pub hash: Vec<u8>,
    /// The SNR in dB the hop received the trace with.
    pub snr: f32,
}

/// A trace result (`PUSH_CODE_TRACE_DATA`).
#[derive(Debug, Clone, PartialEq)]
pub struct TraceData {
    /// The trace's tag.
    pub tag: u32,
    /// The trace's auth code.
    pub auth_code: u32,
    /// The trace's flags (low 2 bits: hash size `1 << n`).
    pub flags: u8,
    /// The hops; a hash cut short at the end of a trace another node built
    /// has no SNR and is left out.
    pub hops: Vec<TraceHop>,
    /// The SNR in dB this node received the trace with.
    pub final_snr: Option<f32>,
}

/// Control data (`PUSH_CODE_CONTROL_DATA`).
#[derive(Debug, Clone, PartialEq)]
pub struct ControlData {
    /// SNR in dB.
    pub snr: f32,
    /// RSSI in dBm.
    pub rssi: i8,
    /// The packet's path length byte.
    pub path_len: u8,
    /// The control type (first payload byte).
    pub payload_type: u8,
    /// The payload, including the type byte.
    pub payload: Vec<u8>,
}

/// An answer to a node discovery request.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoverResponse {
    /// SNR in dB of the answer.
    pub snr: f32,
    /// RSSI in dBm of the answer.
    pub rssi: i8,
    /// The packet's path length byte.
    pub path_len: u8,
    /// The node type (low nibble of the control type).
    pub node_type: u8,
    /// The SNR in dB the node received the request with.
    pub snr_in: f32,
    /// The request's tag.
    pub tag: u32,
    /// The node's key: 8 bytes, or 32 when the request asked for full keys.
    pub pubkey: Vec<u8>,
}

/// Raw data (`PUSH_CODE_RAW_DATA`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawData {
    /// SNR in dB.
    pub snr: f32,
    /// RSSI in dBm.
    pub rssi: i8,
    /// The payload.
    pub payload: Vec<u8>,
}

/// Convenience accessors for telemetry readings.
impl TelemetryResponse {
    /// The readings.
    #[must_use]
    pub fn readings(&self) -> &[LppReading] {
        &self.telemetry.readings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_answers_hand_out_their_readings() {
        let answer = TelemetryResponse {
            pubkey_prefix: [1; 6],
            tag: None,
            telemetry: Telemetry::parse(&[1, 116, 0x01, 0x9c]),
        };
        assert_eq!(answer.readings().len(), 1);
        assert_eq!(answer.readings()[0].lpp_type, LppType::Voltage);
    }
}
