//! Parses mesh packets from the RX log and decrypts group texts of known
//! channels (meshcore_py's `MeshcorePacketParser`).

use aes::cipher::{BlockCipherDecrypt, KeyInit};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::protocol::bytes::{ByteReader, lossy_ignore};
use crate::types::contact::PublicKey;
use crate::types::device::ChannelInfo;
use crate::types::rxlog::{
    AdvertInfo, DecryptedGroupText, GroupText, PacketContent, PacketInfo, PayloadType, RouteType,
};

/// The channel log keeps at most this many group packets; when it grows past
/// it, the oldest quarter is dropped.
const CHANNEL_LOG_MAX: usize = 100;
const CHANNEL_LOG_TRIM: usize = 25;

/// A group packet seen in the RX log.
#[derive(Debug, Clone)]
pub(crate) struct LoggedGroupPacket {
    pub(crate) pkt_hash: u32,
    pub(crate) path: Vec<u8>,
    pub(crate) rssi: i8,
    pub(crate) snr: f32,
    pub(crate) recv_time: u64,
    pub(crate) decrypted: Option<DecryptedGroupText>,
}

/// Channels known from `CHANNEL_INFO` replies and `set_channel`, and recent
/// group packets.
#[derive(Debug)]
pub(crate) struct PacketParser {
    channels: Vec<Option<ChannelInfo>>,
    log: Vec<LoggedGroupPacket>,
    pub(crate) decrypt_channels: bool,
    keep_channel_secrets: bool,
}

impl Default for PacketParser {
    fn default() -> Self {
        Self {
            channels: vec![None; 40],
            log: Vec::new(),
            decrypt_channels: false,
            keep_channel_secrets: true,
        }
    }
}

impl PacketParser {
    pub(crate) const fn keep_channel_secrets(&self) -> bool {
        self.keep_channel_secrets
    }

    /// Turning it off drops the channels held and the channel log.
    pub(crate) fn set_keep_channel_secrets(&mut self, keep: bool) {
        self.keep_channel_secrets = keep;
        if !keep {
            self.channels = vec![None; 40];
            self.log.clear();
        }
    }

    /// Remembers a channel the device reported or was set to (when secrets
    /// are kept).
    pub(crate) fn add_channel(&mut self, channel: &ChannelInfo) {
        if !self.keep_channel_secrets {
            return;
        }
        let idx = usize::from(channel.channel_idx);
        if self.channels.len() <= idx {
            self.channels.resize(idx + 1, None);
        }
        self.channels[idx] = Some(channel.clone());
    }

    /// The logged packet whose decrypted message has this hash.
    pub(crate) fn find_by_msg_hash(&self, msg_hash: u32) -> Option<&LoggedGroupPacket> {
        self.log.iter().rev().find(|entry| {
            entry
                .decrypted
                .as_ref()
                .is_some_and(|d| d.msg_hash == msg_hash)
        })
    }

    fn find_by_pkt_hash(&self, pkt_hash: u32) -> Option<&LoggedGroupPacket> {
        self.log
            .iter()
            .rev()
            .find(|entry| entry.pkt_hash == pkt_hash)
    }

    /// Parses a received packet; `None` when it is too short to read.
    pub(crate) fn parse(
        &mut self,
        payload: &[u8],
        snr: f32,
        rssi: i8,
        recv_time: u64,
    ) -> Option<PacketInfo> {
        let mut reader = ByteReader::new(payload);
        let header = reader.u8().ok()?;
        // A packet needs at least its header and path byte.
        if payload.len() < 2 {
            return None;
        }
        let route_type = RouteType::from_bits(header);
        let payload_type = PayloadType::from_bits(header >> 2);
        let payload_ver = header >> 6;
        let transport_code = if route_type.has_transport_codes() {
            Some(reader.array::<4>().ok()?)
        } else {
            None
        };
        let path_byte = reader.u8().ok()?;
        let path_hash_size = (path_byte >> 6) + 1;
        let path_len = path_byte & 0x3F;
        let path = reader
            .bytes(usize::from(path_len) * usize::from(path_hash_size))
            .ok()?
            .to_vec();
        let pkt_payload = reader.rest().to_vec();
        let pkt_hash = hash4(&[&pkt_payload]);

        let content = match payload_type {
            PayloadType::GrpTxt => {
                let group = self.parse_group_text(&pkt_payload, pkt_hash);
                if let Some(group) = &group {
                    self.log_group_packet(LoggedGroupPacket {
                        pkt_hash,
                        path: path.clone(),
                        rssi,
                        snr,
                        recv_time,
                        decrypted: group.decrypted.clone(),
                    });
                }
                group.map_or(PacketContent::None, PacketContent::GroupText)
            }
            PayloadType::Advert => {
                parse_advert(&pkt_payload).map_or(PacketContent::None, PacketContent::Advert)
            }
            _ => PacketContent::None,
        };

        Some(PacketInfo {
            header,
            route_type,
            payload_type,
            payload_ver,
            transport_code,
            path_len,
            path_hash_size,
            path,
            pkt_payload,
            pkt_hash,
            content,
        })
    }

    fn parse_group_text(&self, pkt_payload: &[u8], pkt_hash: u32) -> Option<GroupText> {
        let mut reader = ByteReader::new(pkt_payload);
        let chan_hash = reader.u8().ok()?;
        let cipher_mac = reader.array::<2>().ok()?;
        let crypted = reader.rest().to_vec();

        let channel = self.channels.iter().flatten().find(|channel| {
            channel.channel_hash() == chan_hash
                && mac2(&channel.channel_secret, &crypted) == cipher_mac
        });
        let chan_name = channel.map(|c| c.channel_name.clone());
        let decrypted = match channel {
            Some(channel) if self.decrypt_channels => {
                match self
                    .find_by_pkt_hash(pkt_hash)
                    .and_then(|l| l.decrypted.clone())
                {
                    // a repeat of a packet already decrypted
                    Some(decrypted) => Some(decrypted),
                    // new, or logged while decryption was off
                    None => decrypt_group_text(&channel.channel_secret, &crypted),
                }
            }
            _ => None,
        };
        Some(GroupText {
            chan_hash,
            cipher_mac,
            crypted,
            chan_name,
            decrypted,
        })
    }

    fn log_group_packet(&mut self, entry: LoggedGroupPacket) {
        self.log.push(entry);
        if self.log.len() > CHANNEL_LOG_MAX {
            self.log.drain(..CHANNEL_LOG_TRIM);
        }
    }
}

/// The first 4 bytes (LE) of the SHA-256 of the concatenated parts.
pub(crate) fn hash4(parts: &[&[u8]]) -> u32 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    u32::from_le_bytes([digest[0], digest[1], digest[2], digest[3]])
}

/// The hash meshcore_py matches channel messages to logged packets with:
/// SHA-256 of the timestamp (LE) and the text.
pub(crate) fn message_hash(sender_timestamp: u32, text: &[u8]) -> u32 {
    hash4(&[&sender_timestamp.to_le_bytes(), text])
}

/// The first two bytes of HMAC-SHA256 over the encrypted message.
fn mac2(secret: &[u8; 16], data: &[u8]) -> [u8; 2] {
    let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(secret) else {
        return [0, 0];
    };
    mac.update(data);
    let tag = mac.finalize().into_bytes();
    [tag[0], tag[1]]
}

/// Decrypts a group text with AES-128-ECB: timestamp (4), flags (txt type in
/// the top 6 bits, attempt in the low 2), then the text, NUL padded.
fn decrypt_group_text(secret: &[u8; 16], crypted: &[u8]) -> Option<DecryptedGroupText> {
    let (blocks, rest) = crypted.as_chunks::<16>();
    if blocks.is_empty() || !rest.is_empty() {
        return None;
    }
    let cipher = aes::Aes128::new_from_slice(secret).ok()?;
    let mut plain = Vec::with_capacity(crypted.len());
    for chunk in blocks {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        plain.extend_from_slice(&block);
    }
    let sender_timestamp = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    let flags = plain[4];
    let text = trim_nul(&plain[5..]);
    Some(DecryptedGroupText {
        message: lossy_ignore(text),
        msg_hash: message_hash(sender_timestamp, text),
        sender_timestamp,
        attempt: flags & 3,
        txt_type: flags >> 2,
    })
}

/// Strips NUL bytes at both ends (Python's `bytes.strip(b"\0")`).
pub(crate) fn trim_nul(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|&b| b != 0).map_or(start, |p| p + 1);
    &bytes[start..end]
}

fn parse_advert(pkt_payload: &[u8]) -> Option<AdvertInfo> {
    let mut reader = ByteReader::new(pkt_payload);
    let adv_key = PublicKey::new(reader.array::<32>().ok()?);
    let adv_timestamp = reader.u32().ok()?;
    let signature = reader.bytes(64).ok()?.to_vec();
    let adv_flags = reader.u8().ok()?;
    let mut advert = AdvertInfo {
        adv_key,
        adv_timestamp,
        signature,
        adv_flags,
        adv_type: adv_flags & 0x0F,
        adv_lat: None,
        adv_lon: None,
        adv_feat1: None,
        adv_feat2: None,
        adv_name: None,
    };
    if adv_flags & 0x10 != 0 {
        advert.adv_lat = Some(f64::from(reader.i32().ok()?) / 1e6);
        advert.adv_lon = Some(f64::from(reader.i32().ok()?) / 1e6);
    }
    if adv_flags & 0x20 != 0 {
        advert.adv_feat1 = Some(reader.array::<2>().ok()?);
    }
    if adv_flags & 0x40 != 0 {
        advert.adv_feat2 = Some(reader.array::<2>().ok()?);
    }
    if adv_flags & 0x80 != 0 {
        advert.adv_name = Some(lossy_ignore(trim_nul(reader.rest())));
    }
    Some(advert)
}

#[cfg(test)]
mod tests {
    use aes::cipher::BlockCipherEncrypt;

    use super::*;

    fn channel(idx: u8, name: &str) -> ChannelInfo {
        ChannelInfo {
            channel_idx: idx,
            channel_name: name.to_string(),
            channel_secret: crate::types::device::channel_secret_from_name(name),
        }
    }

    /// Builds a flood group text packet as a companion sends it.
    fn group_packet(channel: &ChannelInfo, timestamp: u32, text: &str, path: &[u8]) -> Vec<u8> {
        let mut plain = timestamp.to_le_bytes().to_vec();
        plain.push(0); // plain text, attempt 0
        plain.extend_from_slice(text.as_bytes());
        while !plain.len().is_multiple_of(16) {
            plain.push(0);
        }
        let cipher = aes::Aes128::new_from_slice(&channel.channel_secret).unwrap();
        let mut crypted = Vec::new();
        for chunk in plain.as_chunks::<16>().0 {
            let mut block = aes::Block::default();
            block.copy_from_slice(chunk);
            cipher.encrypt_block(&mut block);
            crypted.extend_from_slice(&block);
        }
        let mut packet = vec![(5 << 2) | 1, u8::try_from(path.len()).unwrap()];
        packet.extend_from_slice(path);
        packet.push(channel.channel_hash());
        packet.extend_from_slice(&mac2(&channel.channel_secret, &crypted));
        packet.extend(crypted);
        packet
    }

    #[test]
    fn names_and_decrypts_group_texts_of_known_channels() {
        let public = channel(0, "#public");
        let packet = group_packet(&public, 1_700_000_000, "alice: hi", &[0xaa, 0xbb]);

        let mut parser = PacketParser::default();
        let info = parser.parse(&packet, 5.0, -90, 1).unwrap();
        assert_eq!(info.route_type, RouteType::Flood);
        assert_eq!(info.payload_type, PayloadType::GrpTxt);
        assert_eq!(info.path, vec![0xaa, 0xbb]);
        let PacketContent::GroupText(group) = &info.content else {
            panic!()
        };
        assert_eq!(group.chan_name, None);

        parser.add_channel(&public);
        let info = parser.parse(&packet, 5.0, -90, 1).unwrap();
        let PacketContent::GroupText(group) = &info.content else {
            panic!()
        };
        assert_eq!(group.chan_name.as_deref(), Some("#public"));
        assert!(group.decrypted.is_none());

        parser.decrypt_channels = true;
        let info = parser.parse(&packet, 5.0, -90, 1).unwrap();
        let PacketContent::GroupText(group) = &info.content else {
            panic!()
        };
        let decrypted = group.decrypted.as_ref().unwrap();
        assert_eq!(decrypted.message, "alice: hi");
        assert_eq!(decrypted.sender_timestamp, 1_700_000_000);
        let hash = message_hash(1_700_000_000, b"alice: hi");
        assert_eq!(decrypted.msg_hash, hash);
        assert_eq!(
            parser.find_by_msg_hash(hash).unwrap().path,
            vec![0xaa, 0xbb]
        );
    }

    #[test]
    fn forgetting_secrets_drops_channels_and_log() {
        let public = channel(0, "#public");
        let mut parser = PacketParser::default();
        parser.add_channel(&public);
        parser.set_keep_channel_secrets(false);
        parser.add_channel(&public);
        let packet = group_packet(&public, 1, "x", &[]);
        let info = parser.parse(&packet, 0.0, 0, 0).unwrap();
        let PacketContent::GroupText(group) = &info.content else {
            panic!()
        };
        assert_eq!(group.chan_name, None);
    }

    #[test]
    fn short_packets_are_not_parsed() {
        let mut parser = PacketParser::default();
        assert!(parser.parse(&[0x15], 0.0, 0, 0).is_none());
        // transport route ending before its path byte
        assert!(parser.parse(&[0x14, 1, 2, 3, 4], 0.0, 0, 0).is_none());
        // a path longer than the packet
        assert!(parser.parse(&[0x11, 0x03, 0xaa], 0.0, 0, 0).is_none());
    }

    #[test]
    fn parses_adverts() {
        let mut payload = vec![7u8; 32];
        payload.extend(1000u32.to_le_bytes());
        payload.extend([0u8; 64]);
        payload.push(0x80 | 0x10 | 2);
        payload.extend(53_551_100i32.to_le_bytes());
        payload.extend(9_993_700i32.to_le_bytes());
        payload.extend(b"Rep1");
        let mut packet = vec![(4 << 2) | 1, 0];
        packet.extend(payload);
        let info = PacketParser::default().parse(&packet, 0.0, 0, 0).unwrap();
        let PacketContent::Advert(advert) = info.content else {
            panic!()
        };
        assert_eq!(advert.adv_type, 2);
        assert_eq!(advert.adv_name.as_deref(), Some("Rep1"));
        assert_eq!(advert.adv_lat, Some(53.5511));
    }
}
