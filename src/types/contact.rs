//! Public keys and contacts.

use std::fmt;

use crate::error::{Error, Result};
use crate::protocol::path::PathLen;

/// Length of a public key in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;

/// Length of the key prefix messages are addressed with.
pub const PUBLIC_KEY_PREFIX_LEN: usize = 6;

/// A node's 32-byte Ed25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; PUBLIC_KEY_LEN]);

impl PublicKey {
    /// Wraps 32 key bytes.
    #[must_use]
    pub const fn new(bytes: [u8; PUBLIC_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Takes the key from a slice of exactly 32 bytes.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; PUBLIC_KEY_LEN]>::try_from(bytes).ok().map(Self)
    }

    /// Parses 64 hex digits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for anything but 32 bytes of hex.
    pub fn from_hex(text: &str) -> Result<Self> {
        crate::hex::decode(text)
            .and_then(|bytes| Self::from_slice(&bytes))
            .ok_or_else(|| Error::invalid(format!("{text:?} is not a 64 digit hex public key")))
    }

    /// The key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PUBLIC_KEY_LEN] {
        &self.0
    }

    /// The key as lower-case hex.
    #[must_use]
    pub fn to_hex(&self) -> String {
        crate::hex::encode(self.0)
    }

    /// The 6-byte prefix messages are addressed with.
    #[must_use]
    pub fn prefix(&self) -> [u8; PUBLIC_KEY_PREFIX_LEN] {
        let mut prefix = [0u8; PUBLIC_KEY_PREFIX_LEN];
        prefix.copy_from_slice(&self.0[..PUBLIC_KEY_PREFIX_LEN]);
        prefix
    }

    /// True when the key starts with `prefix`.
    #[must_use]
    pub fn starts_with(&self, prefix: &[u8]) -> bool {
        self.0.starts_with(prefix)
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({}..)", crate::hex::encode(&self.0[..6]))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl From<[u8; PUBLIC_KEY_LEN]> for PublicKey {
    fn from(bytes: [u8; PUBLIC_KEY_LEN]) -> Self {
        Self(bytes)
    }
}

impl std::str::FromStr for PublicKey {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::from_hex(text)
    }
}

/// What kind of node a contact is (`ADV_TYPE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContactType {
    /// No type (`ADV_TYPE_NONE`), e.g. a transient entry for an anonymous request.
    #[default]
    None,
    /// A chat node or companion (`ADV_TYPE_CHAT`).
    Chat,
    /// A repeater (`ADV_TYPE_REPEATER`).
    Repeater,
    /// A room server (`ADV_TYPE_ROOM`).
    Room,
    /// A sensor (`ADV_TYPE_SENSOR`).
    Sensor,
    /// A type this library does not know.
    Other(u8),
}

impl ContactType {
    /// Maps the type byte.
    #[must_use]
    pub const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::None,
            1 => Self::Chat,
            2 => Self::Repeater,
            3 => Self::Room,
            4 => Self::Sensor,
            other => Self::Other(other),
        }
    }

    /// The type byte.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Chat => 1,
            Self::Repeater => 2,
            Self::Room => 3,
            Self::Sensor => 4,
            Self::Other(code) => code,
        }
    }

    /// The short name meshcore_py's packet parser uses
    /// (`NONE`, `CLI`, `REP`, `ROOM`, `SENS`).
    #[must_use]
    pub const fn short_name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Chat => "CLI",
            Self::Repeater => "REP",
            Self::Room => "ROOM",
            Self::Sensor => "SENS",
            Self::Other(_) => "UNK",
        }
    }
}

/// Bits of [`Contact::flags`].
pub mod contact_flags {
    /// The contact is a favourite.
    pub const FAVOURITE: u8 = 0x01;
    /// The contact may request base telemetry when the mode is "use flags".
    pub const TELEMETRY_BASE: u8 = 0x02;
    /// The contact may request location telemetry when the mode is "use flags".
    pub const TELEMETRY_LOCATION: u8 = 0x04;
    /// The contact may request environment telemetry when the mode is "use flags".
    pub const TELEMETRY_ENVIRONMENT: u8 = 0x08;
    /// The contact may run CLI commands on this node.
    pub const REMOTE_CLI: u8 = 0x10;
}

/// A contact as the device stores it (`RESP_CODE_CONTACT`, `PUSH_CODE_NEW_ADVERT`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// The node's public key.
    pub public_key: PublicKey,
    /// What kind of node it is.
    pub contact_type: ContactType,
    /// Flags, see [`contact_flags`].
    pub flags: u8,
    /// The out path length byte; [`PathLen::NONE`] means flood.
    pub out_path_len: PathLen,
    /// The out path: the `out_path_len.byte_len()` bytes the length byte
    /// announces, at most the 64 the device stores (it takes the length byte
    /// unchecked, so another client may have written one announcing more).
    pub out_path: Vec<u8>,
    /// The advertised name.
    pub name: String,
    /// The name field as read, written back unchanged by
    /// [`MeshCore::update_contact`](crate::MeshCore::update_contact) while
    /// `name` is unchanged (a name cut mid-character keeps its bytes).
    pub name_raw: [u8; 32],
    /// The timestamp of the node's last advert, by its clock.
    pub last_advert: u32,
    /// Latitude in millionths of a degree (0 when unknown).
    pub latitude_e6: i32,
    /// Longitude in millionths of a degree (0 when unknown).
    pub longitude_e6: i32,
    /// When the device last changed the record, by its clock.
    pub lastmod: u32,
}

impl Contact {
    /// A contact as meshcore_py's CLI builds one to add: no flags, flood
    /// path, no location, no timestamps.
    #[must_use]
    pub fn new(public_key: PublicKey, contact_type: ContactType, name: impl Into<String>) -> Self {
        let name = name.into();
        let mut contact = Self {
            public_key,
            contact_type,
            flags: 0,
            out_path_len: PathLen::NONE,
            out_path: Vec::new(),
            name: String::new(),
            name_raw: [0; 32],
            last_advert: 0,
            latitude_e6: 0,
            longitude_e6: 0,
            lastmod: 0,
        };
        contact.set_name(&name);
        contact
    }

    /// Sets the name and its raw field. The name is cut to 31 bytes at a
    /// character boundary, and at a NUL byte: the firmware stores the 32-byte
    /// field as it comes and hands back at most 31 bytes of it, up to the
    /// first NUL.
    pub fn set_name(&mut self, name: &str) {
        self.name_raw = name_field_of(name);
        self.name = crate::protocol::bytes::text_until_nul(&self.name_raw);
    }

    /// The name field to send: the raw field while `name` still decodes from
    /// it, else `name` cut to 31 bytes.
    #[must_use]
    pub fn name_field(&self) -> [u8; 32] {
        if crate::protocol::bytes::text_until_nul(&self.name_raw) == self.name {
            return self.name_raw;
        }
        name_field_of(&self.name)
    }

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

    /// True when messages to this contact are flooded (no known path).
    #[must_use]
    pub const fn is_flood(&self) -> bool {
        self.out_path_len.is_none()
    }

    /// The out path as hex.
    #[must_use]
    pub fn out_path_hex(&self) -> String {
        crate::hex::encode(&self.out_path)
    }
}

/// A name field: the name cut to 31 bytes at a character boundary, then NULs.
fn name_field_of(name: &str) -> [u8; 32] {
    let name = crate::util::cut_at_char_boundary(name, 31);
    let mut field = [0u8; 32];
    field[..name.len()].copy_from_slice(name.as_bytes());
    field
}

/// The contacts of one `CMD_GET_CONTACTS` reply.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactList {
    /// The contacts in the order the device sent them.
    pub contacts: Vec<Contact>,
    /// The most recent `lastmod` of the list, to pass as `since` next time.
    pub lastmod: u32,
}

/// Where a message goes: a full public key or a key prefix of 6 bytes or more
/// (messages are addressed with the first 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    bytes: Vec<u8>,
    contact_type: Option<ContactType>,
}

impl Destination {
    /// A destination from hex: a full key (64 digits) or a prefix of at least
    /// 6 bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for bad hex, or fewer than 6 or more than 32
    /// bytes.
    pub fn from_hex(text: &str) -> Result<Self> {
        let bytes = crate::hex::decode(text)
            .ok_or_else(|| Error::invalid(format!("{text:?} is not hex")))?;
        Self::from_bytes(&bytes)
    }

    /// A destination from key bytes: 6 to 32 bytes.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidArgument`] for fewer than 6 or more than 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if !(PUBLIC_KEY_PREFIX_LEN..=PUBLIC_KEY_LEN).contains(&bytes.len()) {
            return Err(Error::invalid(format!(
                "a destination needs 6 to 32 key bytes, got {}",
                bytes.len()
            )));
        }
        Ok(Self {
            bytes: bytes.to_vec(),
            contact_type: None,
        })
    }

    /// The 6-byte prefix messages are addressed with.
    #[must_use]
    pub fn prefix(&self) -> [u8; PUBLIC_KEY_PREFIX_LEN] {
        let mut prefix = [0u8; PUBLIC_KEY_PREFIX_LEN];
        prefix.copy_from_slice(&self.bytes[..PUBLIC_KEY_PREFIX_LEN]);
        prefix
    }

    /// The full key, when this destination has one.
    #[must_use]
    pub fn public_key(&self) -> Option<PublicKey> {
        PublicKey::from_slice(&self.bytes)
    }

    /// The key bytes this destination holds (6 to 32).
    #[must_use]
    pub fn key_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The contact type, when the destination was made from a contact.
    #[must_use]
    pub const fn contact_type(&self) -> Option<ContactType> {
        self.contact_type
    }
}

impl From<PublicKey> for Destination {
    fn from(key: PublicKey) -> Self {
        Self {
            bytes: key.0.to_vec(),
            contact_type: None,
        }
    }
}

impl From<&PublicKey> for Destination {
    fn from(key: &PublicKey) -> Self {
        Self::from(*key)
    }
}

impl From<&Contact> for Destination {
    fn from(contact: &Contact) -> Self {
        Self {
            bytes: contact.public_key.0.to_vec(),
            contact_type: Some(contact.contact_type),
        }
    }
}

impl From<[u8; PUBLIC_KEY_PREFIX_LEN]> for Destination {
    fn from(prefix: [u8; PUBLIC_KEY_PREFIX_LEN]) -> Self {
        Self {
            bytes: prefix.to_vec(),
            contact_type: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_hex_round_trip() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xab;
        bytes[31] = 0x01;
        let key = PublicKey::new(bytes);
        let parsed: PublicKey = key.to_hex().parse().unwrap();
        assert_eq!(parsed, key);
        assert_eq!(key.prefix(), [0xab, 0, 0, 0, 0, 0]);
        assert!(PublicKey::from_hex("abcd").is_err());
        assert_eq!(format!("{key:?}"), "PublicKey(ab0000000000..)");
    }

    #[test]
    fn contact_type_codes() {
        for code in 0..=255u8 {
            assert_eq!(ContactType::from_code(code).code(), code);
        }
        assert_eq!(ContactType::from_code(4), ContactType::Sensor);
    }

    #[test]
    fn new_contact_defaults() {
        let contact = Contact::new(PublicKey::new([1; 32]), ContactType::Repeater, "Rep");
        assert!(contact.is_flood());
        assert_eq!(contact.name_raw[..4], *b"Rep\0");
        assert_eq!(contact.name_field(), contact.name_raw);
        assert_eq!(contact.out_path_hex(), "");
        assert_eq!(contact.contact_type.short_name(), "REP");
        assert_eq!(ContactType::Other(9).short_name(), "UNK");
    }

    #[test]
    fn name_field_keeps_raw_bytes_until_the_name_changes() {
        let mut contact = Contact::new(PublicKey::new([1; 32]), ContactType::Chat, "");
        contact.name_raw = [0; 32];
        contact.name_raw[..4].copy_from_slice(b"Caf\xc3");
        contact.name = "Caf".into();
        assert_eq!(&contact.name_field()[..4], b"Caf\xc3");
        contact.name = "Café".into();
        assert_eq!(&contact.name_field()[..5], "Café".as_bytes());
    }

    #[test]
    fn long_names_are_cut_where_the_firmware_cuts_them() {
        let mut contact = Contact::new(PublicKey::new([1; 32]), ContactType::Chat, "");
        contact.set_name(&"a".repeat(40));
        assert_eq!(contact.name.len(), 31);
        assert_eq!(contact.name_raw[31], 0);
        // not in the middle of a character
        contact.set_name(&"é".repeat(20));
        assert_eq!(contact.name.len(), 30);
        contact.name = "b".repeat(40);
        assert_eq!(contact.name_field()[..31], [b'b'; 31]);
        assert_eq!(contact.name_field()[31], 0);
    }

    #[test]
    fn destinations() {
        let key = PublicKey::new([7; 32]);
        assert_eq!(Destination::from(key).public_key(), Some(key));
        let prefix = Destination::from_hex("070707070707").unwrap();
        assert_eq!(prefix.public_key(), None);
        assert_eq!(prefix.prefix(), [7; 6]);
        assert!(Destination::from_hex("0707").is_err());
    }
}
