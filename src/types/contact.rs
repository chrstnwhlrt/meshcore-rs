//! Contact data structures.

use bytes::Bytes;

/// Length of a public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length of a public key prefix used in messages.
pub const PUBLIC_KEY_PREFIX_LEN: usize = 6;

/// Maximum path length in bytes.
pub const MAX_PATH_LEN: usize = 64;

/// Maximum name length in bytes.
pub const MAX_NAME_LEN: usize = 32;

/// A 32-byte public key identifying a contact or device.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    /// Creates a new public key from bytes.
    ///
    /// # Panics
    ///
    /// Panics if the slice is not exactly 32 bytes.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut key = [0u8; PUBLIC_KEY_LEN];
        key.copy_from_slice(bytes);
        Self(key)
    }

    /// Tries to create a public key from bytes.
    ///
    /// Returns `None` if the slice is not exactly 32 bytes.
    #[must_use]
    pub fn try_from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != PUBLIC_KEY_LEN {
            return None;
        }
        let mut key = [0u8; PUBLIC_KEY_LEN];
        key.copy_from_slice(bytes);
        Some(Self(key))
    }

    /// Returns the 6-byte prefix used in message addressing.
    #[must_use]
    pub fn prefix(&self) -> [u8; PUBLIC_KEY_PREFIX_LEN] {
        let mut prefix = [0u8; PUBLIC_KEY_PREFIX_LEN];
        prefix.copy_from_slice(&self.0[..PUBLIC_KEY_PREFIX_LEN]);
        prefix
    }

    /// Returns the key as a byte slice.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Returns the key as a hex string.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parses a public key from a hex string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not valid hex or not 64 characters.
    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let bytes = hex::decode(s)?;
        if bytes.len() != PUBLIC_KEY_LEN {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        Ok(Self::from_bytes(&bytes))
    }
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({}...)", &self.to_hex()[..12])
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Contact flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactFlags(u8);

impl ContactFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);

    /// Contact is trusted.
    pub const TRUSTED: Self = Self(1 << 0);

    /// Contact is hidden.
    pub const HIDDEN: Self = Self(1 << 1);

    /// Creates flags from a raw byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    /// Returns the raw byte value.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }

    /// Check if a flag is set.
    #[must_use]
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }
}

/// Device/contact type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum ContactType {
    /// Unknown device type.
    #[default]
    Unknown = 0,
    /// Standard node.
    Node = 1,
    /// Repeater node.
    Repeater = 2,
    /// Room/chat server.
    Room = 3,
}

impl ContactType {
    /// Parses a contact type from a byte.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Node,
            2 => Self::Repeater,
            3 => Self::Room,
            _ => Self::Unknown,
        }
    }
}

/// Information about a contact.
#[derive(Debug, Clone)]
pub struct Contact {
    /// The contact's public key.
    pub public_key: PublicKey,
    /// Device type.
    pub device_type: ContactType,
    /// Contact flags.
    pub flags: ContactFlags,
    /// Outbound path length (-1 means flood).
    pub out_path_len: i8,
    /// Outbound path data.
    pub out_path: Bytes,
    /// Advertised name.
    pub name: String,
    /// Last advertisement timestamp (Unix seconds).
    pub last_advert: u32,
    /// Advertised latitude.
    pub latitude: Option<f64>,
    /// Advertised longitude.
    pub longitude: Option<f64>,
    /// Last modification timestamp.
    pub last_modified: u32,
}

impl Contact {
    /// Returns true if this contact uses flood routing.
    #[must_use]
    pub const fn is_flood(&self) -> bool {
        self.out_path_len < 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_key_from_bytes() {
        let bytes = [0u8; 32];
        let key = PublicKey::from_bytes(&bytes);
        assert_eq!(key.as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn test_public_key_prefix() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xAB;
        bytes[1] = 0xCD;
        bytes[5] = 0xEF;
        let key = PublicKey::from_bytes(&bytes);
        let prefix = key.prefix();
        assert_eq!(prefix[0], 0xAB);
        assert_eq!(prefix[1], 0xCD);
        assert_eq!(prefix[5], 0xEF);
    }

    #[test]
    fn test_public_key_hex() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xAB;
        let key = PublicKey::from_bytes(&bytes);
        let hex = key.to_hex();
        assert_eq!(&hex[..2], "ab");

        let parsed = PublicKey::from_hex(&hex).unwrap();
        assert_eq!(parsed.as_bytes(), key.as_bytes());
    }

    #[test]
    fn test_contact_flags() {
        let flags = ContactFlags::from_byte(0b11);
        assert!(flags.contains(ContactFlags::TRUSTED));
        assert!(flags.contains(ContactFlags::HIDDEN));

        let flags = ContactFlags::from_byte(0b01);
        assert!(flags.contains(ContactFlags::TRUSTED));
        assert!(!flags.contains(ContactFlags::HIDDEN));
    }

    #[test]
    fn test_contact_type() {
        assert_eq!(ContactType::from_byte(0), ContactType::Unknown);
        assert_eq!(ContactType::from_byte(1), ContactType::Node);
        assert_eq!(ContactType::from_byte(2), ContactType::Repeater);
        assert_eq!(ContactType::from_byte(3), ContactType::Room);
        assert_eq!(ContactType::from_byte(99), ContactType::Unknown);
    }
}
