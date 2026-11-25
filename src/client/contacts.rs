//! Contact commands. Successful changes are mirrored into the contact cache.

use std::time::Duration;

use super::{MeshCore, Turn, reply, wait_reply};
use crate::error::{Error, Result};
use crate::event::{Event, Subscription};
use crate::protocol::codes::CommandType;
use crate::protocol::path::{MAX_PATH_SIZE, PathLen};
use crate::state::{Answer, Change, State};
use crate::types::{AdvertPath, AutoAddConfig, Contact, ContactList, PublicKey};

/// How long a contact list may pause between contacts. meshcore_py looks every
/// 5 s whether a contact came, so it lets a list pause 5 to 10 s.
const CONTACTS_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads a contact list that has started on the link with generation `link`:
/// any number of contacts, then the end marker. The list may pause up to
/// [`CONTACTS_IDLE_TIMEOUT`] between them.
async fn read_contacts(link: u64, events: &mut Subscription) -> Result<ContactList> {
    let mut deadline = tokio::time::Instant::now() + CONTACTS_IDLE_TIMEOUT;
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv_on_link(link))
            .await
            .map_err(|_| Error::Timeout)??;
        match event {
            Event::NextContact(_) => {
                deadline = tokio::time::Instant::now() + CONTACTS_IDLE_TIMEOUT;
            }
            Event::Contacts(list) => return Ok((*list).clone()),
            Event::Error(info) => return Err(super::error_from_info(info)),
            Event::Disconnected { .. } => return Err(Error::ConnectionLost),
            _ => {}
        }
    }
}

/// A change of the contact `key` that `taken` makes in the cache. When it is
/// not known whether the device made it, the next list has to bring the contact.
fn contact_change(key: PublicKey, taken: impl FnOnce(&mut State) + Send + 'static) -> Change {
    Box::new(move |state, answer| match answer {
        Answer::Taken => taken(state),
        Answer::Refused => state.contact_refused(key),
        Answer::Unknown => state.contact_changed(key),
    })
}

fn key_frame(command: CommandType, key: &PublicKey) -> Vec<u8> {
    let mut frame = Vec::with_capacity(33);
    frame.push(command.code());
    frame.extend_from_slice(key.as_bytes());
    frame
}

/// The `CMD_ADD_UPDATE_CONTACT` frame for a contact.
pub(crate) fn update_contact_frame(contact: &Contact, lastmod: Option<u32>) -> Result<Vec<u8>> {
    if contact.out_path.len() > MAX_PATH_SIZE {
        return Err(Error::invalid(format!(
            "path of {} bytes, at most {MAX_PATH_SIZE} fit",
            contact.out_path.len()
        )));
    }
    // The device would take the missing hops for zero hashes.
    if !contact.out_path_len.is_none() && contact.out_path_len.byte_len() != contact.out_path.len()
    {
        return Err(Error::invalid(format!(
            "the out path has {} bytes, its length byte announces {}",
            contact.out_path.len(),
            contact.out_path_len.byte_len()
        )));
    }
    // The device would keep the name up to the NUL.
    if contact.name.contains('\0') {
        return Err(Error::invalid("the contact name holds a NUL byte"));
    }
    let mut frame = key_frame(CommandType::AddUpdateContact, &contact.public_key);
    frame.push(contact.contact_type.code());
    frame.push(contact.flags);
    frame.push(contact.out_path_len.as_byte());
    let mut path = [0u8; MAX_PATH_SIZE];
    path[..contact.out_path.len()].copy_from_slice(&contact.out_path);
    frame.extend_from_slice(&path);
    frame.extend_from_slice(&contact.name_field());
    frame.extend_from_slice(&contact.last_advert.to_le_bytes());
    frame.extend_from_slice(&contact.latitude_e6.to_le_bytes());
    frame.extend_from_slice(&contact.longitude_e6.to_le_bytes());
    if let Some(lastmod) = lastmod {
        frame.extend_from_slice(&lastmod.to_le_bytes());
    }
    Ok(frame)
}

/// A contact as the device holds it after [`update_contact_frame`], in the
/// form it hands contacts back: it keeps the name field as written and
/// returns it up to the first NUL, at most 31 bytes, then NULs. Without
/// `lastmod` the device stamps the record with its clock, which is not known
/// here.
fn as_stored(contact: &Contact, lastmod: Option<u32>) -> Contact {
    let mut stored = contact.clone();
    let field = contact.name_field();
    let len = field[..31].iter().position(|&b| b == 0).unwrap_or(31);
    stored.name_raw = [0; 32];
    stored.name_raw[..len].copy_from_slice(&field[..len]);
    stored.name = crate::protocol::bytes::text_until_nul(&stored.name_raw);
    if let Some(lastmod) = lastmod {
        stored.lastmod = lastmod;
    }
    stored
}

impl Turn<'_> {
    /// Asks for the contacts and waits for the list to start; returns the
    /// generation of its link and the events from then on. An end before the
    /// start belongs to an earlier list that went on after its request gave up.
    async fn start_contacts(&self, lastmod: u32) -> Result<(u64, Subscription)> {
        let mut frame = vec![CommandType::GetContacts.code()];
        if lastmod > 0 {
            frame.extend_from_slice(&lastmod.to_le_bytes());
        }
        let expect = |state: &mut State| state.contacts_request_since = Some(lastmod);
        let (link, mut events) = self.send(&frame, expect).await?;
        let start = reply!(Event::ContactsStart { .. } => ());
        wait_reply(link, &mut events, CONTACTS_IDLE_TIMEOUT, start).await?;
        Ok((link, events))
    }
}

impl MeshCore {
    /// Reads the contact list: all contacts with `lastmod` 0, else those
    /// changed since, which tells neither of contacts removed on the device
    /// nor of changes it stamped with an earlier time, its clock set back. A
    /// list asked for in full that brings as many contacts as the device
    /// reports to hold (or is as short as the one before, as firmware 1.16.0
    /// counts entries it does not list) replaces the cache; any other is
    /// merged. When the cache then holds another number of contacts than the
    /// device, the next list has to be a full one:
    /// [`contacts_dirty`](Self::contacts_dirty) is true and
    /// [`contacts_lastmod`](Self::contacts_lastmod) 0. A contact that changes
    /// in the cache while the list comes in (written or removed by this
    /// client, or deleted by the device) keeps that change, as the list may
    /// hold it from before.
    ///
    /// When the list stalls for 5 s (no start, contact or end), this fails
    /// with [`Error::Timeout`]. A list that only starts after that, or another
    /// app's on another link, is merged into the cache but does not count as
    /// read, as it may be partial. A call takes only the end of a list whose
    /// start came after its request: the end of an earlier list that goes on
    /// late is not its answer, and the device refuses the request with
    /// [`ErrorCode::BadState`](crate::ErrorCode::BadState) while such a list
    /// is still coming.
    pub async fn get_contacts(&self, lastmod: u32) -> Result<ContactList> {
        let turn = self.turn().await;
        let (link, mut events) = turn.start_contacts(lastmod).await?;
        read_contacts(link, &mut events).await
    }

    /// Asks for the contact list and returns once it starts, without waiting
    /// for the rest; the contacts arrive as [`Event::NextContact`] and
    /// [`Event::Contacts`], and the cache takes them as with
    /// [`get_contacts`](Self::get_contacts).
    pub async fn get_contacts_async(&self, lastmod: u32) -> Result<()> {
        let turn = self.turn().await;
        turn.start_contacts(lastmod).await.map(drop)
    }

    /// Forgets a contact's path, so messages to it are flooded.
    pub async fn reset_path(&self, key: &PublicKey) -> Result<()> {
        let key = *key;
        let frame = key_frame(CommandType::ResetPath, &key);
        let change = contact_change(key, move |state| state.reset_contact_path(&key));
        self.request_change(&frame, change, reply!(Event::Ok { .. } => ()))
            .await
    }

    /// Sends a contact's advert zero-hop, for nodes nearby to add it.
    pub async fn share_contact(&self, key: &PublicKey) -> Result<()> {
        self.request_ok(&key_frame(CommandType::ShareContact, key))
            .await
            .map(drop)
    }

    /// Exports a contact, or this node with `None`, as a `meshcore://` URI.
    pub async fn export_contact(&self, key: Option<&PublicKey>) -> Result<String> {
        let frame = match key {
            Some(key) => key_frame(CommandType::ExportContact, key),
            None => vec![CommandType::ExportContact.code()],
        };
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::ContactUri(uri) => uri.clone()),
        )
        .await
    }

    /// Imports a contact card (the bytes after `meshcore://`, 98 or more).
    pub async fn import_contact(&self, card: &[u8]) -> Result<()> {
        if card.len() < 98 {
            return Err(Error::invalid(format!(
                "contact card of {} bytes, the firmware takes 98 and more",
                card.len()
            )));
        }
        let mut frame = vec![CommandType::ImportContact.code()];
        frame.extend_from_slice(card);
        self.request_ok(&frame).await.map(drop)
    }

    /// Removes a contact from the device and the cache. A list on its way
    /// meanwhile (as with [`get_contacts_async`](Self::get_contacts_async))
    /// does not count as read then: the firmware leaves another contact out
    /// of it.
    pub async fn remove_contact(&self, key: &PublicKey) -> Result<()> {
        let key = *key;
        let frame = key_frame(CommandType::RemoveContact, &key);
        let change = contact_change(key, move |state| drop(state.remove_contact(&key)));
        let turn = self.turn().await;
        self.state().removing_contact();
        turn.request_change(&frame, change, reply!(Event::Ok { .. } => ()))
            .await
    }

    /// Writes a contact to the device (adding it if new) and the cache.
    /// Without `lastmod` the device stamps the record with its clock, which
    /// the cache does not learn: it keeps the contact's `lastmod` then. A new
    /// contact without a type ([`ContactType::None`](crate::ContactType::None))
    /// the device keeps only for a while (firmware 1.16 and later) and leaves
    /// out of its lists, so it stays out of the cache too.
    pub async fn update_contact(&self, contact: &Contact, lastmod: Option<u32>) -> Result<()> {
        let frame = update_contact_frame(contact, lastmod)?;
        let stored = as_stored(contact, lastmod);
        let change = contact_change(contact.public_key, move |state| {
            state.cache_device_contact(stored);
        });
        self.request_change(&frame, change, reply!(Event::Ok { .. } => ()))
            .await
    }

    /// Adds a contact (same as [`update_contact`](Self::update_contact)).
    pub async fn add_contact(&self, contact: &Contact) -> Result<()> {
        self.update_contact(contact, None).await
    }

    /// Sets a contact's path: whole hops of `hash_mode + 1` bytes, at most 63
    /// hops and 64 bytes (hash mode 3 is reserved). Without a hash mode the
    /// device's mode is used (one [`send_device_query`](Self::send_device_query)).
    /// Returns the contact as the device took it.
    pub async fn change_contact_path(
        &self,
        contact: &Contact,
        path: &[u8],
        hash_mode: Option<u8>,
    ) -> Result<Contact> {
        let hash_mode = match hash_mode {
            Some(mode) => mode,
            None => self.get_path_hash_mode().await?,
        };
        let hash_size = usize::from(hash_mode) + 1;
        if !path.len().is_multiple_of(hash_size) {
            return Err(Error::invalid(format!(
                "path of {} bytes is not a whole number of {hash_size}-byte hops",
                path.len()
            )));
        }
        let hops =
            u8::try_from(path.len() / hash_size).map_err(|_| Error::invalid("path too long"))?;
        let mut changed = contact.clone();
        changed.out_path_len = PathLen::new(hops, hash_mode)?;
        changed.out_path = path.to_vec();
        self.update_contact(&changed, None).await?;
        Ok(as_stored(&changed, None))
    }

    /// Sets a contact's flags. Returns the contact as the device took it.
    pub async fn change_contact_flags(&self, contact: &Contact, flags: u8) -> Result<Contact> {
        let mut changed = contact.clone();
        changed.flags = flags;
        self.update_contact(&changed, None).await?;
        Ok(as_stored(&changed, None))
    }

    /// Sets which adverts the device adds as contacts by itself, and
    /// optionally the hop limit: adverts that came over fewer hops are added
    /// (1: direct ones only, 0: no limit; the firmware caps it at 64). The
    /// type flags (see [`autoadd_flags`](crate::autoadd_flags)) count while
    /// contacts are added manually
    /// ([`set_manual_add_contacts`](Self::set_manual_add_contacts)); otherwise
    /// the device adds every type.
    pub async fn set_autoadd_config(&self, flags: u8, max_hops: Option<u8>) -> Result<()> {
        let mut frame = vec![CommandType::SetAutoaddConfig.code(), flags];
        frame.extend(max_hops);
        self.request_ok(&frame).await.map(drop)
    }

    /// The auto-add flags and, from firmware 1.14, the hop limit.
    pub async fn get_autoadd_config(&self) -> Result<AutoAddConfig> {
        let frame = [CommandType::GetAutoaddConfig.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::AutoAddConfig(c) => *c),
        )
        .await
    }

    /// Reads one contact from the device (and refreshes it in the cache). A
    /// node the device knows only from an anonymous request is read too, with
    /// [`ContactType::None`](crate::ContactType::None), but stays out of the
    /// cache, as it stays out of the device's lists. A cached one that the
    /// device refuses to hand out marks the contacts as changed
    /// ([`contacts_dirty`](Self::contacts_dirty)).
    pub async fn get_contact_by_key(&self, key: &PublicKey) -> Result<Contact> {
        let key = *key;
        let frame = key_frame(CommandType::GetContactByKey, &key);
        let change = Box::new(move |state: &mut State, answer| {
            if answer == Answer::Refused {
                state.contact_refused(key);
            }
        });
        self.request_change(
            &frame,
            change,
            reply!(Event::NextContact(c) if c.public_key == key => (**c).clone()),
        )
        .await
    }

    /// The path the last advert of a node arrived over.
    pub async fn get_advert_path(&self, key: &PublicKey) -> Result<AdvertPath> {
        let mut frame = vec![CommandType::GetAdvertPath.code(), 0];
        frame.extend_from_slice(key.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::AdvertPath(p) => (**p).clone()),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContactType;

    #[test]
    fn update_frame_layout() {
        let mut contact = Contact::new(PublicKey::new([3; 32]), ContactType::Repeater, "Rep");
        contact.out_path_len = PathLen::new(2, 1).unwrap();
        contact.out_path = vec![1, 2, 3, 4];
        contact.latitude_e6 = 16_000_002;
        contact.longitude_e6 = -2_097_148;
        let frame = update_contact_frame(&contact, None).unwrap();
        assert_eq!(frame.len(), 1 + 32 + 3 + 64 + 32 + 12);
        assert_eq!(frame[33..36], [2, 0, 0x42]);
        assert_eq!(frame[36..40], [1, 2, 3, 4]);
        assert_eq!(&frame[100..103], b"Rep");
        let lat_at = 1 + 32 + 3 + 64 + 32 + 4;
        assert_eq!(frame[lat_at..lat_at + 4], 16_000_002i32.to_le_bytes());
        let with_lastmod = update_contact_frame(&contact, Some(2000)).unwrap();
        assert_eq!(with_lastmod[..frame.len()], frame[..]);
        assert_eq!(with_lastmod[frame.len()..], 2000u32.to_le_bytes());
    }

    #[test]
    fn path_and_length_byte_have_to_agree() {
        let mut contact = Contact::new(PublicKey::new([3; 32]), ContactType::Repeater, "Rep");
        contact.out_path_len = PathLen::new(3, 0).unwrap();
        contact.out_path = vec![1, 2];
        assert!(update_contact_frame(&contact, None).is_err());
        contact.out_path.push(3);
        assert!(update_contact_frame(&contact, None).is_ok());
    }

    #[test]
    fn the_cache_gets_the_name_as_the_device_hands_it_back() {
        let mut contact = Contact::new(PublicKey::new([3; 32]), ContactType::Chat, "");
        // written as given, handed back up to the NUL
        contact.set_name("a\0b");
        assert_eq!(contact.name_field()[..3], *b"a\0b");
        let stored = as_stored(&contact, None);
        assert_eq!(stored.name, "a");
        let mut expected = [0u8; 32];
        expected[0] = b'a';
        assert_eq!(stored.name_raw, expected);
        // a field without a NUL comes back as 31 bytes
        contact.name_raw = [b'x'; 32];
        contact.name = "x".repeat(32);
        let stored = as_stored(&contact, Some(5));
        assert_eq!(stored.name, "x".repeat(31));
        assert_eq!(stored.name_raw[31], 0);
        assert_eq!(stored.lastmod, 5);
    }

    #[test]
    fn flood_contact_is_written_with_ff() {
        let contact = Contact::new(PublicKey::new([3; 32]), ContactType::Chat, "x");
        let frame = update_contact_frame(&contact, None).unwrap();
        assert_eq!(frame[35], 0xff);
    }
}
