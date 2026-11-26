//! The path length byte and path helpers.

use std::fmt;

use crate::error::{Error, Result};

/// The largest path the firmware stores, in bytes (`MAX_PATH_SIZE`).
pub const MAX_PATH_SIZE: usize = 64;

/// The path length byte of contacts, messages and packets.
///
/// The low 6 bits are the hop count, the top 2 bits the hash mode: each hop is
/// identified by `hash_mode + 1` bytes of the node's key. `0xFF` means no path:
/// for a contact the path is unknown and messages to it are flooded, for a
/// received message it arrived by direct routing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PathLen(u8);

impl PathLen {
    /// No path (`0xFF`): flood for a contact, direct for a received message.
    pub const NONE: Self = Self(0xFF);

    /// A zero-hop path with 1-byte hashes.
    pub const ZERO_HOP: Self = Self(0);

    /// Wraps the byte as it is on the wire.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    /// Builds the byte from a hop count and a hash mode.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for more than 63 hops, a hash mode above 2
    /// (mode 3, 4-byte hashes, is reserved) or a path longer than
    /// [`MAX_PATH_SIZE`] bytes.
    pub fn new(hops: u8, hash_mode: u8) -> Result<Self> {
        if hops > 63 {
            return Err(Error::invalid(format!("{hops} hops, at most 63 fit")));
        }
        if hash_mode > 2 {
            return Err(Error::invalid(format!("hash mode {hash_mode}, at most 2")));
        }
        if usize::from(hops) * usize::from(hash_mode + 1) > MAX_PATH_SIZE {
            return Err(Error::invalid(format!(
                "{hops} hops of {} bytes exceed {MAX_PATH_SIZE} bytes",
                hash_mode + 1
            )));
        }
        Ok(Self(hops | (hash_mode << 6)))
    }

    /// The byte as it is on the wire.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    /// True for `0xFF`.
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0xFF
    }

    /// The hop count, or `None` for `0xFF`.
    #[must_use]
    pub const fn hops(self) -> Option<u8> {
        if self.is_none() {
            None
        } else {
            Some(self.0 & 0x3F)
        }
    }

    /// The hash mode (0-3), or `None` for `0xFF`.
    #[must_use]
    pub const fn hash_mode(self) -> Option<u8> {
        if self.is_none() {
            None
        } else {
            Some(self.0 >> 6)
        }
    }

    /// Bytes per hop (`hash_mode + 1`), or `None` for `0xFF`.
    #[must_use]
    pub const fn hash_size(self) -> Option<u8> {
        match self.hash_mode() {
            Some(mode) => Some(mode + 1),
            None => None,
        }
    }

    /// The number of path bytes this byte announces (0 for `0xFF`).
    #[must_use]
    pub const fn byte_len(self) -> usize {
        match (self.hops(), self.hash_size()) {
            (Some(hops), Some(size)) => hops as usize * size as usize,
            _ => 0,
        }
    }

    /// The hop count as meshcore_py reports it: hops, or -1 for `0xFF`.
    #[must_use]
    pub const fn hops_or_minus_one(self) -> i16 {
        match self.hops() {
            Some(hops) => hops as i16,
            None => -1,
        }
    }

    /// The hash mode as meshcore_py reports it: mode, or -1 for `0xFF`.
    #[must_use]
    pub const fn hash_mode_or_minus_one(self) -> i8 {
        match self.hash_mode() {
            Some(mode) => mode.cast_signed(),
            None => -1,
        }
    }
}

impl fmt::Debug for PathLen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.hops(), self.hash_size()) {
            (Some(hops), Some(size)) => write!(f, "PathLen({hops} hops x {size} B)"),
            _ => f.write_str("PathLen(none)"),
        }
    }
}

/// Splits a path into its hops.
#[must_use]
pub fn split_hops(path: &[u8], len: PathLen) -> Vec<&[u8]> {
    match len.hash_size() {
        Some(size) => path.chunks(usize::from(size)).collect(),
        None => Vec::new(),
    }
}

/// Parses a path as meshcore_py's `change_contact_path` takes it: hex, with an
/// optional `:mode` suffix giving the hash mode (`"a1b2c3"`, `"a1b2c3d4:1"`).
///
/// # Errors
///
/// [`Error::InvalidArgument`] for bad hex or a bad mode.
pub fn parse_path_hex(text: &str) -> Result<(Vec<u8>, Option<u8>)> {
    let (hex_part, mode) = match text.split_once(':') {
        Some((hex_part, mode)) => {
            let mode: u8 = mode
                .trim()
                .parse()
                .map_err(|_| Error::invalid(format!("bad hash mode in path {text:?}")))?;
            if mode > 2 {
                return Err(Error::invalid(format!("hash mode {mode}, at most 2")));
            }
            (hex_part, Some(mode))
        }
        None => (text, None),
    };
    let bytes = crate::hex::decode(hex_part)
        .ok_or_else(|| Error::invalid(format!("path {text:?} is not hex")))?;
    Ok((bytes, mode))
}

/// Encodes the reply path a server should use when answering an anonymous
/// request: the byte `hops | mode << 6`, then the hops of `out_path` in
/// reverse order (each multi-byte hash kept intact).
///
/// Mirrors meshcore_py's `encode_reply_path`: an unusable hash mode gives a
/// zero-hop path, hops are capped at what the path holds and what the
/// server's 64-byte buffer takes.
#[must_use]
pub fn encode_reply_path(out_path_len: PathLen, out_path: &[u8]) -> Vec<u8> {
    let hash_mode = out_path_len.hash_mode().unwrap_or(0);
    if hash_mode > 2 {
        log::warn!(
            target: "meshcore",
            "Unsupported out_path_hash_mode {hash_mode}; requesting a zero-hop reply path instead"
        );
        return vec![0];
    }
    let hash_size = usize::from(hash_mode) + 1;
    let mut hops = usize::from(out_path_len.hops().unwrap_or(0)).min(63);
    hops = hops.min(out_path.len() / hash_size);
    let max_hops = 63.min(MAX_PATH_SIZE / hash_size);
    if hops > max_hops {
        log::warn!(
            target: "meshcore",
            "Reply path of {hops} hops x {hash_size}B exceeds the {MAX_PATH_SIZE}B the server \
             can hold; truncating to {max_hops}"
        );
        hops = max_hops;
    }
    let mut out = Vec::with_capacity(1 + hops * hash_size);
    out.push(hops as u8 | (hash_mode << 6));
    for hop in (0..hops).rev() {
        out.extend_from_slice(&out_path[hop * hash_size..(hop + 1) * hash_size]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_path_byte() {
        let len = PathLen::from_byte(0x43);
        assert_eq!(len.hops(), Some(3));
        assert_eq!(len.hash_mode(), Some(1));
        assert_eq!(len.hash_size(), Some(2));
        assert_eq!(len.byte_len(), 6);
        assert!(PathLen::NONE.is_none());
        assert_eq!(PathLen::NONE.hops_or_minus_one(), -1);
        assert_eq!(PathLen::NONE.hash_mode_or_minus_one(), -1);
        assert_eq!(PathLen::NONE.byte_len(), 0);
    }

    #[test]
    fn builds_the_path_byte() {
        assert_eq!(PathLen::new(3, 1).unwrap().as_byte(), 0x43);
        assert!(PathLen::new(64, 0).is_err());
        assert!(PathLen::new(1, 3).is_err());
        assert!(PathLen::new(33, 1).is_err());
    }

    #[test]
    fn parses_path_strings() {
        assert_eq!(
            parse_path_hex("a1b2c3").unwrap(),
            (vec![0xa1, 0xb2, 0xc3], None)
        );
        assert_eq!(
            parse_path_hex("a1b2:1").unwrap(),
            (vec![0xa1, 0xb2], Some(1))
        );
        assert_eq!(parse_path_hex("").unwrap(), (Vec::new(), None));
        assert!(parse_path_hex("xyz").is_err());
        assert!(parse_path_hex("a1:3").is_err());
    }

    #[test]
    fn reply_path_reverses_by_hop() {
        let path = [0xa1, 0xa2, 0xb1, 0xb2];
        let len = PathLen::new(2, 1).unwrap();
        assert_eq!(
            encode_reply_path(len, &path),
            vec![0x42, 0xb1, 0xb2, 0xa1, 0xa2]
        );
        assert_eq!(encode_reply_path(PathLen::NONE, &[]), vec![0]);
        // the hop count never exceeds what the path holds
        assert_eq!(
            encode_reply_path(PathLen::new(3, 0).unwrap(), &[1]),
            vec![1, 1]
        );
        // a reserved hash mode asks for a zero-hop reply
        assert_eq!(
            encode_reply_path(PathLen::from_byte(0xc1), &[1, 2, 3, 4]),
            vec![0]
        );
        // nor what the server's 64 bytes take: 32 of 35 hops of 2 bytes
        let long = encode_reply_path(PathLen::from_byte(0x40 | 0x23), &[7; 70]);
        assert_eq!(long[0], 0x40 | 0x20);
        assert_eq!(long.len(), 1 + 64);
    }

    #[test]
    fn splits_hops() {
        let len = PathLen::new(2, 1).unwrap();
        assert_eq!(
            split_hops(&[1, 2, 3, 4], len),
            vec![&[1, 2][..], &[3, 4][..]]
        );
    }
}
