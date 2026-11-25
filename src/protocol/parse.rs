//! Parsers for the payloads the device sends. Each takes the frame after its
//! code byte.

use crate::error::{Error, Result};
use crate::lpp::{Telemetry, parse_mma};
use crate::protocol::bytes::{ByteReader, lossy_ignore, text_until_nul, until_nul};
use crate::protocol::path::PathLen;
use crate::types::{
    Ack, AclEntry, AdvertPath, AutoAddConfig, BatteryStatus, ChannelData, ChannelInfo,
    ChannelMessage, Contact, ContactMessage, ContactType, ControlData, CoreStats,
    DefaultFloodScope, DeviceInfo, DiscoverResponse, FreqRange, LoginFailed, LoginSuccess,
    MmaResponse, Neighbour, NeighboursResponse, PacketStats, PathDiscovery, PublicKey, RadioStats,
    RawData, SelfInfo, SentInfo, StatusResponse, TextType, TraceData, TraceHop, TuningParams,
};

fn snr(raw: i8) -> f32 {
    f32::from(raw) / 4.0
}

/// A contact (`RESP_CODE_CONTACT`, `PUSH_CODE_NEW_ADVERT`): key, type, flags,
/// path length, a 64-byte path field, a 32-byte name field, last advert,
/// latitude, longitude, lastmod.
pub(crate) fn contact(data: &[u8]) -> Result<Contact> {
    let mut r = ByteReader::new(data);
    let public_key = PublicKey::new(r.array()?);
    let contact_type = ContactType::from_code(r.u8()?);
    let flags = r.u8()?;
    let out_path_len = PathLen::from_byte(r.u8()?);
    let path_field = r.bytes(64)?;
    // Take exactly the bytes the path occupies: a hop hash may contain 0x00.
    let out_path = path_field[..out_path_len.byte_len().min(64)].to_vec();
    let name_raw: [u8; 32] = r.array()?;
    Ok(Contact {
        public_key,
        contact_type,
        flags,
        out_path_len,
        out_path,
        name: text_until_nul(&name_raw),
        name_raw,
        last_advert: r.u32()?,
        latitude_e6: r.i32()?,
        longitude_e6: r.i32()?,
        lastmod: r.u32()?,
    })
}

/// The device's own information (`RESP_CODE_SELF_INFO`); returns it with the
/// length of the name on the device (decoded, it lacks the bytes of a
/// character cut short).
pub(crate) fn self_info(data: &[u8]) -> Result<(SelfInfo, usize)> {
    let mut r = ByteReader::new(data);
    let adv_type = r.u8()?;
    let tx_power = r.i8()?;
    let max_tx_power = r.u8()?;
    let public_key = PublicKey::new(r.array()?);
    let latitude_e6 = r.i32()?;
    let longitude_e6 = r.i32()?;
    let multi_acks = r.u8()?;
    let adv_loc_policy = r.u8()?;
    let telemetry_mode = r.u8()?;
    let manual_add_contacts = r.u8()? > 0;
    let radio_freq = f64::from(r.u32()?) / 1000.0;
    let radio_bw = f64::from(r.u32()?) / 1000.0;
    let radio_sf = r.u8()?;
    let radio_cr = r.u8()?;
    let name = until_nul(r.rest());
    let info = SelfInfo {
        adv_type,
        tx_power,
        max_tx_power,
        public_key,
        latitude_e6,
        longitude_e6,
        multi_acks,
        adv_loc_policy,
        telemetry_mode_base: telemetry_mode & 0b11,
        telemetry_mode_loc: (telemetry_mode >> 2) & 0b11,
        telemetry_mode_env: (telemetry_mode >> 4) & 0b11,
        manual_add_contacts,
        radio_freq,
        radio_bw,
        radio_sf,
        radio_cr,
        name: lossy_ignore(name),
    };
    Ok((info, name.len()))
}

/// Firmware information (`RESP_CODE_DEVICE_INFO`).
pub(crate) fn device_info(data: &[u8]) -> Result<DeviceInfo> {
    let mut r = ByteReader::new(data);
    let fw_ver = r.u8()?;
    let mut info = DeviceInfo {
        fw_ver,
        max_contacts: None,
        max_channels: None,
        ble_pin: None,
        fw_build: None,
        model: None,
        ver: None,
        repeat: None,
        path_hash_mode: None,
    };
    if fw_ver >= 3 {
        info.max_contacts = Some(u16::from(r.u8()?) * 2);
        info.max_channels = Some(r.u8()?);
        info.ble_pin = Some(r.u32()?);
        info.fw_build = Some(text_until_nul(r.bytes(12)?));
        info.model = Some(text_until_nul(r.bytes(40)?));
        info.ver = Some(text_until_nul(r.bytes(20)?));
    }
    if fw_ver >= 9 {
        info.repeat = r.opt_u8().map(|b| b != 0);
    }
    if fw_ver >= 10 {
        info.path_hash_mode = r.opt_u8();
    }
    Ok(info)
}

/// The reply to a send command (`RESP_CODE_SENT`): type, expected ACK, timeout.
pub(crate) fn sent(data: &[u8]) -> Result<SentInfo> {
    let mut r = ByteReader::new(data);
    if r.remaining() < 9 {
        return Err(Error::protocol("invalid_frame_length"));
    }
    Ok(SentInfo {
        flood: r.u8()? == 1,
        expected_ack: r.u32()?,
        suggested_timeout: r.u32()?,
    })
}

/// A private message; `v3` frames start with SNR and two reserved bytes.
pub(crate) fn contact_message(data: &[u8], v3: bool) -> Result<ContactMessage> {
    let mut r = ByteReader::new(data);
    let snr = if v3 {
        let value = snr(r.i8()?);
        r.skip(2)?;
        Some(value)
    } else {
        None
    };
    let pubkey_prefix = r.array()?;
    let path_len = PathLen::from_byte(r.u8()?);
    let txt_type = TextType::from_code(r.u8()?);
    let sender_timestamp = r.u32()?;
    let signature = if txt_type == TextType::SignedPlain {
        Some(r.array()?)
    } else {
        None
    };
    Ok(ContactMessage {
        pubkey_prefix,
        path_len,
        txt_type,
        sender_timestamp,
        signature,
        text: lossy_ignore(r.rest()),
        snr,
    })
}

/// A channel message; returns it with the text bytes the RX log hash is
/// computed over (NULs stripped).
pub(crate) fn channel_message(data: &[u8], v3: bool) -> Result<(ChannelMessage, Vec<u8>)> {
    let mut r = ByteReader::new(data);
    let snr = if v3 {
        let value = snr(r.i8()?);
        r.skip(2)?;
        Some(value)
    } else {
        None
    };
    let channel_idx = r.u8()?;
    let path_len = PathLen::from_byte(r.u8()?);
    let txt_type = TextType::from_code(r.u8()?);
    let sender_timestamp = r.u32()?;
    let text_bytes = crate::packet::trim_nul(r.rest()).to_vec();
    let message = ChannelMessage {
        channel_idx,
        path_len,
        txt_type,
        sender_timestamp,
        text: lossy_ignore(&text_bytes),
        snr,
        txt_hash: None,
        rx_info: None,
        v3,
    };
    Ok((message, text_bytes))
}

/// A channel datagram (`RESP_CODE_CHANNEL_DATA_RECV`): SNR, 2 reserved,
/// channel, path length, data type (LE 16), data length, data.
pub(crate) fn channel_data(data: &[u8]) -> Result<ChannelData> {
    let mut r = ByteReader::new(data);
    if r.remaining() < 8 {
        return Err(Error::protocol("invalid_frame_length"));
    }
    let snr = snr(r.i8()?);
    r.skip(2)?;
    let channel_idx = r.u8()?;
    let path_len = PathLen::from_byte(r.u8()?);
    let data_type = r.u16()?;
    let data_len = usize::from(r.u8()?);
    let payload = r.bytes(data_len.min(r.remaining()))?.to_vec();
    Ok(ChannelData {
        snr,
        channel_idx,
        path_len,
        data_type,
        payload,
    })
}

/// Battery and storage: millivolts, then used and total KiB when present.
pub(crate) fn battery(data: &[u8]) -> Result<BatteryStatus> {
    let mut r = ByteReader::new(data);
    let level = r.u16()?;
    let (used_kb, total_kb) = if r.remaining() >= 8 {
        (Some(r.u32()?), Some(r.u32()?))
    } else {
        (None, None)
    };
    Ok(BatteryStatus {
        level,
        used_kb,
        total_kb,
    })
}

/// A channel: index, 32-byte name field, 16-byte secret.
pub(crate) fn channel_info(data: &[u8]) -> Result<ChannelInfo> {
    let mut r = ByteReader::new(data);
    let channel_idx = r.u8()?;
    let channel_name = text_until_nul(r.bytes(32)?);
    let channel_secret = r.array()?;
    Ok(ChannelInfo {
        channel_idx,
        channel_name,
        channel_secret,
    })
}

/// Statistics of one of the three groups.
pub(crate) enum Stats {
    Core(CoreStats),
    Radio(RadioStats),
    Packets(PacketStats),
}

pub(crate) fn stats(data: &[u8]) -> Result<Stats> {
    let mut r = ByteReader::new(data);
    match r.u8()? {
        0 => Ok(Stats::Core(CoreStats {
            battery_mv: r.u16()?,
            uptime_secs: r.u32()?,
            errors: r.u16()?,
            queue_len: r.u8()?,
        })),
        1 => Ok(Stats::Radio(RadioStats {
            noise_floor: r.i16()?,
            last_rssi: r.i8()?,
            last_snr: snr(r.i8()?),
            tx_air_secs: r.u32()?,
            rx_air_secs: r.u32()?,
        })),
        2 => Ok(Stats::Packets(PacketStats {
            recv: r.u32()?,
            sent: r.u32()?,
            flood_tx: r.u32()?,
            direct_tx: r.u32()?,
            flood_rx: r.u32()?,
            direct_rx: r.u32()?,
            recv_errors: r.opt_u32(),
        })),
        other => Err(Error::protocol(format!("unknown_stats_type: {other}"))),
    }
}

/// Status fields (52 bytes, 56 with the receive error count) of a status answer.
pub(crate) fn status(
    data: &[u8],
    pubkey_prefix: [u8; 6],
    tag: Option<u32>,
) -> Result<StatusResponse> {
    let mut r = ByteReader::new(data);
    if r.remaining() < 52 {
        return Err(Error::protocol("invalid_frame_length"));
    }
    Ok(StatusResponse {
        pubkey_prefix,
        tag,
        bat: r.u16()?,
        tx_queue_len: r.u16()?,
        noise_floor: r.i16()?,
        last_rssi: r.i16()?,
        nb_recv: r.u32()?,
        nb_sent: r.u32()?,
        airtime: r.u32()?,
        uptime: r.u32()?,
        sent_flood: r.u32()?,
        sent_direct: r.u32()?,
        recv_flood: r.u32()?,
        recv_direct: r.u32()?,
        full_evts: r.u16()?,
        last_snr: f32::from(r.i16()?) / 4.0,
        direct_dups: r.u16()?,
        flood_dups: r.u16()?,
        rx_airtime: r.u32()?,
        recv_errors: r.opt_u32(),
    })
}

/// A status push (`PUSH_CODE_STATUS_RESPONSE`): reserved, key prefix, status.
pub(crate) fn status_push(data: &[u8]) -> Result<StatusResponse> {
    let mut r = ByteReader::new(data);
    r.skip(1)?;
    let prefix = r.array()?;
    status(r.rest(), prefix, None)
}

pub(crate) fn ack(data: &[u8]) -> Result<Ack> {
    let mut r = ByteReader::new(data);
    Ok(Ack {
        code: r.u32()?,
        trip_time: r.opt_u32(),
    })
}

/// A login success: permissions, key prefix, then (new firmware) server
/// clock, ACL permissions and firmware level. Each field is read at its fixed
/// offset when the frame holds all of it, as meshcore_py gates them.
pub(crate) fn login_success(data: &[u8]) -> LoginSuccess {
    LoginSuccess {
        permissions: data.first().copied(),
        pubkey_prefix: data.get(1..7).and_then(|prefix| prefix.try_into().ok()),
        server_timestamp: data
            .get(7..11)
            .and_then(|time| time.try_into().ok())
            .map(u32::from_le_bytes),
        acl_permissions: data.get(11).copied(),
        fw_ver_level: data.get(12).copied(),
    }
}

pub(crate) fn login_failed(data: &[u8]) -> LoginFailed {
    let mut r = ByteReader::new(data);
    let _reserved = r.opt_u8();
    LoginFailed {
        pubkey_prefix: if data.len() >= 7 {
            r.array().ok()
        } else {
            None
        },
    }
}

/// A trace: reserved, path length in bytes, flags, tag, auth code, the hop
/// hashes, one SNR per hop, the final SNR.
pub(crate) fn trace(data: &[u8]) -> Result<TraceData> {
    let mut r = ByteReader::new(data);
    r.skip(1)?;
    let path_bytes = usize::from(r.u8()?);
    let flags = r.u8()?;
    let tag = r.u32()?;
    let auth_code = r.u32()?;
    let hash_len = 1usize << (flags & 3);
    let hop_count = path_bytes / hash_len;
    let hashes = r.bytes(hop_count * hash_len)?;
    let snrs = r.bytes(hop_count)?;
    let hops = hashes
        .chunks_exact(hash_len)
        .zip(snrs)
        .map(|(hash, &raw)| TraceHop {
            hash: hash.to_vec(),
            snr: snr(raw.cast_signed()),
        })
        .collect();
    // The firmware always appends it; without hops meshcore_py ignores it.
    let final_snr = if hop_count > 0 {
        Some(snr(r.i8()?))
    } else {
        r.opt_u8().map(|raw| snr(raw.cast_signed()))
    };
    Ok(TraceData {
        tag,
        auth_code,
        flags,
        hops,
        final_snr,
    })
}

/// A path length byte and the path after it.
fn read_path(r: &mut ByteReader<'_>) -> Result<(PathLen, Vec<u8>)> {
    let len = PathLen::from_byte(r.u8()?);
    let path = r.bytes(len.byte_len())?.to_vec();
    Ok((len, path))
}

/// A path discovery result: reserved, key prefix, out path, in path.
pub(crate) fn path_discovery(data: &[u8]) -> Result<PathDiscovery> {
    let mut r = ByteReader::new(data);
    r.skip(1)?;
    let pubkey_prefix = r.array()?;
    let (out_path_len, out_path) = read_path(&mut r)?;
    let (in_path_len, in_path) = read_path(&mut r)?;
    Ok(PathDiscovery {
        pubkey_prefix,
        out_path_len,
        out_path,
        in_path_len,
        in_path,
    })
}

/// Control data: SNR, RSSI, path length, payload. A node discovery answer
/// (type `0x9N`) also yields a [`DiscoverResponse`].
pub(crate) fn control_data(data: &[u8]) -> Result<(ControlData, Option<DiscoverResponse>)> {
    let mut r = ByteReader::new(data);
    let snr_value = snr(r.i8()?);
    let rssi = r.i8()?;
    let path_len = r.u8()?;
    let payload = r.rest().to_vec();
    let Some(&payload_type) = payload.first() else {
        return Err(Error::protocol("invalid_frame_length"));
    };
    // A discovery answer too short to read still leaves the control data:
    // control packets come unchecked from any node.
    let read_discover = || {
        let mut p = ByteReader::new(&payload[1..]);
        let snr_in = snr(p.i8().ok()?);
        let tag = p.u32().ok()?;
        let key = p.rest();
        let key_len = if key.len() < 32 { key.len().min(8) } else { 32 };
        Some(DiscoverResponse {
            snr: snr_value,
            rssi,
            path_len,
            node_type: payload_type & 0x0F,
            snr_in,
            tag,
            pubkey: key[..key_len].to_vec(),
        })
    };
    let discover = if payload_type & 0xF0 == 0x90 {
        read_discover()
    } else {
        None
    };
    Ok((
        ControlData {
            snr: snr_value,
            rssi,
            path_len,
            payload_type,
            payload,
        },
        discover,
    ))
}

/// Raw data: SNR, RSSI, a reserved byte, payload.
pub(crate) fn raw_data(data: &[u8]) -> Result<RawData> {
    let mut r = ByteReader::new(data);
    let snr = snr(r.i8()?);
    let rssi = r.i8()?;
    r.skip(1)?;
    Ok(RawData {
        snr,
        rssi,
        payload: r.rest().to_vec(),
    })
}

pub(crate) fn advert_path(data: &[u8]) -> Result<AdvertPath> {
    let mut r = ByteReader::new(data);
    let timestamp = r.u32()?;
    let (path_len, path) = read_path(&mut r)?;
    Ok(AdvertPath {
        timestamp,
        path_len,
        path,
    })
}

pub(crate) fn autoadd_config(data: &[u8]) -> Result<AutoAddConfig> {
    let mut r = ByteReader::new(data);
    Ok(AutoAddConfig {
        config: r.u8()?,
        max_hops: r.opt_u8(),
    })
}

/// Pairs of lower and upper frequency until a zero or the end.
pub(crate) fn repeat_freqs(data: &[u8]) -> Vec<FreqRange> {
    let mut r = ByteReader::new(data);
    let mut ranges = Vec::new();
    while r.remaining() >= 8 {
        let (Ok(min), Ok(max)) = (r.u32(), r.u32()) else {
            break;
        };
        if min == 0 || max == 0 {
            break;
        }
        ranges.push(FreqRange { min, max });
    }
    ranges
}

/// The default flood scope: a 31-byte name field and a 16-byte key, or
/// nothing when no scope is set.
pub(crate) fn default_flood_scope(data: &[u8]) -> Result<Option<DefaultFloodScope>> {
    if data.len() < 47 {
        return Ok(None);
    }
    let mut r = ByteReader::new(data);
    // The name ends at its first NUL: after a shorter name the field still
    // holds the old name's tail.
    let name = text_until_nul(r.bytes(31)?);
    let key = r.array()?;
    Ok(Some(DefaultFloodScope { name, key }))
}

pub(crate) fn tuning_params(data: &[u8]) -> Result<TuningParams> {
    let mut r = ByteReader::new(data);
    Ok(TuningParams {
        rx_delay: r.u32()?,
        airtime_factor: r.u32()?,
    })
}

/// A telemetry push: reserved, key prefix, LPP readings.
pub(crate) fn telemetry_push(data: &[u8]) -> Result<([u8; 6], Telemetry)> {
    let mut r = ByteReader::new(data);
    r.skip(1)?;
    let prefix = r.array()?;
    Ok((prefix, Telemetry::parse(r.rest())))
}

/// An ACL answer: 7-byte entries (key prefix, permissions); all-zero keys
/// (padding) are skipped.
pub(crate) fn acl(data: &[u8]) -> Vec<AclEntry> {
    data.as_chunks::<7>()
        .0
        .iter()
        .filter(|entry| entry[..6] != [0; 6])
        .map(|entry| {
            let mut key = [0u8; 6];
            key.copy_from_slice(&entry[..6]);
            AclEntry {
                key,
                perm: entry[6],
            }
        })
        .collect()
}

/// A min/max/average answer: the sensor's clock, then the series (from
/// offset 4 also when the clock is cut short, as meshcore_py reads them).
pub(crate) fn mma(data: &[u8], pubkey_prefix: [u8; 6], tag: u32) -> MmaResponse {
    let timestamp = ByteReader::new(data).opt_u32();
    let (entries, undecoded) = parse_mma(data.get(4..).unwrap_or_default());
    MmaResponse {
        pubkey_prefix,
        tag,
        timestamp,
        entries,
        undecoded,
    }
}

/// A neighbours answer: total count, count in this answer, then per
/// neighbour its key prefix (as long as asked for, at most the 32 bytes the
/// repeater sends), seconds since heard and SNR.
pub(crate) fn neighbours(
    data: &[u8],
    pubkey_prefix: [u8; 6],
    tag: u32,
    prefix_len: u8,
) -> Result<NeighboursResponse> {
    let key_len = usize::from(prefix_len).min(crate::types::PUBLIC_KEY_LEN);
    let mut r = ByteReader::new(data);
    let neighbours_count = r.i16()?;
    let results_count = r.i16()?;
    let mut list = Vec::new();
    for _ in 0..results_count.max(0) {
        list.push(Neighbour {
            pubkey: r.bytes(key_len)?.to_vec(),
            secs_ago: r.i32()?,
            snr: snr(r.i8()?),
        });
    }
    Ok(NeighboursResponse {
        pubkey_prefix,
        tag: Some(tag),
        pubkey_prefix_length: prefix_len,
        neighbours_count,
        results_count,
        neighbours: list,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn contact_bytes(name: &[u8], path_len: u8, path: &[u8]) -> Vec<u8> {
        let mut data = vec![0x11; 32];
        data.extend([2, 0x05, path_len]);
        let mut field = [0u8; 64];
        field[..path.len()].copy_from_slice(path);
        data.extend(field);
        let mut name_field = [0u8; 32];
        name_field[..name.len()].copy_from_slice(name);
        data.extend(name_field);
        data.extend(1000u32.to_le_bytes());
        data.extend(16_000_002i32.to_le_bytes());
        data.extend((-2_097_148i32).to_le_bytes());
        data.extend(2000u32.to_le_bytes());
        data
    }

    #[test]
    fn contact_with_multibyte_path_containing_zero() {
        // 2 hops of 2-byte hashes; a hash holds 0x00
        let c = contact(&contact_bytes(b"Rep", 0x42, &[0xaa, 0x00, 0xbb, 0xcc])).unwrap();
        assert_eq!(c.out_path_len.hops(), Some(2));
        assert_eq!(c.out_path_len.hash_mode(), Some(1));
        assert_eq!(c.out_path, vec![0xaa, 0x00, 0xbb, 0xcc]);
        assert_eq!(c.name, "Rep");
        assert_eq!(c.contact_type, ContactType::Repeater);
        assert_eq!(c.latitude_e6, 16_000_002);
        assert_eq!(c.lastmod, 2000);
    }

    #[test]
    fn contact_path_announced_longer_than_stored() {
        // another client wrote 40 hops of 2 bytes; the device keeps 64 bytes
        let c = contact(&contact_bytes(b"Rep", 0x68, &[7; 64])).unwrap();
        assert_eq!(c.out_path_len.byte_len(), 80);
        assert_eq!(c.out_path, vec![7; 64]);
        assert_eq!(c.name, "Rep");
    }

    #[test]
    fn contact_flood_path() {
        let c = contact(&contact_bytes(b"Node", 0xff, &[])).unwrap();
        assert!(c.is_flood());
        assert_eq!(c.out_path, Vec::<u8>::new());
        assert!(contact(&[0; 100]).is_err());
    }

    #[test]
    fn short_self_info_is_an_error_not_a_panic() {
        assert!(self_info(&[0; 55]).is_err());
        let mut data = vec![1, 0xf7, 22];
        data.extend([0x22; 32]);
        data.extend([0; 12]);
        data.extend(869_525u32.to_le_bytes());
        data.extend(250_000u32.to_le_bytes());
        data.extend([11, 5]);
        data.extend(b"MyNode");
        let (info, name_len) = self_info(&data).unwrap();
        assert_eq!(info.tx_power, -9);
        assert_eq!((info.name.as_str(), name_len), ("MyNode", 6));
        assert!((info.radio_freq - 869.525).abs() < 1e-9);
        // a name cut inside a character
        data.push(0xc3);
        let (info, name_len) = self_info(&data).unwrap();
        assert_eq!((info.name.as_str(), name_len), ("MyNode", 7));
    }

    #[test]
    fn device_info_with_repeat_and_hash_mode() {
        let mut data = vec![10, 175, 40];
        data.extend(123_456u32.to_le_bytes());
        let mut build = [0u8; 12];
        build[..11].copy_from_slice(b"15 Oct 2026");
        data.extend(build);
        let mut model = [0u8; 40];
        model[..6].copy_from_slice(b"Heltec");
        data.extend(model);
        let mut ver = [0u8; 20];
        ver[..7].copy_from_slice(b"v1.18.0");
        data.extend(ver);
        data.extend([1, 2]);
        let info = device_info(&data).unwrap();
        assert_eq!(info.max_contacts, Some(350));
        assert_eq!(info.fw_build.as_deref(), Some("15 Oct 2026"));
        assert_eq!(info.model.as_deref(), Some("Heltec"));
        assert_eq!(info.ver.as_deref(), Some("v1.18.0"));
        assert_eq!(info.repeat, Some(true));
        assert_eq!(info.path_hash_mode, Some(2));
    }

    #[test]
    fn sent_needs_the_whole_frame() {
        assert!(sent(&[1, 2, 3]).is_err());
        let info = sent(&[1, 0x78, 0x56, 0x34, 0x12, 0x88, 0x13, 0, 0]).unwrap();
        assert!(info.flood);
        assert_eq!(info.expected_ack, 0x1234_5678);
        assert_eq!(info.suggested_timeout, 5000);
    }

    #[test]
    fn signed_contact_message() {
        let mut data = vec![0x10, 0, 0];
        data.extend([1, 2, 3, 4, 5, 6, 0xff, 2]);
        data.extend(100u32.to_le_bytes());
        data.extend([9, 9, 9, 9]);
        data.extend(b"hello");
        let msg = contact_message(&data, true).unwrap();
        assert_eq!(msg.snr, Some(4.0));
        assert!(msg.path_len.is_none());
        assert_eq!(msg.signature, Some([9; 4]));
        assert_eq!(msg.text, "hello");
    }

    #[test]
    fn channel_data_widened_type() {
        let data = [0x08, 0, 0, 3, 0x41, 0x34, 0x12, 2, 0xaa, 0xbb];
        let d = channel_data(&data).unwrap();
        assert_eq!(d.snr, 2.0);
        assert_eq!(d.channel_idx, 3);
        assert_eq!(d.path_len.hops(), Some(1));
        assert_eq!(d.data_type, 0x1234);
        assert_eq!(d.payload, vec![0xaa, 0xbb]);
        assert!(channel_data(&[0; 5]).is_err());
    }

    #[test]
    fn battery_with_and_without_storage() {
        assert_eq!(battery(&[0xd4, 0x0d]).unwrap().used_kb, None);
        let mut data = vec![0xd4, 0x0d];
        data.extend(1024u32.to_le_bytes());
        data.extend(4096u32.to_le_bytes());
        let b = battery(&data).unwrap();
        assert_eq!(
            (b.level, b.used_kb, b.total_kb),
            (3540, Some(1024), Some(4096))
        );
        // a truncated storage part is ignored
        assert_eq!(battery(&data[..6]).unwrap().used_kb, None);
    }

    #[test]
    fn packet_stats_with_receive_errors() {
        let mut data = vec![2];
        for v in [10u32, 20, 3, 4, 5, 6, 7] {
            data.extend(v.to_le_bytes());
        }
        let Stats::Packets(p) = stats(&data).unwrap() else {
            panic!()
        };
        assert_eq!(p.recv_errors, Some(7));
        let Stats::Packets(p) = stats(&data[..25]).unwrap() else {
            panic!()
        };
        assert_eq!(p.recv_errors, None);
        assert!(stats(&[9]).is_err());
    }

    #[test]
    fn status_push_and_length_check() {
        let mut data = vec![0, 1, 2, 3, 4, 5, 6];
        data.extend(4100u16.to_le_bytes());
        data.extend([0; 50]);
        let s = status_push(&data).unwrap();
        assert_eq!(s.pubkey_prefix, [1, 2, 3, 4, 5, 6]);
        assert_eq!(s.bat, 4100);
        assert_eq!(s.recv_errors, None);
        assert!(status_push(&data[..40]).is_err());
    }

    #[test]
    fn login_frames_of_all_firmware_tiers() {
        let legacy = login_success(&[0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(legacy.pubkey_prefix, Some([1, 2, 3, 4, 5, 6]));
        assert_eq!(legacy.server_timestamp, None);
        let mut modern = vec![1, 1, 2, 3, 4, 5, 6];
        modern.extend(77u32.to_le_bytes());
        modern.extend([3, 2]);
        let modern = login_success(&modern);
        assert!(modern.is_admin());
        assert_eq!(modern.server_timestamp, Some(77));
        assert_eq!(modern.fw_ver_level, Some(2));
        // a prefix cut short: no field is read from its bytes
        let cut = login_success(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(cut.permissions, Some(1));
        assert_eq!(
            (cut.pubkey_prefix, cut.server_timestamp, cut.acl_permissions),
            (None, None, None)
        );
        assert_eq!(
            login_failed(&[0, 1, 2, 3, 4, 5, 6]).pubkey_prefix,
            Some([1, 2, 3, 4, 5, 6])
        );
    }

    #[test]
    fn trace_with_two_byte_hashes() {
        let mut data = vec![0, 4, 1];
        data.extend(7u32.to_le_bytes());
        data.extend(9u32.to_le_bytes());
        data.extend([0xaa, 0xab, 0xbb, 0xbc]);
        data.extend([8, 0xfc]);
        data.push(12);
        let t = trace(&data).unwrap();
        assert_eq!(t.tag, 7);
        assert_eq!(t.hops.len(), 2);
        assert_eq!(t.hops[1].hash, vec![0xbb, 0xbc]);
        assert_eq!(t.hops[1].snr, -1.0);
        assert_eq!(t.final_snr, Some(3.0));
        // a path cut short (here its final SNR) makes the trace unreadable
        assert!(trace(&data[..data.len() - 1]).is_err());
    }

    #[test]
    fn neighbours_have_the_prefix_length_the_repeater_sends() {
        let mut data = 1i16.to_le_bytes().to_vec();
        data.extend(1i16.to_le_bytes());
        data.extend([7; 32]);
        data.extend(30i32.to_le_bytes());
        data.push(8);
        // asked for 40 bytes, the repeater sends 32
        let answer = neighbours(&data, [1; 6], 5, 40).unwrap();
        assert_eq!(answer.pubkey_prefix_length, 40);
        assert_eq!(answer.neighbours[0].pubkey, vec![7; 32]);
        assert_eq!(answer.neighbours[0].secs_ago, 30);
        assert_eq!(answer.neighbours[0].snr, 2.0);
    }

    #[test]
    fn mma_series_start_after_the_clock() {
        let mut data = 1000u32.to_le_bytes().to_vec();
        data.extend([1, 103, 0, 200, 0, 255, 0, 221]);
        let answer = mma(&data, [1; 6], 9);
        assert_eq!(answer.timestamp, Some(1000));
        assert_eq!(answer.entries.len(), 1);
        // a clock cut short: no series either
        let answer = mma(&[1, 103], [1; 6], 9);
        assert_eq!((answer.timestamp, answer.entries.len()), (None, 0));
        assert_eq!(answer.undecoded, Vec::<u8>::new());
    }

    #[test]
    fn path_discovery_with_unknown_out_path() {
        let data = [0, 1, 2, 3, 4, 5, 6, 0xff, 0x41, 0xaa, 0xbb];
        let p = path_discovery(&data).unwrap();
        assert!(p.out_path_len.is_none());
        assert_eq!(p.in_path, vec![0xaa, 0xbb]);
        // cut inside a path
        assert!(path_discovery(&data[..10]).is_err());
    }

    #[test]
    fn node_discover_answer() {
        let mut data = vec![0x14, 0xa6, 0x00, 0x92, 0x10];
        data.extend(0xdead_beefu32.to_le_bytes());
        data.extend([7; 8]);
        let (control, discover) = control_data(&data).unwrap();
        assert_eq!(control.payload_type, 0x92);
        let d = discover.unwrap();
        assert_eq!(d.node_type, 2);
        assert_eq!(d.snr_in, 4.0);
        assert_eq!(d.tag, 0xdead_beef);
        assert_eq!(d.pubkey, vec![7; 8]);
        // an answer cut inside its tag still gives the control data
        let (control, discover) = control_data(&data[..7]).unwrap();
        assert_eq!(control.payload, vec![0x92, 0x10, 0xef, 0xbe]);
        assert!(discover.is_none());
    }

    #[test]
    fn default_flood_scope_ends_at_first_nul() {
        assert_eq!(default_flood_scope(&[]).unwrap(), None);
        let mut data = b"#ham\0rg".to_vec();
        data.resize(31, 0);
        data.extend([5; 16]);
        let scope = default_flood_scope(&data).unwrap().unwrap();
        assert_eq!(scope.name, "#ham");
        assert_eq!(scope.key, [5; 16]);
    }

    #[test]
    fn acl_and_neighbours() {
        let mut data = vec![1, 2, 3, 4, 5, 6, 3];
        data.extend([0; 7]);
        assert_eq!(
            acl(&data),
            vec![AclEntry {
                key: [1, 2, 3, 4, 5, 6],
                perm: 3
            }]
        );

        let mut data = Vec::new();
        data.extend(5i16.to_le_bytes());
        data.extend(1i16.to_le_bytes());
        data.extend([0xa1, 0xa2, 0xa3, 0xa4]);
        data.extend(30i32.to_le_bytes());
        data.push(0xf8);
        let n = neighbours(&data, [0; 6], 1, 4).unwrap();
        assert_eq!(n.neighbours_count, 5);
        assert_eq!(n.neighbours[0].secs_ago, 30);
        assert_eq!(n.neighbours[0].snr, -2.0);
    }

    #[test]
    fn repeat_frequency_pairs() {
        let mut data = Vec::new();
        for v in [433_000u32, 434_000, 869_000, 870_000] {
            data.extend(v.to_le_bytes());
        }
        assert_eq!(repeat_freqs(&data).len(), 2);
        data.extend([0; 8]);
        data.extend([1; 8]);
        assert_eq!(repeat_freqs(&data).len(), 2);
    }
}
