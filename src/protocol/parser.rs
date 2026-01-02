//! Binary data parsing utilities for the `MeshCore` protocol.
//!
//! This module provides functions to parse binary data from device responses.

use bytes::Buf;

use crate::error::{Error, Result};
use crate::types::{
    BatteryStatus, Channel, Contact, ContactFlags, ContactMessage, ContactType, DeviceInfo,
    DeviceStatus, PublicKey, RadioConfig, SelfInfo, SignalQuality, TelemetryMode, TextType,
};

/// Coordinate scaling factor (multiply by 1e6 for storage).
const COORD_SCALE: f64 = 1_000_000.0;

/// SNR scaling factor (raw value is multiplied by 4 in protocol).
const SNR_SCALE: f32 = 4.0;

/// Parses a null-terminated or fixed-length string from bytes.
fn parse_string(data: &[u8], max_len: usize) -> String {
    let len = data
        .iter()
        .take(max_len)
        .position(|&b| b == 0)
        .unwrap_or_else(|| max_len.min(data.len()));
    String::from_utf8_lossy(&data[..len]).into_owned()
}

/// Parses coordinates from signed i32 (scaled by 1e6).
///
/// Returns `None` if value is 0, as the protocol uses 0 as a sentinel
/// meaning "no coordinate set". This means (0.0, 0.0) cannot be represented.
fn parse_coord(value: i32) -> Option<f64> {
    if value == 0 {
        None
    } else {
        Some(f64::from(value) / COORD_SCALE)
    }
}

/// Parses `SelfInfo` from device response.
///
/// Format:
/// ```text
/// [adv_type:1] [tx_power:1] [max_tx_power:1] [pubkey:32] [lat:4LE] [lon:4LE]
/// [multi_acks:1] [adv_loc_policy:1] [telemetry_mode:1] [manual_add:1]
/// [freq:4LE] [bw:4LE] [sf:1] [cr:1] [name:...]
/// ```
pub fn parse_self_info(data: &[u8]) -> Result<SelfInfo> {
    if data.len() < 52 {
        return Err(Error::Protocol {
            message: format!("SelfInfo too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let advert_type = cursor.get_u8();
    let tx_power = cursor.get_u8();
    let max_tx_power = cursor.get_u8();

    let mut pubkey_bytes = [0u8; 32];
    cursor.copy_to_slice(&mut pubkey_bytes);
    let public_key = PublicKey::from_bytes(&pubkey_bytes);

    let lat_raw = cursor.get_i32_le();
    let lon_raw = cursor.get_i32_le();

    let multi_acks = cursor.get_u8();
    let advert_loc_policy = cursor.get_u8();
    let telemetry_byte = cursor.get_u8();
    let manual_add = cursor.get_u8();

    let freq_raw = cursor.get_u32_le();
    let bw_raw = cursor.get_u32_le();
    let sf = cursor.get_u8();
    let cr = cursor.get_u8();

    let name_start = cursor.position() as usize;
    let name = parse_string(&data[name_start..], 32);

    Ok(SelfInfo {
        advert_type,
        tx_power,
        max_tx_power,
        public_key,
        latitude: parse_coord(lat_raw),
        longitude: parse_coord(lon_raw),
        multi_acks,
        advert_loc_policy,
        telemetry_mode: TelemetryMode::from_byte(telemetry_byte),
        manual_add_contacts: manual_add != 0,
        radio: RadioConfig {
            frequency_mhz: f64::from(freq_raw) / 1000.0,
            bandwidth_khz: f64::from(bw_raw) / 1000.0,
            spreading_factor: sf,
            coding_rate: cr,
        },
        name,
    })
}

/// Parses `DeviceInfo` from device response.
///
/// Format:
/// ```text
/// [fw_ver:1] (if >= 3: [max_contacts:1*2] [max_channels:1] [ble_pin:4LE]
/// [build:12] [model:40] [ver:20])
/// ```
pub fn parse_device_info(data: &[u8]) -> Result<DeviceInfo> {
    if data.is_empty() {
        return Err(Error::Protocol {
            message: "DeviceInfo empty".into(),
        });
    }

    let firmware_version = data[0];

    if firmware_version >= 3 && data.len() >= 79 {
        let mut cursor = std::io::Cursor::new(&data[1..]);

        let max_contacts_raw = cursor.get_u8();
        let max_contacts = u16::from(max_contacts_raw) * 2;
        let max_channels = cursor.get_u8();
        let ble_pin = cursor.get_u32_le();

        let build = parse_string(&data[7..19], 12);
        let model = parse_string(&data[19..59], 40);
        let version = parse_string(&data[59..79], 20);

        Ok(DeviceInfo {
            firmware_version,
            max_contacts: Some(max_contacts),
            max_channels: Some(max_channels),
            ble_pin: Some(ble_pin),
            build: Some(build),
            model: Some(model),
            version: Some(version),
        })
    } else {
        Ok(DeviceInfo {
            firmware_version,
            max_contacts: None,
            max_channels: None,
            ble_pin: None,
            build: None,
            model: None,
            version: None,
        })
    }
}

/// Parses `Contact` from device response.
///
/// Format:
/// ```text
/// [pubkey:32] [type:1] [flags:1] [path_len:1signed] [path:64]
/// [name:32] [last_advert:4LE] [lat:4LE] [lon:4LE] [lastmod:4LE]
/// ```
pub fn parse_contact(data: &[u8]) -> Result<Contact> {
    // Minimum size: 32 + 1 + 1 + 1 + 64 + 32 + 4 + 4 + 4 + 4 = 147 bytes
    if data.len() < 147 {
        return Err(Error::Protocol {
            message: format!("Contact too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let mut pubkey_bytes = [0u8; 32];
    cursor.copy_to_slice(&mut pubkey_bytes);
    let public_key = PublicKey::from_bytes(&pubkey_bytes);

    let device_type = ContactType::from_byte(cursor.get_u8());
    let flags = ContactFlags::from_byte(cursor.get_u8());
    let out_path_len = cursor.get_i8();

    let mut path_bytes = [0u8; 64];
    cursor.copy_to_slice(&mut path_bytes);
    let path_len = usize::try_from(out_path_len).unwrap_or(0).min(64);
    let out_path = bytes::Bytes::copy_from_slice(&path_bytes[..path_len]);

    // Name is at offset 99 (32+1+1+1+64)
    let name = parse_string(&data[99..131], 32);

    cursor.set_position(131);
    let last_advert = cursor.get_u32_le();
    let lat_raw = cursor.get_i32_le();
    let lon_raw = cursor.get_i32_le();
    let last_modified = cursor.get_u32_le();

    Ok(Contact {
        public_key,
        device_type,
        flags,
        out_path_len,
        out_path,
        name,
        last_advert,
        latitude: parse_coord(lat_raw),
        longitude: parse_coord(lon_raw),
        last_modified,
    })
}

/// Parses `ContactMessage` from device response.
///
/// Format (v1):
/// ```text
/// [pubkey_prefix:6] [path_len:1] [txt_type:1] [timestamp:4LE]
/// (if txt_type==2: [signature:4]) [text...]
/// ```
///
/// Format (v3, with signal):
/// ```text
/// [snr:1] [reserved:2] [pubkey_prefix:6] [path_len:1] [txt_type:1] [timestamp:4LE]
/// (if txt_type==2: [signature:4]) [text...]
/// ```
pub fn parse_contact_message(data: &[u8], v3: bool) -> Result<ContactMessage> {
    let min_len = if v3 { 15 } else { 12 };
    if data.len() < min_len {
        return Err(Error::Protocol {
            message: format!("ContactMessage too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let signal = if v3 {
        let snr_raw = cursor.get_i8();
        // Skip 2 reserved bytes (always 0x00)
        cursor.advance(2);
        Some(SignalQuality {
            snr: f32::from(snr_raw) / SNR_SCALE,
        })
    } else {
        None
    };

    let mut sender_prefix = [0u8; 6];
    cursor.copy_to_slice(&mut sender_prefix);

    let path_len = cursor.get_i8();
    let txt_type_byte = cursor.get_u8();
    let text_type = TextType::from_byte(txt_type_byte);
    let timestamp = cursor.get_u32_le();

    let text_start = cursor.position() as usize;
    // Signed messages have a 4-byte signature prefix before the text
    let (signature, text) = if text_type == TextType::Signed && data.len() > text_start + 4 {
        let sig = data[text_start..text_start + 4].to_vec();
        let txt = String::from_utf8_lossy(&data[text_start + 4..]).into_owned();
        (Some(sig), txt)
    } else {
        (
            None,
            String::from_utf8_lossy(&data[text_start..]).into_owned(),
        )
    };

    Ok(ContactMessage {
        sender_prefix,
        path_len,
        text_type,
        timestamp,
        signature,
        text,
        signal,
    })
}

/// Parses `ChannelMessage` from device response.
///
/// Format (v1):
/// ```text
/// [channel_idx:1] [path_len:1] [txt_type:1] [timestamp:4LE] [text...]
/// ```
///
/// Format (v3, with signal):
/// ```text
/// [snr:1] [reserved:2] [channel_idx:1] [path_len:1] [txt_type:1] [timestamp:4LE] [text...]
/// ```
pub fn parse_channel_message(data: &[u8], v3: bool) -> Result<crate::types::ChannelMessage> {
    let min_len = if v3 { 10 } else { 7 };
    if data.len() < min_len {
        return Err(Error::Protocol {
            message: format!("ChannelMessage too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let signal = if v3 {
        let snr_raw = cursor.get_i8();
        // Skip 2 reserved bytes (always 0x00)
        cursor.advance(2);
        Some(SignalQuality {
            snr: f32::from(snr_raw) / SNR_SCALE,
        })
    } else {
        None
    };

    let channel_index = cursor.get_u8();
    let path_len = cursor.get_i8();
    let txt_type_byte = cursor.get_u8();
    let text_type = TextType::from_byte(txt_type_byte);
    let timestamp = cursor.get_u32_le();

    let text_start = cursor.position() as usize;
    let text = String::from_utf8_lossy(&data[text_start..]).into_owned();

    Ok(crate::types::ChannelMessage {
        channel_index,
        path_len,
        text_type,
        timestamp,
        text,
        signal,
    })
}

/// Parses `BatteryStatus` from device response.
///
/// Format:
/// ```text
/// [millivolts:2LE] (if len > 3: [used_kb:4LE] [total_kb:4LE])
/// ```
pub fn parse_battery(data: &[u8]) -> Result<BatteryStatus> {
    if data.len() < 2 {
        return Err(Error::Protocol {
            message: "Battery data too short".into(),
        });
    }

    let millivolts = u16::from_le_bytes([data[0], data[1]]);

    // Storage info is optional (only present if len > 3)
    let (used_kb, total_kb) = if data.len() >= 10 {
        let used = u32::from_le_bytes([data[2], data[3], data[4], data[5]]);
        let total = u32::from_le_bytes([data[6], data[7], data[8], data[9]]);
        (Some(used), Some(total))
    } else {
        (None, None)
    };

    Ok(BatteryStatus {
        millivolts,
        used_kb,
        total_kb,
    })
}

/// Parses `Channel` info from device response.
///
/// Format:
/// ```text
/// [index:1] [name:32] [secret:16]
/// ```
pub fn parse_channel(data: &[u8]) -> Result<Channel> {
    if data.len() < 49 {
        return Err(Error::Protocol {
            message: format!("Channel too short: {} bytes", data.len()),
        });
    }

    let index = data[0];
    let name = parse_string(&data[1..33], 32);

    let mut secret = [0u8; 16];
    secret.copy_from_slice(&data[33..49]);

    Ok(Channel {
        index,
        name,
        secret,
    })
}

/// Parses `DeviceStatus` from status response.
///
/// Format:
/// ```text
/// [pubkey_prefix:6] [battery:2LE] [tx_queue:2LE] [noise_floor:2LESigned]
/// [last_rssi:2LESigned] [nb_recv:4LE] [nb_sent:4LE] [airtime:4LE] [uptime:4LE]
/// [sent_flood:4LE] [sent_direct:4LE] [recv_flood:4LE] [recv_direct:4LE]
/// [full_evts:2LE] [last_snr:2LESigned/4] [direct_dups:2LE] [flood_dups:2LE]
/// [rx_airtime:4LE]
/// ```
pub fn parse_device_status(data: &[u8]) -> Result<DeviceStatus> {
    if data.len() < 58 {
        return Err(Error::Protocol {
            message: format!("DeviceStatus too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let mut pubkey_prefix = [0u8; 6];
    cursor.copy_to_slice(&mut pubkey_prefix);

    let battery_mv = cursor.get_u16_le();
    let tx_queue_len = cursor.get_u16_le();
    let noise_floor = cursor.get_i16_le();
    let last_rssi = cursor.get_i16_le();
    let packets_received = cursor.get_u32_le();
    let packets_sent = cursor.get_u32_le();
    let airtime_secs = cursor.get_u32_le();
    let uptime_secs = cursor.get_u32_le();
    let sent_flood = cursor.get_u32_le();
    let sent_direct = cursor.get_u32_le();
    let recv_flood = cursor.get_u32_le();
    let recv_direct = cursor.get_u32_le();
    let full_events = cursor.get_u16_le();
    let snr_raw = cursor.get_i16_le();
    let last_snr = f32::from(snr_raw) / SNR_SCALE;
    let direct_dups = cursor.get_u16_le();
    let flood_dups = cursor.get_u16_le();
    let rx_airtime_secs = cursor.get_u32_le();

    Ok(DeviceStatus {
        pubkey_prefix,
        battery_mv,
        tx_queue_len,
        noise_floor,
        last_rssi,
        packets_received,
        packets_sent,
        airtime_secs,
        uptime_secs,
        sent_flood,
        sent_direct,
        recv_flood,
        recv_direct,
        full_events,
        last_snr,
        direct_dups,
        flood_dups,
        rx_airtime_secs,
    })
}

/// Parses core statistics.
///
/// Format:
/// ```text
/// [battery_mv:2LE] [uptime_secs:4LE] [errors:2LE] [queue_len:1]
/// ```
pub fn parse_core_stats(data: &[u8]) -> Result<crate::types::CoreStats> {
    if data.len() < 9 {
        return Err(Error::Protocol {
            message: format!("CoreStats too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let battery_mv = cursor.get_u16_le();
    let uptime_secs = cursor.get_u32_le();
    let errors = cursor.get_u16_le();
    let queue_len = cursor.get_u8();

    Ok(crate::types::CoreStats {
        battery_mv,
        uptime_secs,
        errors,
        queue_len,
    })
}

/// Parses radio statistics.
///
/// Format:
/// ```text
/// [noise_floor:2LESigned] [rssi:1Signed] [snr:1Signed/4]
/// [tx_airtime:4LE] [rx_airtime:4LE]
/// ```
pub fn parse_radio_stats(data: &[u8]) -> Result<crate::types::RadioStats> {
    if data.len() < 12 {
        return Err(Error::Protocol {
            message: format!("RadioStats too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let noise_floor = cursor.get_i16_le();
    let rssi = cursor.get_i8();
    let snr_raw = cursor.get_i8();
    let snr = f32::from(snr_raw) / SNR_SCALE;
    let tx_airtime_secs = cursor.get_u32_le();
    let rx_airtime_secs = cursor.get_u32_le();

    Ok(crate::types::RadioStats {
        noise_floor,
        rssi,
        snr,
        tx_airtime_secs,
        rx_airtime_secs,
    })
}

/// Parses packet statistics.
///
/// Format:
/// ```text
/// [received:4LE] [sent:4LE] [flood_tx:4LE] [direct_tx:4LE]
/// [flood_rx:4LE] [direct_rx:4LE]
/// ```
pub fn parse_packet_stats(data: &[u8]) -> Result<crate::types::PacketStats> {
    if data.len() < 24 {
        return Err(Error::Protocol {
            message: format!("PacketStats too short: {} bytes", data.len()),
        });
    }

    let mut cursor = std::io::Cursor::new(data);

    let received = cursor.get_u32_le();
    let sent = cursor.get_u32_le();
    let flood_tx = cursor.get_u32_le();
    let direct_tx = cursor.get_u32_le();
    let flood_rx = cursor.get_u32_le();
    let direct_rx = cursor.get_u32_le();

    Ok(crate::types::PacketStats {
        received,
        sent,
        flood_tx,
        direct_tx,
        flood_rx,
        direct_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_string() {
        assert_eq!(parse_string(b"hello\0world", 11), "hello");
        assert_eq!(parse_string(b"hello", 5), "hello");
        assert_eq!(parse_string(b"hello", 3), "hel");
    }

    #[test]
    fn test_parse_coord() {
        assert_eq!(parse_coord(0), None);
        assert!((parse_coord(51_500_000).unwrap() - 51.5).abs() < 0.0001);
        assert!((parse_coord(-1_278_000).unwrap() - (-1.278)).abs() < 0.0001);
    }

    #[test]
    fn test_parse_battery() {
        // 3540mV with storage info: 1024 KB used, 4096 KB total
        let mut data = vec![0xD4, 0x0D]; // 3540mV
        data.extend_from_slice(&1024u32.to_le_bytes()); // used_kb
        data.extend_from_slice(&4096u32.to_le_bytes()); // total_kb
        let battery = parse_battery(&data).unwrap();
        assert_eq!(battery.millivolts, 3540);
        assert_eq!(battery.used_kb, Some(1024));
        assert_eq!(battery.total_kb, Some(4096));
    }

    #[test]
    fn test_parse_battery_no_storage() {
        let data = [0xD4, 0x0D]; // 3540mV, no storage info
        let battery = parse_battery(&data).unwrap();
        assert_eq!(battery.millivolts, 3540);
        assert_eq!(battery.used_kb, None);
        assert_eq!(battery.total_kb, None);
    }

    #[test]
    fn test_parse_channel() {
        let mut data = vec![0u8; 49];
        data[0] = 1; // index
        data[1..7].copy_from_slice(b"Public");
        data[33..49].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);

        let channel = parse_channel(&data).unwrap();
        assert_eq!(channel.index, 1);
        assert_eq!(channel.name, "Public");
        assert_eq!(channel.secret[0], 1);
        assert_eq!(channel.secret[15], 16);
    }

    #[test]
    fn test_parse_channel_message() {
        // Build a channel message: channel_idx, path_len, txt_type, timestamp (4 bytes), text
        // Format: [channel_idx:1] [path_len:1] [txt_type:1] [timestamp:4LE] [text...]
        let mut data = Vec::new();
        data.push(2); // channel index
        data.push(0); // path_len
        data.push(0); // txt_type (plain)
        data.extend_from_slice(&1_234_567_890_u32.to_le_bytes()); // timestamp
        data.extend_from_slice(b"Hello"); // text

        let msg = parse_channel_message(&data, false).unwrap();
        assert_eq!(msg.channel_index, 2);
        assert_eq!(msg.timestamp, 1_234_567_890);
        assert_eq!(msg.text, "Hello");
    }

    #[test]
    fn test_parse_core_stats() {
        // Format: [battery_mv:2LE] [uptime_secs:4LE] [errors:2LE] [queue_len:1]
        let mut data = vec![0u8; 9];
        data[0..2].copy_from_slice(&4200u16.to_le_bytes()); // battery_mv
        data[2..6].copy_from_slice(&3600u32.to_le_bytes()); // uptime
        data[6..8].copy_from_slice(&5u16.to_le_bytes()); // errors
        data[8] = 10; // queue_len

        let stats = parse_core_stats(&data).unwrap();
        assert_eq!(stats.battery_mv, 4200);
        assert_eq!(stats.uptime_secs, 3600);
        assert_eq!(stats.errors, 5);
        assert_eq!(stats.queue_len, 10);
    }

    #[test]
    fn test_parse_radio_stats() {
        let mut data = vec![0u8; 12];
        data[0..2].copy_from_slice(&(-100i16).to_le_bytes()); // noise_floor
        data[2] = (-80i8).to_ne_bytes()[0]; // rssi (1 byte signed)
        data[3] = 40u8; // snr * 4 = 10.0 (1 byte signed)
        data[4..8].copy_from_slice(&1000u32.to_le_bytes()); // tx_airtime
        data[8..12].copy_from_slice(&2000u32.to_le_bytes()); // rx_airtime

        let stats = parse_radio_stats(&data).unwrap();
        assert_eq!(stats.noise_floor, -100);
        assert_eq!(stats.rssi, -80);
        assert!((stats.snr - 10.0).abs() < 0.01);
        assert_eq!(stats.tx_airtime_secs, 1000);
        assert_eq!(stats.rx_airtime_secs, 2000);
    }

    #[test]
    fn test_parse_packet_stats() {
        let mut data = vec![0u8; 24];
        data[0..4].copy_from_slice(&100u32.to_le_bytes()); // received
        data[4..8].copy_from_slice(&50u32.to_le_bytes()); // sent
        data[8..12].copy_from_slice(&20u32.to_le_bytes()); // flood_tx
        data[12..16].copy_from_slice(&30u32.to_le_bytes()); // direct_tx
        data[16..20].copy_from_slice(&40u32.to_le_bytes()); // flood_rx
        data[20..24].copy_from_slice(&60u32.to_le_bytes()); // direct_rx

        let stats = parse_packet_stats(&data).unwrap();
        assert_eq!(stats.received, 100);
        assert_eq!(stats.sent, 50);
        assert_eq!(stats.flood_tx, 20);
        assert_eq!(stats.direct_tx, 30);
        assert_eq!(stats.flood_rx, 40);
        assert_eq!(stats.direct_rx, 60);
    }
}
