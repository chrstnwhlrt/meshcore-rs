//! `serde::Serialize` with meshcore_py's payload layout (feature `serde`).
//!
//! Field names, their order and value formats follow the dicts meshcore_py
//! puts into its events, so JSON written from these types reads like the
//! Python library's: keys, paths and tags as hex, `-1` for "no path" (255
//! in messages), readings as `[{"channel", "type", "value"}]`. An [`Event`]
//! serializes as its payload.

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::event::{ErrorInfo, Event};
use crate::hex::encode as hex;
use crate::lpp::{LppReading, LppType, LppValue, Telemetry};
use crate::protocol::codes::PacketType;
use crate::protocol::path::PathLen;
use crate::types::{
    Ack, AclEntry, AclResponse, AdvertPath, AnonOwner, AutoAddConfig, BatteryStatus,
    BinaryResponse, ChannelData, ChannelInfo, ChannelMessage, Contact, ContactList, ContactMessage,
    ControlData, CoreStats, CustomVars, DefaultFloodScope, DeviceInfo, DiscoverResponse, FreqRange,
    LoginFailed, LoginSuccess, MmaEntry, MmaResponse, Neighbour, NeighboursResponse, OwnerInfo,
    PacketContent, PacketInfo, PacketStats, PathDiscovery, PrivateKey, PublicKey, RadioStats,
    RawData, ReceivedMessage, RxLogData, SelfInfo, SensorSubscription, SentInfo, StatusResponse,
    TelemetryResponse, TraceData, TuningParams,
};

/// Tags and ACK codes as meshcore_py prints them: hex of the wire bytes.
fn tag_hex(tag: u32) -> String {
    hex(tag.to_le_bytes())
}

/// The hop count as meshcore_py's message dicts have it: 255 for "direct".
fn message_path_len(len: PathLen) -> i16 {
    len.hops().map_or(255, i16::from)
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl Serialize for Contact {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("public_key", &self.public_key)?;
        m.serialize_entry("type", &self.contact_type.code())?;
        m.serialize_entry("flags", &self.flags)?;
        m.serialize_entry(
            "out_path_hash_mode",
            &self.out_path_len.hash_mode_or_minus_one(),
        )?;
        m.serialize_entry("out_path_len", &self.out_path_len.hops_or_minus_one())?;
        m.serialize_entry("out_path", &hex(&self.out_path))?;
        m.serialize_entry("adv_name", &self.name)?;
        m.serialize_entry("adv_name_raw", &hex(self.name_raw))?;
        m.serialize_entry("last_advert", &self.last_advert)?;
        m.serialize_entry("adv_lat", &self.latitude())?;
        m.serialize_entry("adv_lon", &self.longitude())?;
        m.serialize_entry("lastmod", &self.lastmod)?;
        m.end()
    }
}

/// Contacts keyed by public key, as meshcore_py's `CONTACTS` payload.
impl Serialize for ContactList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.contacts.len()))?;
        for contact in &self.contacts {
            m.serialize_entry(&contact.public_key.to_hex(), contact)?;
        }
        m.end()
    }
}

impl Serialize for SelfInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("adv_type", &self.adv_type)?;
        m.serialize_entry("tx_power", &self.tx_power)?;
        m.serialize_entry("max_tx_power", &self.max_tx_power)?;
        m.serialize_entry("public_key", &self.public_key)?;
        m.serialize_entry("adv_lat", &self.latitude())?;
        m.serialize_entry("adv_lon", &self.longitude())?;
        m.serialize_entry("multi_acks", &self.multi_acks)?;
        m.serialize_entry("adv_loc_policy", &self.adv_loc_policy)?;
        m.serialize_entry("telemetry_mode_env", &self.telemetry_mode_env)?;
        m.serialize_entry("telemetry_mode_loc", &self.telemetry_mode_loc)?;
        m.serialize_entry("telemetry_mode_base", &self.telemetry_mode_base)?;
        m.serialize_entry("manual_add_contacts", &self.manual_add_contacts)?;
        m.serialize_entry("radio_freq", &self.radio_freq)?;
        m.serialize_entry("radio_bw", &self.radio_bw)?;
        m.serialize_entry("radio_sf", &self.radio_sf)?;
        m.serialize_entry("radio_cr", &self.radio_cr)?;
        m.serialize_entry("name", &self.name)?;
        m.end()
    }
}

impl Serialize for DeviceInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("fw ver", &self.fw_ver)?;
        if let Some(v) = self.max_contacts {
            m.serialize_entry("max_contacts", &v)?;
        }
        if let Some(v) = self.max_channels {
            m.serialize_entry("max_channels", &v)?;
        }
        if let Some(v) = self.ble_pin {
            m.serialize_entry("ble_pin", &v)?;
        }
        if let Some(v) = &self.fw_build {
            m.serialize_entry("fw_build", v)?;
        }
        if let Some(v) = &self.model {
            m.serialize_entry("model", v)?;
        }
        if let Some(v) = &self.ver {
            m.serialize_entry("ver", v)?;
        }
        if let Some(v) = self.repeat {
            m.serialize_entry("repeat", &v)?;
        }
        if let Some(v) = self.path_hash_mode {
            m.serialize_entry("path_hash_mode", &v)?;
        }
        m.end()
    }
}

impl Serialize for BatteryStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("level", &self.level)?;
        if let (Some(used), Some(total)) = (self.used_kb, self.total_kb) {
            m.serialize_entry("used_kb", &used)?;
            m.serialize_entry("total_kb", &total)?;
        }
        m.end()
    }
}

impl Serialize for SentInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("type", &u8::from(self.flood))?;
        m.serialize_entry("expected_ack", &tag_hex(self.expected_ack))?;
        m.serialize_entry("suggested_timeout", &self.suggested_timeout)?;
        m.end()
    }
}

impl Serialize for Ack {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("code", &tag_hex(self.code))?;
        if let Some(trip_time) = self.trip_time {
            m.serialize_entry("trip_time", &trip_time)?;
        }
        m.end()
    }
}

impl Serialize for ErrorInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if let Some(code) = self.code {
            m.serialize_entry("error_code", &code.code())?;
            if let Some(name) = code.name() {
                m.serialize_entry("code_string", name)?;
            }
        }
        if let Some(reason) = &self.reason {
            m.serialize_entry("reason", reason)?;
        }
        // meshcore_py reports a MSG_SENT, TUNING_PARAMS or STATS reply it
        // cannot read without the packet type, the others with it.
        let without_type = |code| {
            matches!(
                PacketType::from_code(code),
                Some(PacketType::MsgSent | PacketType::TuningParams | PacketType::Stats)
            )
        };
        if let Some(packet_type) = self.packet_type.filter(|&code| !without_type(code)) {
            m.serialize_entry("packet_type", &packet_type)?;
        }
        m.end()
    }
}

impl Serialize for ContactMessage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("type", "PRIV")?;
        if let Some(snr) = self.snr {
            m.serialize_entry("SNR", &snr)?;
        }
        m.serialize_entry("pubkey_prefix", &hex(self.pubkey_prefix))?;
        m.serialize_entry("path_hash_mode", &self.path_len.hash_mode_or_minus_one())?;
        m.serialize_entry("path_len", &message_path_len(self.path_len))?;
        m.serialize_entry("txt_type", &self.txt_type.code())?;
        m.serialize_entry("sender_timestamp", &self.sender_timestamp)?;
        if let Some(signature) = self.signature {
            m.serialize_entry("signature", &hex(signature))?;
        }
        m.serialize_entry("text", &self.text)?;
        m.end()
    }
}

impl Serialize for ChannelMessage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("type", "CHAN")?;
        if let Some(snr) = self.snr {
            m.serialize_entry("SNR", &snr)?;
        }
        m.serialize_entry("channel_idx", &self.channel_idx)?;
        m.serialize_entry("path_hash_mode", &self.path_len.hash_mode_or_minus_one())?;
        m.serialize_entry("path_len", &message_path_len(self.path_len))?;
        m.serialize_entry("txt_type", &self.txt_type.code())?;
        m.serialize_entry("sender_timestamp", &self.sender_timestamp)?;
        m.serialize_entry("text", &self.text)?;
        if self.v3
            && let Some(hash) = self.txt_hash
        {
            m.serialize_entry("txt_hash", &hash)?;
        }
        if let Some(rx) = &self.rx_info {
            m.serialize_entry("path", &hex(&rx.path))?;
            m.serialize_entry("RSSI", &rx.rssi)?;
            if !self.v3 {
                m.serialize_entry("SNR", &rx.snr)?;
            }
            m.serialize_entry("recv_time", &rx.recv_time)?;
            m.serialize_entry("attempt", &rx.attempt)?;
        }
        m.end()
    }
}

impl Serialize for ChannelData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("type", "DATA")?;
        m.serialize_entry("SNR", &self.snr)?;
        m.serialize_entry("channel_idx", &self.channel_idx)?;
        m.serialize_entry("path_hash_mode", &self.path_len.hash_mode_or_minus_one())?;
        m.serialize_entry("path_len", &message_path_len(self.path_len))?;
        m.serialize_entry("data_type", &self.data_type)?;
        m.serialize_entry("data_len", &self.payload.len())?;
        m.serialize_entry("payload", &hex(&self.payload))?;
        m.end()
    }
}

impl Serialize for ReceivedMessage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Contact(m) => m.serialize(s),
            Self::Channel(m) => m.serialize(s),
            Self::ChannelData(d) => d.serialize(s),
        }
    }
}

impl Serialize for StatusResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("pubkey_pre", &hex(self.pubkey_prefix))?;
        m.serialize_entry("bat", &self.bat)?;
        m.serialize_entry("tx_queue_len", &self.tx_queue_len)?;
        m.serialize_entry("noise_floor", &self.noise_floor)?;
        m.serialize_entry("last_rssi", &self.last_rssi)?;
        m.serialize_entry("nb_recv", &self.nb_recv)?;
        m.serialize_entry("nb_sent", &self.nb_sent)?;
        m.serialize_entry("airtime", &self.airtime)?;
        m.serialize_entry("uptime", &self.uptime)?;
        m.serialize_entry("sent_flood", &self.sent_flood)?;
        m.serialize_entry("sent_direct", &self.sent_direct)?;
        m.serialize_entry("recv_flood", &self.recv_flood)?;
        m.serialize_entry("recv_direct", &self.recv_direct)?;
        m.serialize_entry("full_evts", &self.full_evts)?;
        m.serialize_entry("last_snr", &self.last_snr)?;
        m.serialize_entry("direct_dups", &self.direct_dups)?;
        m.serialize_entry("flood_dups", &self.flood_dups)?;
        m.serialize_entry("rx_airtime", &self.rx_airtime)?;
        m.serialize_entry("recv_errors", &self.recv_errors)?;
        m.end()
    }
}

impl Serialize for CoreStats {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("battery_mv", &self.battery_mv)?;
        m.serialize_entry("uptime_secs", &self.uptime_secs)?;
        m.serialize_entry("errors", &self.errors)?;
        m.serialize_entry("queue_len", &self.queue_len)?;
        m.end()
    }
}

impl Serialize for RadioStats {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("noise_floor", &self.noise_floor)?;
        m.serialize_entry("last_rssi", &self.last_rssi)?;
        m.serialize_entry("last_snr", &self.last_snr)?;
        m.serialize_entry("tx_air_secs", &self.tx_air_secs)?;
        m.serialize_entry("rx_air_secs", &self.rx_air_secs)?;
        m.end()
    }
}

impl Serialize for PacketStats {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(7))?;
        m.serialize_entry("recv", &self.recv)?;
        m.serialize_entry("sent", &self.sent)?;
        m.serialize_entry("flood_tx", &self.flood_tx)?;
        m.serialize_entry("direct_tx", &self.direct_tx)?;
        m.serialize_entry("flood_rx", &self.flood_rx)?;
        m.serialize_entry("direct_rx", &self.direct_rx)?;
        m.serialize_entry("recv_errors", &self.recv_errors)?;
        m.end()
    }
}

impl Serialize for ChannelInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("channel_idx", &self.channel_idx)?;
        m.serialize_entry("channel_name", &self.channel_name)?;
        m.serialize_entry("channel_secret", &hex(self.channel_secret))?;
        m.serialize_entry("channel_hash", &hex([self.channel_hash()]))?;
        m.end()
    }
}

impl Serialize for LoginSuccess {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if let Some(perms) = self.permissions {
            m.serialize_entry("permissions", &perms)?;
            m.serialize_entry("is_admin", &self.is_admin())?;
        }
        if let Some(prefix) = self.pubkey_prefix {
            m.serialize_entry("pubkey_prefix", &hex(prefix))?;
        }
        if let Some(v) = self.server_timestamp {
            m.serialize_entry("server_timestamp", &v)?;
        }
        if let Some(v) = self.acl_permissions {
            m.serialize_entry("acl_permissions", &v)?;
        }
        if let Some(v) = self.fw_ver_level {
            m.serialize_entry("fw_ver_level", &v)?;
        }
        m.end()
    }
}

impl Serialize for LoginFailed {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if let Some(prefix) = self.pubkey_prefix {
            m.serialize_entry("pubkey_prefix", &hex(prefix))?;
        }
        m.end()
    }
}

/// A value as meshcore_py's `lpp_format_val` gives it.
struct LppValueOut<'a>(LppType, &'a LppValue);

impl Serialize for LppValueOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match (self.0, self.1) {
            (_, LppValue::Scalar(v)) => s.serialize_f64(*v),
            (LppType::Accelerometer, LppValue::Xyz { x, y, z }) => {
                let mut m = s.serialize_map(Some(3))?;
                m.serialize_entry("acc_x", x)?;
                m.serialize_entry("acc_y", y)?;
                m.serialize_entry("acc_z", z)?;
                m.end()
            }
            (_, LppValue::Xyz { x, y, z }) => {
                let mut seq = s.serialize_seq(Some(3))?;
                seq.serialize_element(x)?;
                seq.serialize_element(y)?;
                seq.serialize_element(z)?;
                seq.end()
            }
            (_, LppValue::Rgb { r, g, b }) => {
                let mut m = s.serialize_map(Some(3))?;
                m.serialize_entry("red", &f64::from(*r))?;
                m.serialize_entry("green", &f64::from(*g))?;
                m.serialize_entry("blue", &f64::from(*b))?;
                m.end()
            }
            (
                _,
                LppValue::Gps {
                    latitude,
                    longitude,
                    altitude,
                },
            ) => {
                let mut m = s.serialize_map(Some(3))?;
                m.serialize_entry("latitude", latitude)?;
                m.serialize_entry("longitude", longitude)?;
                m.serialize_entry("altitude", altitude)?;
                m.end()
            }
        }
    }
}

impl Serialize for LppReading {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("channel", &self.channel)?;
        m.serialize_entry("type", self.lpp_type.name())?;
        m.serialize_entry("value", &LppValueOut(self.lpp_type, &self.value))?;
        m.end()
    }
}

/// The readings, as meshcore_py's `lpp` list.
impl Serialize for Telemetry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.readings.serialize(s)
    }
}

impl Serialize for TelemetryResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        if let Some(tag) = self.tag {
            // the answer to a binary request
            m.serialize_entry("tag", &tag_hex(tag))?;
            m.serialize_entry("lpp", &self.telemetry)?;
            m.serialize_entry("pubkey_prefix", &hex(self.pubkey_prefix))?;
        } else {
            // a telemetry push
            m.serialize_entry("pubkey_pre", &hex(self.pubkey_prefix))?;
            m.serialize_entry("lpp", &self.telemetry)?;
        }
        if !self.telemetry.undecoded.is_empty() {
            m.serialize_entry("lpp_undecoded", &hex(&self.telemetry.undecoded))?;
        }
        m.end()
    }
}

impl Serialize for MmaEntry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("channel", &self.channel)?;
        m.serialize_entry("type", self.lpp_type.name())?;
        m.serialize_entry("min", &LppValueOut(self.lpp_type, &self.min))?;
        m.serialize_entry("max", &LppValueOut(self.lpp_type, &self.max))?;
        m.serialize_entry("avg", &LppValueOut(self.lpp_type, &self.avg))?;
        m.end()
    }
}

impl Serialize for MmaResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("tag", &tag_hex(self.tag))?;
        m.serialize_entry("mma_data", &self.entries)?;
        m.serialize_entry("pubkey_prefix", &hex(self.pubkey_prefix))?;
        m.end()
    }
}

impl Serialize for AclEntry {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("key", &hex(self.key))?;
        m.serialize_entry("perm", &self.perm)?;
        m.end()
    }
}

impl Serialize for AclResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("tag", &tag_hex(self.tag))?;
        m.serialize_entry("acl_data", &self.entries)?;
        m.serialize_entry("pubkey_prefix", &hex(self.pubkey_prefix))?;
        m.end()
    }
}

impl Serialize for Neighbour {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("pubkey", &hex(&self.pubkey))?;
        m.serialize_entry("secs_ago", &self.secs_ago)?;
        m.serialize_entry("snr", &self.snr)?;
        m.end()
    }
}

impl Serialize for NeighboursResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("pubkey_prefix", &hex(self.pubkey_prefix))?;
        if let Some(tag) = self.tag {
            m.serialize_entry("tag", &tag_hex(tag))?;
        }
        m.serialize_entry("pubkey_prefix_length", &self.pubkey_prefix_length)?;
        m.serialize_entry("neighbours_count", &self.neighbours_count)?;
        m.serialize_entry("results_count", &self.results_count)?;
        m.serialize_entry("neighbours", &self.neighbours)?;
        m.end()
    }
}

impl Serialize for PathDiscovery {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(7))?;
        m.serialize_entry("pubkey_pre", &hex(self.pubkey_prefix))?;
        m.serialize_entry("out_path_len", &self.out_path_len.hops_or_minus_one())?;
        m.serialize_entry(
            "out_path_hash_len",
            &hash_len_or_minus_one(self.out_path_len),
        )?;
        m.serialize_entry("out_path", &hex(&self.out_path))?;
        m.serialize_entry("in_path_len", &self.in_path_len.hops_or_minus_one())?;
        m.serialize_entry("in_path_hash_len", &hash_len_or_minus_one(self.in_path_len))?;
        m.serialize_entry("in_path", &hex(&self.in_path))?;
        m.end()
    }
}

fn hash_len_or_minus_one(len: PathLen) -> i8 {
    len.hash_size().map_or(-1, u8::cast_signed)
}

impl Serialize for TraceData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Hop<'a>(Option<&'a [u8]>, f32);
        impl Serialize for Hop<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                let mut m = s.serialize_map(None)?;
                if let Some(hash) = self.0 {
                    m.serialize_entry("hash", &hex(hash))?;
                }
                m.serialize_entry("snr", &self.1)?;
                m.end()
            }
        }
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("tag", &self.tag)?;
        m.serialize_entry("auth", &self.auth_code)?;
        m.serialize_entry("flags", &self.flags)?;
        m.serialize_entry("path_len", &self.hops.len())?;
        if !self.hops.is_empty() {
            let mut path: Vec<Hop<'_>> = self
                .hops
                .iter()
                .map(|hop| Hop(Some(&hop.hash), hop.snr))
                .collect();
            if let Some(snr) = self.final_snr {
                path.push(Hop(None, snr));
            }
            m.serialize_entry("path", &path)?;
        }
        m.end()
    }
}

impl Serialize for RxLogData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("raw_hex", &hex(&self.raw))?;
        m.serialize_entry("recv_time", &self.recv_time)?;
        if let Some(snr) = self.snr {
            m.serialize_entry("snr", &snr)?;
        }
        if let Some(rssi) = self.rssi {
            m.serialize_entry("rssi", &rssi)?;
        }
        if !self.payload.is_empty() {
            m.serialize_entry("payload", &hex(&self.payload))?;
            m.serialize_entry("payload_length", &self.payload.len())?;
            if let Some(packet) = &self.packet {
                serialize_packet(&mut m, packet)?;
            } else {
                // meshcore_py's placeholders for a packet too short to read
                m.serialize_entry("route_type", &-1)?;
                m.serialize_entry("route_typename", "UNK")?;
                m.serialize_entry("payload_type", &-1)?;
                m.serialize_entry("payload_typename", "UNK")?;
                m.serialize_entry("payload_ver", &0)?;
                m.serialize_entry("path_len", &0)?;
                m.serialize_entry("path_hash_size", &1)?;
                m.serialize_entry("path", "")?;
                m.serialize_entry("pkt_payload", "")?;
                m.serialize_entry("pkt_hash", &0)?;
            }
        }
        m.end()
    }
}

fn serialize_packet<M: SerializeMap>(m: &mut M, packet: &PacketInfo) -> Result<(), M::Error> {
    m.serialize_entry("header", &packet.header)?;
    m.serialize_entry("route_type", &packet.route_type.bits())?;
    m.serialize_entry("route_typename", packet.route_type.name())?;
    m.serialize_entry("payload_type", &packet.payload_type.bits())?;
    m.serialize_entry("payload_typename", packet.payload_type.name())?;
    m.serialize_entry("payload_ver", &packet.payload_ver)?;
    if let Some(code) = packet.transport_code {
        m.serialize_entry("transport_code", &hex(code))?;
    }
    m.serialize_entry("path_len", &packet.path_len)?;
    m.serialize_entry("path_hash_size", &packet.path_hash_size)?;
    m.serialize_entry("path", &hex(&packet.path))?;
    m.serialize_entry("pkt_payload", &hex(&packet.pkt_payload))?;
    m.serialize_entry("pkt_hash", &packet.pkt_hash)?;
    match &packet.content {
        PacketContent::None => {}
        PacketContent::GroupText(group) => {
            m.serialize_entry("chan_hash", &hex([group.chan_hash]))?;
            m.serialize_entry("cipher_mac", &hex(group.cipher_mac))?;
            m.serialize_entry("crypted", &hex(&group.crypted))?;
            if let Some(name) = &group.chan_name {
                m.serialize_entry("chan_name", name)?;
            }
            if let Some(d) = &group.decrypted {
                m.serialize_entry("message", &d.message)?;
                m.serialize_entry("msg_hash", &d.msg_hash)?;
                m.serialize_entry("sender_timestamp", &d.sender_timestamp)?;
                m.serialize_entry("attempt", &d.attempt)?;
                m.serialize_entry("txt_type", &d.txt_type)?;
            }
        }
        PacketContent::Advert(advert) => {
            if let Some(name) = &advert.adv_name {
                m.serialize_entry("adv_name", name)?;
            }
            m.serialize_entry("adv_key", &advert.adv_key)?;
            m.serialize_entry("adv_timestamp", &advert.adv_timestamp)?;
            m.serialize_entry("signature", &hex(&advert.signature))?;
            m.serialize_entry("adv_flags", &advert.adv_flags)?;
            m.serialize_entry("adv_type", &advert.adv_type)?;
            if let Some(lat) = advert.adv_lat {
                m.serialize_entry("adv_lat", &lat)?;
            }
            if let Some(lon) = advert.adv_lon {
                m.serialize_entry("adv_lon", &lon)?;
            }
            if let Some(feat) = advert.adv_feat1 {
                m.serialize_entry("adv_feat1", &hex(feat))?;
            }
            if let Some(feat) = advert.adv_feat2 {
                m.serialize_entry("adv_feat2", &hex(feat))?;
            }
        }
    }
    Ok(())
}

impl Serialize for ControlData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(5))?;
        m.serialize_entry("SNR", &self.snr)?;
        m.serialize_entry("RSSI", &self.rssi)?;
        m.serialize_entry("path_len", &self.path_len)?;
        m.serialize_entry("payload_type", &self.payload_type)?;
        m.serialize_entry("payload", &hex(&self.payload))?;
        m.end()
    }
}

impl Serialize for DiscoverResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(7))?;
        m.serialize_entry("SNR", &self.snr)?;
        m.serialize_entry("RSSI", &self.rssi)?;
        m.serialize_entry("path_len", &self.path_len)?;
        m.serialize_entry("node_type", &self.node_type)?;
        m.serialize_entry("SNR_in", &self.snr_in)?;
        m.serialize_entry("tag", &tag_hex(self.tag))?;
        m.serialize_entry("pubkey", &hex(&self.pubkey))?;
        m.end()
    }
}

impl Serialize for RawData {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("SNR", &self.snr)?;
        m.serialize_entry("RSSI", &self.rssi)?;
        m.serialize_entry("payload", &hex(&self.payload))?;
        m.end()
    }
}

impl Serialize for BinaryResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("tag", &tag_hex(self.tag))?;
        m.serialize_entry("data", &hex(&self.data))?;
        m.end()
    }
}

impl Serialize for CustomVars {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.len()))?;
        for (name, value) in self.iter() {
            m.serialize_entry(name, value)?;
        }
        m.end()
    }
}

impl Serialize for FreqRange {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("min", &self.min)?;
        m.serialize_entry("max", &self.max)?;
        m.end()
    }
}

impl Serialize for DefaultFloodScope {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("scope_name", &self.name)?;
        m.serialize_entry("scope_key", &hex(self.key))?;
        m.end()
    }
}

impl Serialize for AutoAddConfig {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(None)?;
        m.serialize_entry("config", &self.config)?;
        if let Some(max_hops) = self.max_hops {
            m.serialize_entry("max_hops", &max_hops)?;
        }
        m.end()
    }
}

impl Serialize for AdvertPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(4))?;
        m.serialize_entry("timestamp", &self.timestamp)?;
        m.serialize_entry("path_hash_mode", &self.path_len.hash_mode_or_minus_one())?;
        m.serialize_entry("path_len", &self.path_len.hops_or_minus_one())?;
        m.serialize_entry("path", &hex(&self.path))?;
        m.end()
    }
}

impl Serialize for TuningParams {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("rx_delay", &self.rx_delay)?;
        m.serialize_entry("airtime_factor", &self.airtime_factor)?;
        m.end()
    }
}

impl Serialize for OwnerInfo {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(3))?;
        m.serialize_entry("firmware_version", &self.firmware_version)?;
        m.serialize_entry("name", &self.name)?;
        m.serialize_entry("owner", &self.owner)?;
        m.end()
    }
}

impl Serialize for AnonOwner {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("name", &self.name)?;
        m.serialize_entry("owner", &self.owner)?;
        m.end()
    }
}

impl Serialize for SensorSubscription {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(2))?;
        m.serialize_entry("expiry_secs", &self.expiry_secs)?;
        m.serialize_entry("scope", &self.scope)?;
        m.end()
    }
}

impl Serialize for PrivateKey {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

fn single<S: Serializer, V: Serialize + ?Sized>(
    s: S,
    key: &str,
    value: &V,
) -> Result<S::Ok, S::Error> {
    let mut m = s.serialize_map(Some(1))?;
    m.serialize_entry(key, value)?;
    m.end()
}

fn empty<S: Serializer>(s: S) -> Result<S::Ok, S::Error> {
    s.serialize_map(Some(0))?.end()
}

/// An event serializes as meshcore_py's payload of the same event.
impl Serialize for Event {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Connected { info, reconnected } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("connection_info", info)?;
                if *reconnected {
                    m.serialize_entry("reconnected", &true)?;
                }
                m.end()
            }
            // meshcore_py adds a flag except for a manual disconnect: after
            // giving up reconnecting, or for a link lost without reconnecting
            // (where it is false unless reconnecting is configured with zero
            // attempts, which this event does not record).
            Self::Disconnected { reason } => {
                let mut m = s.serialize_map(None)?;
                m.serialize_entry("reason", reason)?;
                match reason.as_str() {
                    "manual_disconnect" => {}
                    "reconnect_failed" => m.serialize_entry("max_attempts_exceeded", &true)?,
                    _ => m.serialize_entry("reconnect_failed", &false)?,
                }
                m.end()
            }
            Self::Ok { value } => match value {
                Some(value) => single(s, "value", value),
                None => empty(s),
            },
            Self::Error(info) => info.serialize(s),
            Self::SelfInfo(info) => info.serialize(s),
            Self::DeviceInfo(info) => info.serialize(s),
            Self::Battery(battery) => battery.serialize(s),
            Self::CurrentTime(time) => single(s, "time", time),
            Self::CustomVars(vars) => vars.serialize(s),
            Self::ContactsStart { count } => single(s, "count", count),
            Self::NextContact(c) | Self::NewContact(c) => c.serialize(s),
            Self::Contacts(list) => list.serialize(s),
            Self::ContactUri(uri) => single(s, "uri", uri),
            Self::ContactDeleted(key) => single(s, "pubkey", key),
            Self::ContactsFull | Self::MessagesWaiting => empty(s),
            Self::MessageSent(sent) => sent.serialize(s),
            Self::ContactMessage(m) => m.serialize(s),
            Self::ChannelMessage(m) => m.serialize(s),
            Self::ChannelData(d) => d.serialize(s),
            Self::NoMoreMessages => single(s, "messages_available", &false),
            Self::Advertisement(key) | Self::PathUpdate(key) => single(s, "public_key", key),
            Self::Ack(ack) => ack.serialize(s),
            Self::RawData(raw) => raw.serialize(s),
            Self::LoginSuccess(login) => login.serialize(s),
            Self::LoginFailed(login) => login.serialize(s),
            Self::StatusResponse(status) => status.serialize(s),
            Self::RxLog(log) => log.serialize(s),
            Self::TraceData(trace) => trace.serialize(s),
            Self::TelemetryResponse(t) => t.serialize(s),
            Self::BinaryResponse(r) => r.serialize(s),
            Self::MmaResponse(r) => r.serialize(s),
            Self::AclResponse(r) => r.serialize(s),
            Self::NeighboursResponse(r) => r.serialize(s),
            Self::StatsCore(stats) => stats.serialize(s),
            Self::StatsRadio(stats) => stats.serialize(s),
            Self::StatsPackets(stats) => stats.serialize(s),
            Self::ChannelInfo(channel) => channel.serialize(s),
            Self::PathResponse(path) => path.serialize(s),
            Self::PrivateKey(key) => single(s, "private_key", key.as_ref()),
            Self::Disabled => single(s, "reason", "private_key_export_disabled"),
            Self::TuningParams(t) => t.serialize(s),
            Self::ControlData(c) => c.serialize(s),
            Self::DiscoverResponse(d) => d.serialize(s),
            Self::SignStart { max_length } => single(s, "max_length", max_length),
            Self::Signature(signature) => single(s, "signature", &hex(signature)),
            Self::AllowedRepeatFreq(freqs) => single(s, "freqs", freqs),
            Self::DefaultFloodScope(scope) => match scope {
                Some(scope) => scope.serialize(s),
                None => empty(s),
            },
            Self::AutoAddConfig(config) => config.serialize(s),
            Self::AdvertPath(path) => path.serialize(s),
            Self::CliReply(text) => single(s, "text", text),
            Self::Unknown { code, data } => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("code", code)?;
                m.serialize_entry("data", &hex(data))?;
                m.end()
            }
        }
    }
}
