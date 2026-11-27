//! Turns frames from the device into events and keeps the client's state
//! (meshcore_py's `MessageReader` plus the data tracking of `MeshCore`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{ErrorCode, Result};
use crate::event::{ErrorInfo, Event};
use crate::lpp::Telemetry;
use crate::packet::{PacketParser, message_hash};
use crate::protocol::bytes::{ByteReader, lossy_ignore};
use crate::protocol::codes::{AnonReqType, BinaryReqType, PacketType};
use crate::protocol::parse;
use crate::protocol::path::PathLen;
use crate::types::{
    AclResponse, BinaryResponse, ChannelRxInfo, Contact, ContactList, ContactType, CustomVars,
    PrivateKey, PublicKey, RxLogData, SelfInfo, SentInfo, TelemetryResponse,
};

/// The kind of a request whose answer is parsed by its tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestKind {
    Binary(BinaryReqType),
    Anon(AnonReqType),
}

/// A request about to be sent: bound to its tag when the `MSG_SENT` reply
/// arrives, before any later frame is read.
#[derive(Debug, Clone)]
pub(crate) struct PreparedRequest {
    pub(crate) kind: RequestKind,
    pub(crate) pubkey_prefix: [u8; 6],
    pub(crate) timeout: Option<Duration>,
    pub(crate) min_timeout: Duration,
    /// The suggested timeout is multiplied by this (anonymous requests: hops + 1).
    pub(crate) timeout_factor: u32,
    pub(crate) neighbours_prefix_len: u8,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    kind: RequestKind,
    pubkey_prefix: [u8; 6],
    /// `None` for a wait too long to count (no limit).
    expires_at: Option<Instant>,
    neighbours_prefix_len: u8,
}

/// How the device answered a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// With `OK`: it has done what the command says.
    Taken,
    /// With an error, or that the feature is disabled.
    Refused,
    /// With neither, or the answer was not waited for.
    Unknown,
}

/// Notes what a command changes in what the client holds, by the answer.
pub(crate) type Change = Box<dyn FnOnce(&mut State, Answer) + Send>;

#[derive(Debug, Default)]
struct ContactsInProgress {
    /// The `since` of the request the list answers; `None` for a list that
    /// does not count as read: one no request here waits for (another app's on
    /// another link, or one that came after its request timed out), or one
    /// that may leave out contacts.
    since: Option<u32>,
    /// How many contacts the device holds, as the list's start reports.
    count: u32,
    contacts: Vec<Contact>,
    /// Contacts that changed in the cache while the list came in (written or
    /// removed by the client, or deleted by the device) and that the list
    /// did not send again after: its copy of them is older, so its end keeps
    /// what the cache holds for them.
    newer_in_cache: Vec<PublicKey>,
    /// An advert or path update came while the list came in: the list may
    /// hold that contact from before the change.
    changed: bool,
    /// The contacts reported as changed while the list came in that it has
    /// not brought since.
    reported: Vec<PublicKey>,
}

/// State shared by the reader task and the client.
#[derive(Default)]
pub(crate) struct State {
    pub(crate) self_info: Option<SelfInfo>,
    /// The device's public key, unless a private key was imported since the
    /// last self info.
    pub(crate) own_key: Option<PublicKey>,
    /// The length of the device's name on the device, in bytes.
    pub(crate) name_len: usize,
    pub(crate) time: u32,
    pub(crate) contacts: Vec<Contact>,
    /// A contact list was read that left the cache with all contacts the
    /// device holds. Without one, it may hold contacts that came in one by
    /// one, or since a time.
    pub(crate) contacts_listed: bool,
    pub(crate) contacts_lastmod: u32,
    pub(crate) contacts_dirty: bool,
    /// How many more contacts the device counts than its lists bring: firmware
    /// 1.16.0 counts the transient entries they leave out.
    unlisted: usize,
    /// The same of the last list asked for in full, which may have been short
    /// by accident; `None` when it brought more than the device counted.
    last_unlisted: Option<usize>,
    /// The contacts that no list has brought since the device reported them as
    /// changed, or a command that changes them got no answer or was refused.
    reported: Vec<PublicKey>,
    pub(crate) pending_contacts: Vec<Contact>,
    /// Set before a contact list request goes out: the `since` it asks with.
    pub(crate) contacts_request_since: Option<u32>,
    building: Option<ContactsInProgress>,
    /// What the command in flight changes: noted when the device answers,
    /// before any later frame is read, or when the command ends after it went
    /// out.
    pub(crate) change: Option<Change>,
    pub(crate) prepared: Option<PreparedRequest>,
    pending: HashMap<u32, PendingRequest>,
    pub(crate) parser: PacketParser,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            contacts_dirty: true,
            ..Self::default()
        }
    }

    /// Handles one frame and returns the events it yields.
    pub(crate) fn handle_frame(&mut self, frame: &[u8]) -> Vec<Event> {
        let Some((&code, data)) = frame.split_first() else {
            // meshcore_py's text: it logs the IndexError of reading the code.
            log::warn!(target: "meshcore", "Received empty packet: index out of range");
            return Vec::new();
        };
        let Some(packet_type) = PacketType::from_code(code) else {
            log::debug!(target: "meshcore", "unhandled frame code 0x{code:02x} ({} bytes)", frame.len());
            return vec![Event::Unknown {
                code,
                data: data.to_vec(),
            }];
        };
        match self.handle(packet_type, data) {
            Ok(events) => events,
            Err(err) => {
                // meshcore_py's wording, without its traceback. Not the frame
                // itself: message frames carry decrypted text. Stats and the
                // private key have lines of their own (logged in `handle`).
                if !matches!(packet_type, PacketType::Stats | PacketType::PrivateKey) {
                    log::error!(
                        target: "meshcore",
                        "handle_rx parse error: packet type 0x{code:02x}, {} bytes: {err}",
                        frame.len()
                    );
                }
                // A list coming in lacks the contact that could not be read,
                // whichever it was: the list does not count as read, and the
                // next one is a full one.
                if packet_type == PacketType::Contact && self.building.is_some() {
                    self.discount_list();
                    self.contacts_lastmod = 0;
                    self.contacts_changed();
                }
                // A reply answers the command in flight, so its caller learns
                // at once (`get_contacts` waits for the end of its list). A
                // push came unasked: an error for it would end an unrelated
                // command, so it is only logged.
                if packet_type.is_push() {
                    Vec::new()
                } else {
                    let reason = match err {
                        crate::Error::Protocol(reason) => reason,
                        other => other.to_string(),
                    };
                    vec![Event::Error(ErrorInfo::unreadable(code, reason))]
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn handle(&mut self, packet_type: PacketType, data: &[u8]) -> Result<Vec<Event>> {
        let event = match packet_type {
            PacketType::Ok => {
                self.note_change(Answer::Taken);
                Event::Ok {
                    value: (data.len() == 4)
                        .then(|| u32::from_le_bytes([data[0], data[1], data[2], data[3]])),
                }
            }
            PacketType::Error => {
                self.note_change(Answer::Refused);
                Event::Error(ErrorInfo {
                    code: data.first().map(|&c| ErrorCode::from_code(c)),
                    reason: None,
                    packet_type: None,
                })
            }
            PacketType::ContactStart => {
                let count = ByteReader::new(data).opt_u32().unwrap_or(0);
                self.building = Some(ContactsInProgress {
                    since: self.contacts_request_since.take(),
                    count,
                    ..ContactsInProgress::default()
                });
                Event::ContactsStart { count }
            }
            PacketType::Contact => {
                let contact = parse::contact(data)?;
                if let Some(building) = &mut self.building {
                    // By key, as in meshcore_py: a contact that comes twice
                    // (the answer to CMD_GET_CONTACT_BY_KEY in the middle of
                    // a list) is listed once.
                    upsert(&mut building.contacts, contact.clone());
                    // Sent again after it changed in the cache or on the
                    // device: this copy is the newer one.
                    for keys in [&mut building.newer_in_cache, &mut building.reported] {
                        keys.retain(|key| *key != contact.public_key);
                    }
                } else {
                    // a single contact (CMD_GET_CONTACT_BY_KEY)
                    self.cache_device_contact(contact.clone());
                }
                Event::NextContact(Box::new(contact))
            }
            PacketType::ContactEnd => {
                let lastmod = ByteReader::new(data).opt_u32().unwrap_or(0);
                let contacts = if let Some(building) = self.building.take() {
                    self.end_list(building, lastmod)
                } else {
                    // Without its start the list is incomplete: the cache stays.
                    log::warn!(target: "meshcore", "end of a contact list that did not start");
                    Vec::new()
                };
                Event::Contacts(Arc::new(ContactList { contacts, lastmod }))
            }
            PacketType::NewAdvert => {
                let contact = parse::contact(data)?;
                upsert(&mut self.pending_contacts, contact.clone());
                Event::NewContact(Box::new(contact))
            }
            PacketType::SelfInfo => {
                // It answers APP_START, which stops a contact list the device
                // is sending. That list never ends: what came of it stays out
                // of the cache, and single contacts go to the cache again.
                if let Some(stopped) = self.building.take() {
                    log::debug!(
                        target: "meshcore",
                        "contact list stopped by APP_START after {} contacts",
                        stopped.contacts.len()
                    );
                }
                let (info, name_len) = parse::self_info(data)?;
                if self.own_key.is_some_and(|key| key != info.public_key) {
                    log::debug!(target: "meshcore", "another device answered: {}", info.public_key);
                    self.forget_device();
                }
                self.own_key = Some(info.public_key);
                self.self_info = Some(info.clone());
                self.name_len = name_len;
                Event::SelfInfo(Box::new(info))
            }
            PacketType::DeviceInfo => Event::DeviceInfo(Box::new(parse::device_info(data)?)),
            PacketType::MsgSent => {
                let sent = parse::sent(data)?;
                self.bind_prepared(&sent);
                Event::MessageSent(sent)
            }
            PacketType::ContactMsgRecv | PacketType::ContactMsgRecvV3 => {
                let v3 = packet_type == PacketType::ContactMsgRecvV3;
                Event::ContactMessage(Box::new(parse::contact_message(data, v3)?))
            }
            PacketType::ChannelMsgRecv | PacketType::ChannelMsgRecvV3 => {
                let v3 = packet_type == PacketType::ChannelMsgRecvV3;
                let (mut message, text) = parse::channel_message(data, v3)?;
                if self.parser.decrypt_channels {
                    let hash = message_hash(message.sender_timestamp, &text);
                    message.txt_hash = Some(hash);
                    if let Some(logged) = self.parser.find_by_msg_hash(hash) {
                        message.rx_info = Some(ChannelRxInfo {
                            path: logged.path.clone(),
                            rssi: logged.rssi,
                            snr: logged.snr,
                            recv_time: logged.recv_time,
                            attempt: logged.decrypted.as_ref().map_or(0, |d| d.attempt),
                        });
                    }
                }
                Event::ChannelMessage(Box::new(message))
            }
            PacketType::ChannelDataRecv => Event::ChannelData(Box::new(parse::channel_data(data)?)),
            PacketType::CurrentTime => {
                let time = ByteReader::new(data).u32()?;
                self.time = time;
                Event::CurrentTime(time)
            }
            PacketType::NoMoreMsgs => Event::NoMoreMessages,
            PacketType::ContactUri => {
                Event::ContactUri(format!("meshcore://{}", crate::hex::encode(data)))
            }
            PacketType::Battery => Event::Battery(parse::battery(data)?),
            PacketType::CustomVars => Event::CustomVars(CustomVars::parse(&lossy_ignore(data))),
            PacketType::Stats => match parse::stats(data) {
                Ok(parse::Stats::Core(core)) => Event::StatsCore(core),
                Ok(parse::Stats::Radio(radio)) => Event::StatsRadio(radio),
                Ok(parse::Stats::Packets(packets)) => Event::StatsPackets(packets),
                Err(err) => {
                    log_unreadable_stats(data);
                    return Err(err);
                }
            },
            PacketType::AutoaddConfig => Event::AutoAddConfig(parse::autoadd_config(data)?),
            PacketType::ChannelInfo => {
                let channel = parse::channel_info(data)?;
                self.parser.add_channel(&channel);
                Event::ChannelInfo(Box::new(channel))
            }
            PacketType::Advertisement => {
                let key = read_key(data)?;
                self.contact_changed(key);
                Event::Advertisement(key)
            }
            PacketType::PathUpdate => {
                let key = read_key(data)?;
                // One of a transient entry is in no list.
                if self.known(&key).is_some() {
                    self.contact_reported(key);
                }
                self.contacts_changed();
                Event::PathUpdate(key)
            }
            PacketType::Ack => Event::Ack(parse::ack(data)?),
            PacketType::MessagesWaiting => Event::MessagesWaiting,
            PacketType::RawData => Event::RawData(Box::new(parse::raw_data(data)?)),
            PacketType::LoginSuccess => Event::LoginSuccess(parse::login_success(data)),
            PacketType::LoginFailed => Event::LoginFailed(parse::login_failed(data)),
            PacketType::StatusResponse => {
                Event::StatusResponse(Box::new(parse::status_push(data)?))
            }
            PacketType::LogData => Event::RxLog(Box::new(self.rx_log(data))),
            PacketType::TraceData => Event::TraceData(Box::new(parse::trace(data)?)),
            PacketType::TelemetryResponse => {
                let (pubkey_prefix, telemetry) = parse::telemetry_push(data)?;
                Event::TelemetryResponse(Box::new(TelemetryResponse {
                    pubkey_prefix,
                    tag: None,
                    telemetry,
                }))
            }
            PacketType::AllowedRepeatFreq => Event::AllowedRepeatFreq(parse::repeat_freqs(data)),
            PacketType::BinaryResponse => return self.binary_response(data),
            PacketType::PathDiscoveryResponse => {
                Event::PathResponse(Box::new(parse::path_discovery(data)?))
            }
            PacketType::PrivateKey => match ByteReader::new(data).array::<64>() {
                Ok(key) => Event::PrivateKey(Box::new(PrivateKey::new(key))),
                Err(err) => {
                    // meshcore_py's line, with the length of the whole frame
                    log::error!(
                        target: "meshcore",
                        "Invalid private key response length: {}",
                        data.len() + 1
                    );
                    return Err(err);
                }
            },
            PacketType::SignStart => {
                let mut r = ByteReader::new(data);
                r.skip(1)?;
                Event::SignStart {
                    max_length: r.u32()?,
                }
            }
            PacketType::Signature => Event::Signature(data.to_vec()),
            PacketType::Disabled => {
                self.note_change(Answer::Refused);
                Event::Disabled
            }
            PacketType::ControlData => {
                let (control, discover) = parse::control_data(data)?;
                let mut events = vec![Event::ControlData(Box::new(control))];
                if let Some(discover) = discover {
                    events.push(Event::DiscoverResponse(Box::new(discover)));
                }
                return Ok(events);
            }
            PacketType::ContactDeleted => {
                let key = read_key(data)?;
                self.remove_contact(&key);
                Event::ContactDeleted(key)
            }
            PacketType::ContactsFull => Event::ContactsFull,
            PacketType::TuningParams => Event::TuningParams(parse::tuning_params(data)?),
            PacketType::DefaultFloodScope => {
                Event::DefaultFloodScope(parse::default_flood_scope(data)?)
            }
            PacketType::CliReply => Event::CliReply(lossy_ignore(data)),
            PacketType::AdvertPath => Event::AdvertPath(Box::new(parse::advert_path(data)?)),
        };
        Ok(vec![event])
    }

    /// Takes a contact list that has ended into the cache; returns its contacts.
    fn end_list(&mut self, list: ContactsInProgress, lastmod: u32) -> Vec<Contact> {
        let count = usize::try_from(list.count).unwrap_or(usize::MAX);
        let contacts = list.contacts;
        let newer: Vec<(PublicKey, Option<Contact>)> = list
            .newer_in_cache
            .iter()
            .map(|key| (*key, self.cached(key).cloned()))
            .collect();
        // Only a list known to be full replaces the cache: one asked for in
        // full that brought every contact the device holds, or is as short as
        // the one before. Any other may be partial (the late answer to an
        // earlier request can take the place of a new one) and would drop the
        // contacts it leaves out.
        let unlisted = count.checked_sub(contacts.len());
        let full = list.since == Some(0)
            && unlisted.is_some_and(|n| n == 0 || self.last_unlisted == Some(n));
        if full {
            self.contacts.clone_from(&contacts);
        } else {
            for contact in &contacts {
                upsert(&mut self.contacts, contact.clone());
            }
        }
        // What changed in the cache meanwhile stays as it is now.
        let changed_meanwhile = !newer.is_empty();
        for (key, contact) in newer {
            match contact {
                Some(contact) => upsert(&mut self.contacts, contact),
                None => self.contacts.retain(|c| c.public_key != key),
            }
        }
        // One that does not count as read may start after a time not known
        // here, so it does not stand for the list.
        let Some(since) = list.since else {
            return contacts;
        };
        if since == 0 {
            self.last_unlisted = unlisted;
        }
        if full {
            self.unlisted = unlisted.unwrap_or(0);
        }
        // A change reported before the list began that it does not bring came
        // in the second the list starts after, the device's clock was set
        // back, or the contact is gone. One without an answer may not have
        // happened, or have left the contact's lastmod. A full list is complete.
        self.reported.retain(|key| {
            let brought = contacts.iter().any(|c| c.public_key == *key);
            list.reported.contains(key) || (!full && !brought)
        });
        let lacking = self.reported.iter().any(|key| !list.reported.contains(key));
        // Less: the cache lacks contacts. Greater: it holds some that were
        // removed on the device unnoticed. With changes meanwhile the numbers
        // tell nothing.
        let held = (!changed_meanwhile).then(|| (self.contacts.len() + self.unlisted).cmp(&count));
        let incomplete = if since == 0 {
            !full
        } else {
            held.is_some_and(Ordering::is_ne)
        };
        if lacking || incomplete {
            self.contacts_lastmod = 0;
        } else if since == 0 || !contacts.is_empty() {
            // One since a time that brings nothing ends with 0: that time
            // counts on.
            self.contacts_lastmod = lastmod;
        }
        self.contacts_listed |= full || held.is_some_and(Ordering::is_ge);
        self.contacts_dirty = list.changed || lacking || incomplete;
        contacts
    }

    fn rx_log(&mut self, data: &[u8]) -> RxLogData {
        let recv_time = crate::util::unix_now_u64();
        let snr = data.first().map(|&b| f32::from(b.cast_signed()) / 4.0);
        let rssi = data.get(1).map(|&b| b.cast_signed());
        let payload = data.get(2..).unwrap_or_default().to_vec();
        let packet = if payload.is_empty() {
            None
        } else {
            self.parser
                .parse(&payload, snr.unwrap_or(0.0), rssi.unwrap_or(0), recv_time)
        };
        RxLogData {
            recv_time,
            snr,
            rssi,
            raw: data.to_vec(),
            payload,
            packet,
        }
    }

    /// A binary response: always the raw event, plus the typed one for a
    /// tracked request.
    fn binary_response(&mut self, data: &[u8]) -> Result<Vec<Event>> {
        let mut r = ByteReader::new(data);
        r.skip(1)?;
        let tag = r.u32()?;
        let body = r.rest();
        let mut events = vec![Event::BinaryResponse(Box::new(BinaryResponse {
            tag,
            data: body.to_vec(),
        }))];

        let Some(request) = self.pending.remove(&tag) else {
            log::debug!(target: "meshcore", "no tracked request for binary response tag {tag:08x}");
            return Ok(events);
        };
        let prefix = request.pubkey_prefix;
        let typed = match request.kind {
            // A line in the form of meshcore_py's for the other answers;
            // meshcore_py drops a status answer cut short without one.
            RequestKind::Binary(BinaryReqType::Status) => {
                match parse::status(body, prefix, Some(tag)) {
                    Ok(status) => Some(Event::StatusResponse(Box::new(status))),
                    Err(err) => {
                        log::error!(
                            target: "meshcore",
                            "Error parsing binary STATUS response: {err}"
                        );
                        None
                    }
                }
            }
            RequestKind::Binary(BinaryReqType::Telemetry) => {
                Some(Event::TelemetryResponse(Box::new(TelemetryResponse {
                    pubkey_prefix: prefix,
                    tag: Some(tag),
                    telemetry: Telemetry::parse(body),
                })))
            }
            RequestKind::Binary(BinaryReqType::Mma) => {
                Some(Event::MmaResponse(Box::new(parse::mma(body, prefix, tag))))
            }
            RequestKind::Binary(BinaryReqType::Acl) => {
                Some(Event::AclResponse(Box::new(AclResponse {
                    pubkey_prefix: prefix,
                    tag,
                    entries: parse::acl(body),
                })))
            }
            RequestKind::Binary(BinaryReqType::Neighbours) => {
                match parse::neighbours(body, prefix, tag, request.neighbours_prefix_len) {
                    Ok(neighbours) => Some(Event::NeighboursResponse(Box::new(neighbours))),
                    Err(err) => {
                        log::error!(
                            target: "meshcore",
                            "Error parsing binary NEIGHBOURS response: {err}"
                        );
                        None
                    }
                }
            }
            _ => None,
        };
        events.extend(typed);
        Ok(events)
    }

    fn bind_prepared(&mut self, sent: &SentInfo) {
        let Some(prepared) = self.prepared.take() else {
            return;
        };
        let suggested = u64::from(sent.suggested_timeout) * u64::from(prepared.timeout_factor);
        let wait = prepared
            .timeout
            .unwrap_or_else(|| Duration::from_micros(suggested * 1250))
            .max(prepared.min_timeout);
        let now = Instant::now();
        self.pending
            .retain(|_, request| request.expires_at.is_none_or(|at| at > now));
        self.pending.insert(
            sent.expected_ack,
            PendingRequest {
                kind: prepared.kind,
                pubkey_prefix: prepared.pubkey_prefix,
                expires_at: now.checked_add(wait),
                neighbours_prefix_len: prepared.neighbours_prefix_len,
            },
        );
    }

    /// The command in flight has ended, or was given up: a reply to it that
    /// comes later is not taken for it. One that did not go out changes
    /// nothing.
    pub(crate) fn end_command(&mut self, went_out: bool) {
        self.contacts_request_since = None;
        self.prepared = None;
        if !went_out {
            self.change = None;
        }
        self.note_change(Answer::Unknown);
    }

    fn note_change(&mut self, answer: Answer) {
        if let Some(note) = self.change.take() {
            note(self, answer);
        }
    }

    /// Forgets what is held of the device, as another one is there now, or
    /// this one after a factory reset. The client's own settings stay.
    pub(crate) fn forget_device(&mut self) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.forget_channels();
        *self = Self {
            parser,
            change: self.change.take(),
            ..Self::new()
        };
    }

    /// The device took another key, whose public key comes with its next self
    /// info. It reads its contacts anew from its storage then, without those
    /// it had not written yet, so the next list is asked for in full.
    pub(crate) fn own_key_imported(&mut self) {
        self.own_key = None;
        self.contacts_lastmod = 0;
        self.discount_list();
        self.contacts_changed();
    }

    /// A command is about to remove a contact on the device. A list on its
    /// way leaves out the contact that takes the place of one it has brought.
    pub(crate) fn removing_contact(&mut self) {
        self.discount_list();
    }

    /// A contact list on its way does not count as read.
    fn discount_list(&mut self) {
        if let Some(building) = &mut self.building {
            building.since = None;
        }
    }

    /// Contacts changed on the device (advert, path update), or may have while
    /// no link was up: the cache is dirty, also after a list that is coming in
    /// now.
    pub(crate) fn contacts_changed(&mut self) {
        self.contacts_dirty = true;
        if let Some(building) = &mut self.building {
            building.changed = true;
        }
    }

    /// A contact changed on the device, or may have: the next list has to
    /// bring it.
    pub(crate) fn contact_changed(&mut self, key: PublicKey) {
        self.contact_reported(key);
        self.contacts_changed();
    }

    /// The device refused a command about a contact. One known here it may
    /// not hold any more.
    pub(crate) fn contact_refused(&mut self, key: PublicKey) {
        if self.known(&key).is_some() {
            self.contact_changed(key);
        }
    }

    fn contact_reported(&mut self, key: PublicKey) {
        let building = self.building.as_mut().map(|b| &mut b.reported);
        for keys in std::iter::once(&mut self.reported).chain(building) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }

    /// Takes a contact the device holds into the cache. One without a type
    /// that is not known yet is a transient entry of the device (as for an
    /// anonymous request), which its contact lists leave out.
    pub(crate) fn cache_device_contact(&mut self, contact: Contact) {
        if contact.contact_type != ContactType::None || self.known(&contact.public_key).is_some() {
            self.upsert_contact(contact);
        }
    }

    /// Inserts or replaces a contact in the cache.
    pub(crate) fn upsert_contact(&mut self, contact: Contact) {
        self.changed_in_cache(contact.public_key);
        upsert(&mut self.contacts, contact);
    }

    /// Removes a contact from the cache.
    pub(crate) fn remove_contact(&mut self, key: &PublicKey) -> Option<Contact> {
        self.changed_in_cache(*key);
        // No list is expected to bring it any more.
        self.reported.retain(|reported| reported != key);
        if let Some(building) = &mut self.building {
            building.reported.retain(|reported| reported != key);
        }
        let index = self.contacts.iter().position(|c| c.public_key == *key)?;
        Some(self.contacts.remove(index))
    }

    /// Sets a contact to flood in the cache, also one that so far only a
    /// list coming in has brought.
    pub(crate) fn reset_contact_path(&mut self, key: &PublicKey) {
        if let Some(mut contact) = self.known(key).cloned() {
            contact.out_path_len = PathLen::NONE;
            contact.out_path.clear();
            self.upsert_contact(contact);
        }
    }

    /// The newest copy of a contact: of a list coming in, or in the cache.
    fn known(&self, key: &PublicKey) -> Option<&Contact> {
        self.listed(key).or_else(|| self.cached(key))
    }

    /// A cached contact by key.
    fn cached(&self, key: &PublicKey) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.public_key == *key)
    }

    /// A contact by key that the list coming in has brought, newer than the
    /// one the cache may hold.
    fn listed(&self, key: &PublicKey) -> Option<&Contact> {
        let building = self.building.as_ref()?;
        if building.newer_in_cache.contains(key) {
            return None;
        }
        building.contacts.iter().find(|c| c.public_key == *key)
    }

    /// Notes that the cached contact `key` changed while a list comes in, so
    /// that the list's older copy does not undo it.
    fn changed_in_cache(&mut self, key: PublicKey) {
        if let Some(building) = &mut self.building
            && !building.newer_in_cache.contains(&key)
        {
            building.newer_in_cache.push(key);
        }
    }

    /// The first cached contact whose key starts with `prefix`.
    pub(crate) fn contact_by_prefix(&self, prefix: &[u8]) -> Option<&Contact> {
        if prefix.is_empty() {
            return None;
        }
        self.contacts
            .iter()
            .find(|c| c.public_key.starts_with(prefix))
    }
}

fn upsert(list: &mut Vec<Contact>, contact: Contact) {
    if let Some(existing) = list.iter_mut().find(|c| c.public_key == contact.public_key) {
        *existing = contact;
    } else {
        list.push(contact);
    }
}

/// Logs meshcore_py's line for a stats reply it cannot read (`data` after the
/// code; meshcore_py counts the code too). Only a reply cut short or of an
/// unknown type cannot be read.
fn log_unreadable_stats(data: &[u8]) {
    let len = data.len() + 1;
    match data.first() {
        None => log::error!(
            target: "meshcore",
            "Stats response too short: {len} bytes, need at least 2 for header"
        ),
        Some(0) => log::error!(
            target: "meshcore",
            "Stats core response too short: {len} bytes, expected 11"
        ),
        Some(1) => log::error!(
            target: "meshcore",
            "Stats radio response too short: {len} bytes, expected 14"
        ),
        Some(2) => log::error!(
            target: "meshcore",
            "Stats packets response too short: {len} bytes, expected 26 or 30"
        ),
        Some(other) => log::error!(
            target: "meshcore",
            "Unknown stats type: {other}, data: {:02x}{}",
            PacketType::Stats.code(),
            crate::hex::encode(data)
        ),
    }
}

fn read_key(data: &[u8]) -> Result<PublicKey> {
    Ok(PublicKey::new(ByteReader::new(data).array()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;

    fn contact_frame(code: u8, key: u8, name: &str) -> Vec<u8> {
        let mut frame = vec![code];
        frame.extend([key; 32]);
        frame.extend([1, 0, 0xff]);
        frame.extend([0; 64]);
        let mut name_field = [0u8; 32];
        name_field[..name.len()].copy_from_slice(name.as_bytes());
        frame.extend(name_field);
        frame.extend([0; 16]);
        frame
    }

    fn end_frame(lastmod: u32) -> Vec<u8> {
        let mut frame = vec![4];
        frame.extend(lastmod.to_le_bytes());
        frame
    }

    fn self_info_frame() -> Vec<u8> {
        let mut frame = vec![5, 1, 22, 22];
        frame.extend([7; 32]);
        // location, multi ACKs, location policy, telemetry modes, manual add
        frame.extend([0; 12]);
        frame.extend(869_525u32.to_le_bytes());
        frame.extend(250_000u32.to_le_bytes());
        frame.extend([11, 5]);
        frame.extend(b"Node");
        frame
    }

    #[test]
    fn full_contact_list_replaces_the_cache() {
        let mut state = State::new();
        state.upsert_contact(crate::Contact::new(
            PublicKey::new([9; 32]),
            crate::ContactType::Chat,
            "gone",
        ));
        state.contacts_request_since = Some(0);
        assert_eq!(
            state.handle_frame(&[2, 2, 0, 0, 0])[0].kind(),
            EventKind::ContactsStart
        );
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&contact_frame(3, 2, "b"));
        let events = state.handle_frame(&end_frame(77));
        let Event::Contacts(list) = &events[0] else {
            panic!()
        };
        assert_eq!(list.contacts.len(), 2);
        assert_eq!(list.lastmod, 77);
        assert_eq!(state.contacts.len(), 2);
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 77);
    }

    #[test]
    fn a_full_request_answered_by_fewer_contacts_than_the_device_holds_is_merged() {
        // the late answer to an earlier request since a time can take the
        // place of a full one
        let mut state = State::new();
        state.upsert_contact(crate::Contact::new(
            PublicKey::new([9; 32]),
            crate::ContactType::Chat,
            "kept",
        ));
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 3, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(40));
        assert_eq!(state.contacts.len(), 2);
        // the cache lacks one
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
    }

    #[test]
    fn a_contact_the_device_deletes_while_a_list_comes_stays_out_of_the_cache() {
        // the device overwrote a contact the list had brought already
        let mut deleted = vec![0x8f];
        deleted.extend([1; 32]);
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&deleted);
        state.handle_frame(&contact_frame(3, 2, "b"));
        let events = state.handle_frame(&end_frame(7));
        // the list is what came, as in meshcore_py
        let Event::Contacts(list) = &events[0] else {
            panic!()
        };
        assert_eq!(list.contacts.len(), 2);
        let names: Vec<&str> = state.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["b"]);
        // one the list sends again after its deletion was added anew
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&deleted);
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&contact_frame(3, 1, "a2"));
        state.handle_frame(&end_frame(8));
        let names: Vec<&str> = state.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a2", "b"]);
    }

    #[test]
    fn changes_to_the_cache_while_a_list_comes_are_kept() {
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 4, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&contact_frame(3, 2, "b"));
        let mut with_path = contact_frame(3, 4, "d");
        with_path[35] = 0x01;
        with_path[36] = 0xaa;
        state.handle_frame(&with_path);
        // commands change contacts the list has brought already: removed,
        // written anew, set to flood (one only the list holds so far)
        state.remove_contact(&PublicKey::new([1; 32]));
        state.upsert_contact(crate::Contact::new(
            PublicKey::new([2; 32]),
            crate::ContactType::Chat,
            "b2",
        ));
        state.reset_contact_path(&PublicKey::new([4; 32]));
        // one written before the list brings it: the list's copy is newer
        state.upsert_contact(crate::Contact::new(
            PublicKey::new([3; 32]),
            crate::ContactType::Chat,
            "c-old",
        ));
        state.handle_frame(&contact_frame(3, 3, "c"));
        let events = state.handle_frame(&end_frame(7));
        // the list is what came
        let Event::Contacts(list) = &events[0] else {
            panic!()
        };
        let names: Vec<&str> = list.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "d", "c"]);
        assert!(!list.contacts[2].is_flood());
        let names: Vec<&str> = state.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["b2", "d", "c"]);
        assert!(state.contacts[1].is_flood());
    }

    #[test]
    fn a_contact_without_a_type_so_far_only_listed_takes_a_change() {
        let mut typeless = contact_frame(3, 1, "a");
        typeless[33] = 0;
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&typeless);
        let key = PublicKey::new([1; 32]);
        state.cache_device_contact(crate::Contact::new(key, ContactType::None, "a2"));
        state.handle_frame(&end_frame(7));
        assert_eq!(state.contacts[0].name, "a2");
    }

    #[test]
    fn a_path_reset_while_a_list_comes_keeps_the_newer_copy() {
        let key = PublicKey::new([1; 32]);
        let mut with_path = contact_frame(3, 1, "a2");
        with_path[35] = 0x01;
        with_path[36] = 0xaa;
        let mut state = State::new();
        state.handle_frame(&contact_frame(3, 1, "a"));
        // the list's, or the cache's when that was written since
        for (written, kept) in [(None, "a2"), (Some("a3"), "a3")] {
            state.contacts_request_since = Some(0);
            state.handle_frame(&[2, 1, 0, 0, 0]);
            state.handle_frame(&with_path);
            if let Some(name) = written {
                state.upsert_contact(crate::Contact::new(key, ContactType::Chat, name));
            }
            state.reset_contact_path(&key);
            state.handle_frame(&end_frame(7));
            assert_eq!(state.contacts[0].name, kept);
            assert!(state.contacts[0].is_flood());
        }
    }

    #[test]
    fn an_advert_while_a_list_comes_leaves_the_contacts_dirty() {
        let mut advert = vec![0x80];
        advert.extend([1; 32]);
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        // the contact changed after the list brought it
        state.handle_frame(&advert);
        state.handle_frame(&end_frame(7));
        assert!(state.contacts_dirty);
        // a list without changes meanwhile clears the mark
        state.contacts_request_since = Some(7);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(9));
        assert!(!state.contacts_dirty);
    }

    #[test]
    fn a_change_reported_while_a_list_comes_is_expected_of_the_next() {
        let advert = |key: u8| [&[0x80][..], &[key; 32]].concat();
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        // one the list has brought already, and one it brings afterwards
        state.handle_frame(&advert(1));
        state.handle_frame(&advert(2));
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&end_frame(30));
        assert!(state.contacts_dirty);
        // a list without the first one's change lacks it, and so does the
        // next, whatever else they bring
        for (name, lastmod) in [("b2", 40), ("b3", 50)] {
            state.contacts_request_since = Some(30);
            state.handle_frame(&[2, 2, 0, 0, 0]);
            state.handle_frame(&contact_frame(3, 2, name));
            state.handle_frame(&end_frame(lastmod));
            assert!(state.contacts_dirty);
            assert_eq!(state.contacts_lastmod, 0);
        }
        // a full one brings all there is
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&end_frame(30));
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 30);
        // with only the second one's change the next list lacks nothing
        state.contacts_request_since = Some(30);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&advert(2));
        state.handle_frame(&contact_frame(3, 2, "b2"));
        state.handle_frame(&end_frame(40));
        state.contacts_request_since = Some(40);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&end_frame(0));
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 40);
    }

    #[test]
    fn a_contact_deleted_and_added_again_while_a_list_comes_is_expected_of_the_next() {
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&[&[0x8f][..], &[1; 32]].concat());
        state.handle_frame(&[&[0x80][..], &[1; 32]].concat());
        state.handle_frame(&end_frame(30));
        assert_eq!(state.contacts, Vec::<Contact>::new());
        state.contacts_request_since = Some(30);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&end_frame(0));
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
    }

    #[test]
    fn a_path_update_of_a_contact_so_far_only_listed_is_expected_of_the_next_list() {
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        // in the same second as the contact's last change
        state.handle_frame(&[&[0x81][..], &[1; 32]].concat());
        state.handle_frame(&end_frame(30));
        state.contacts_request_since = Some(30);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&end_frame(0));
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
    }

    /// Feeds a list of these contacts, of which the device counts `count`.
    fn feed_list(state: &mut State, since: u32, count: u8, contacts: &[(u8, &str)], end: u32) {
        state.contacts_request_since = Some(since);
        state.handle_frame(&[2, count, 0, 0, 0]);
        for (key, name) in contacts {
            state.handle_frame(&contact_frame(3, *key, name));
        }
        state.handle_frame(&end_frame(end));
    }

    #[test]
    fn contacts_removed_unnoticed_make_the_next_list_a_full_one() {
        let mut state = State::new();
        feed_list(&mut state, 0, 2, &[(1, "a"), (2, "b")], 30);
        assert!(!state.contacts_dirty);
        // one was removed, one added
        for _ in 0..2 {
            feed_list(&mut state, 30, 2, &[(3, "c")], 40);
            assert!(state.contacts_dirty);
            assert_eq!(state.contacts_lastmod, 0);
        }
        feed_list(&mut state, 0, 2, &[(2, "b"), (3, "c")], 40);
        assert_eq!(state.contacts.len(), 2);
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 40);
        // one added that a list since a time does not bring
        feed_list(&mut state, 40, 3, &[], 0);
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
        // one added while a list came in, twice: that says nothing of others
        feed_list(
            &mut state,
            0,
            3,
            &[(2, "b"), (3, "c"), (4, "d"), (5, "e")],
            50,
        );
        feed_list(
            &mut state,
            0,
            4,
            &[(2, "b"), (3, "c"), (4, "d"), (5, "e"), (6, "f")],
            60,
        );
        feed_list(&mut state, 60, 4, &[], 0);
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
        // a change in the cache meanwhile: the number tells nothing
        feed_list(&mut state, 0, 3, &[(2, "b"), (3, "c"), (4, "d")], 40);
        state.contacts_request_since = Some(40);
        state.handle_frame(&[2, 3, 0, 0, 0]);
        state.remove_contact(&PublicKey::new([4; 32]));
        state.handle_frame(&end_frame(0));
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 40);
    }

    #[test]
    fn a_short_list_during_which_a_contact_changed_does_not_stand_for_all() {
        // one asked for in full, and one since a time
        for since in [0, 10] {
            let mut state = State::new();
            state.contacts_request_since = Some(since);
            state.handle_frame(&[2, 3, 0, 0, 0]);
            state.handle_frame(&contact_frame(3, 1, "a"));
            state.handle_frame(&contact_frame(3, 2, "b"));
            state.reset_contact_path(&PublicKey::new([1; 32]));
            state.handle_frame(&end_frame(30));
            assert!(!state.contacts_listed);
            // a full one does
            state.contacts_request_since = Some(0);
            state.handle_frame(&[2, 2, 0, 0, 0]);
            state.handle_frame(&contact_frame(3, 1, "a"));
            state.handle_frame(&contact_frame(3, 2, "b"));
            state.reset_contact_path(&PublicKey::new([1; 32]));
            state.handle_frame(&end_frame(30));
            assert!(state.contacts_listed && !state.contacts_dirty);
        }
    }

    #[test]
    fn a_contact_of_a_list_that_cannot_be_read_has_the_contacts_read_in_full() {
        // the list lacks it, whichever contact it was; its request fails, but
        // the list goes on to its end
        let mut state = State::new();
        feed_list(&mut state, 0, 1, &[(1, "a")], 30);
        assert!(!state.contacts_dirty);
        for since in [0, 30] {
            state.contacts_request_since = Some(since);
            state.handle_frame(&[2, 1, 0, 0, 0]);
            let cut = contact_frame(3, 1, "a2");
            assert_eq!(state.handle_frame(&cut[..50])[0].kind(), EventKind::Error);
            state.handle_frame(&end_frame(40));
            assert!(state.contacts_dirty, "since {since}");
            assert_eq!(state.contacts_lastmod, 0, "since {since}");
            // a full list puts it right
            feed_list(&mut state, 0, 1, &[(1, "a2")], 40);
            assert!(!state.contacts_dirty);
            assert_eq!(state.contacts[0].name, "a2");
        }
    }

    #[test]
    fn a_device_may_count_contacts_that_it_does_not_list() {
        // firmware 1.16.0 counts the transient entries its lists leave out:
        // a list as short as the one before is full
        let mut state = State::new();
        feed_list(&mut state, 0, 3, &[(1, "a"), (2, "b")], 30);
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
        feed_list(&mut state, 0, 3, &[(1, "a"), (2, "b")], 30);
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 30);
        feed_list(&mut state, 30, 3, &[], 0);
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 30);
        // one removed unnoticed
        feed_list(&mut state, 30, 2, &[], 0);
        assert!(state.contacts_dirty);
        feed_list(&mut state, 0, 2, &[(2, "b")], 30);
        assert_eq!(state.contacts.len(), 1);
        assert!(!state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 30);
    }

    #[test]
    fn after_an_imported_key_a_list_on_its_way_does_not_count_as_read() {
        let mut state = State::new();
        for started in [false, true] {
            state.contacts_request_since = Some(0);
            if started {
                state.handle_frame(&[2, 1, 0, 0, 0]);
            }
            // the list's request was given up, the import has come since
            state.end_command(true);
            state.own_key_imported();
            if !started {
                state.handle_frame(&[2, 1, 0, 0, 0]);
            }
            state.handle_frame(&contact_frame(3, 1, "a"));
            state.handle_frame(&end_frame(30));
            assert!(state.contacts_dirty);
            assert_eq!(state.contacts_lastmod, 0);
        }
    }

    #[test]
    fn changes_are_noted_by_the_answer() {
        let change = || -> Change {
            Box::new(|state, answer| {
                state.time = match answer {
                    Answer::Taken => 1,
                    Answer::Refused => 2,
                    Answer::Unknown => 3,
                };
            })
        };
        let mut state = State::new();
        for (answer, noted) in [(&[0][..], 1), (&[1, 2], 2), (&[15], 2), (&[10], 3)] {
            state.change = Some(change());
            state.handle_frame(answer);
            // once, before the command ends, if it is one of the answers
            assert_eq!(state.change.is_none(), noted < 3);
            state.end_command(true);
            assert_eq!(state.time, noted);
        }
        // nothing, if the command did not go out
        state.change = Some(change());
        state.time = 0;
        state.end_command(false);
        assert_eq!((state.time, state.change.is_none()), (0, true));
        // an OK that comes later is no answer to it
        state.handle_frame(&[0]);
        assert_eq!(state.time, 0);
        // another device's self info leaves the command in flight alone
        state.handle_frame(&self_info_frame());
        state.change = Some(change());
        let mut other = self_info_frame();
        other[4] = 8;
        state.handle_frame(&other);
        state.handle_frame(&[0]);
        assert_eq!(state.time, 1);
    }

    #[test]
    fn a_partial_answer_to_a_full_request_may_lack_a_reported_change() {
        // the late answer to an earlier request since a time
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&end_frame(30));
        state.handle_frame(&[&[0x80][..], &[1; 32]].concat());
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&end_frame(40));
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
    }

    #[test]
    fn an_advert_cut_short_changes_nothing() {
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(7));
        assert!(!state.contacts_dirty);
        // an advert and a path update without a whole key
        for code in [0x80, 0x81] {
            assert!(state.handle_frame(&[code, 1, 2, 3]).is_empty());
            assert!(!state.contacts_dirty);
        }
    }

    #[test]
    fn a_list_no_request_here_waits_for_is_merged_only() {
        // another app's on another link, or one that came after get_contacts
        // gave up: it may start after a time not known here
        let mut state = State::new();
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(30));
        assert_eq!(state.contacts.len(), 1);
        assert!(!state.contacts_listed && state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 0);
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(30));
        assert!(state.contacts_listed && !state.contacts_dirty);
        // an advert changed a contact the other app has read already
        let mut advert = vec![0x80];
        advert.extend([1; 32]);
        state.handle_frame(&advert);
        state.handle_frame(&[2, 3, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 3, "c"));
        state.handle_frame(&end_frame(50));
        assert_eq!(state.contacts.len(), 2);
        assert!(state.contacts_dirty);
        assert_eq!(state.contacts_lastmod, 30);
    }

    #[test]
    fn a_transient_entry_stays_out_of_the_cache() {
        // the device keeps a node asked anonymously without a type
        let mut transient = contact_frame(3, 1, "");
        transient[33] = 0;
        let mut state = State::new();
        let events = state.handle_frame(&transient);
        assert_eq!(events[0].kind(), EventKind::NextContact);
        assert_eq!(state.contacts, Vec::<Contact>::new());
        // one the cache holds is refreshed whatever its type
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&transient);
        assert_eq!(state.contacts.len(), 1);
        assert_eq!(state.contacts[0].contact_type, ContactType::None);
    }

    #[test]
    fn a_contact_sent_twice_in_a_list_is_listed_once() {
        // the answer to CMD_GET_CONTACT_BY_KEY in the middle of a full list
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&contact_frame(3, 1, "a2"));
        let events = state.handle_frame(&end_frame(7));
        let Event::Contacts(list) = &events[0] else {
            panic!()
        };
        let names: Vec<&str> = list.contacts.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a2", "b"]);
        assert_eq!(state.contacts, list.contacts);
    }

    #[test]
    fn app_start_stops_a_contact_list() {
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        // APP_START (as after a reconnect) stops the list, it never ends
        state.handle_frame(&self_info_frame());
        assert_eq!(state.contacts, Vec::<Contact>::new());
        // the answer to CMD_GET_CONTACT_BY_KEY goes to the cache again
        state.handle_frame(&contact_frame(3, 2, "b"));
        assert_eq!(state.contacts.len(), 1);
        assert_eq!(state.contacts[0].name, "b");
    }

    #[test]
    fn a_list_end_without_its_start_leaves_the_cache_alone() {
        let mut state = State::new();
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.contacts_lastmod = 5;
        let events = state.handle_frame(&end_frame(99));
        assert_eq!(events[0].kind(), EventKind::Contacts);
        assert_eq!(state.contacts.len(), 1);
        assert_eq!(state.contacts_lastmod, 5);
    }

    #[test]
    fn a_list_end_whose_lastmod_is_cut_short_ends_with_0() {
        // meshcore_py fills in zeros for the missing bytes
        let mut state = State::new();
        state.contacts_request_since = Some(0);
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        let events = state.handle_frame(&[4, 0x88, 0x13]);
        let Event::Contacts(list) = &events[0] else {
            panic!()
        };
        assert_eq!(list.lastmod, 0);
        assert_eq!(state.contacts.len(), 1);
        // the next list since then is a full one
        assert_eq!(state.contacts_lastmod, 0);
    }

    #[test]
    fn incremental_contact_list_merges() {
        let mut state = State::new();
        state.handle_frame(&[2, 1, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&end_frame(10));
        state.contacts_request_since = Some(10);
        state.handle_frame(&[2, 2, 0, 0, 0]);
        state.handle_frame(&contact_frame(3, 2, "b"));
        state.handle_frame(&end_frame(20));
        assert_eq!(state.contacts.len(), 2);
    }

    #[test]
    fn new_adverts_go_to_pending_contacts_not_the_cache() {
        let mut state = State::new();
        let events = state.handle_frame(&contact_frame(0x8a, 5, "new"));
        assert_eq!(events[0].kind(), EventKind::NewContact);
        assert_eq!(state.contacts, Vec::<Contact>::new());
        assert_eq!(state.pending_contacts.len(), 1);
    }

    #[test]
    fn contact_deleted_removes_from_the_cache() {
        let mut state = State::new();
        state.handle_frame(&contact_frame(3, 1, "a"));
        assert_eq!(state.contacts.len(), 1);
        let mut frame = vec![0x8f];
        frame.extend([1; 32]);
        state.handle_frame(&frame);
        assert_eq!(state.contacts, Vec::<Contact>::new());
    }

    #[test]
    fn error_frames_carry_the_code() {
        let mut state = State::new();
        let events = state.handle_frame(&[1, 6]);
        let Event::Error(info) = &events[0] else {
            panic!()
        };
        assert_eq!(info.code, Some(ErrorCode::IllegalArg));
    }

    #[test]
    fn unreadable_replies_become_errors_and_pushes_are_dropped() {
        let mut state = State::new();
        // MSG_SENT cut short
        let events = state.handle_frame(&[6, 0, 1, 2]);
        let Event::Error(info) = &events[0] else {
            panic!()
        };
        assert_eq!(info.packet_type, Some(6));
        assert_eq!(info.reason.as_deref(), Some("invalid_frame_length"));
        // a STATUS_RESPONSE push cut short
        assert!(state.handle_frame(&[0x87, 0, 1, 2]).is_empty());
        // an unknown code
        assert_eq!(state.handle_frame(&[0x7f, 1])[0].kind(), EventKind::Unknown);
    }

    #[test]
    fn binary_responses_are_parsed_by_their_tag() {
        let mut state = State::new();
        state.prepared = Some(PreparedRequest {
            kind: RequestKind::Binary(BinaryReqType::Acl),
            pubkey_prefix: [1, 2, 3, 4, 5, 6],
            timeout: None,
            min_timeout: Duration::ZERO,
            timeout_factor: 1,
            neighbours_prefix_len: 0,
        });
        let mut sent = vec![6, 0];
        sent.extend(0xabcd_u32.to_le_bytes());
        sent.extend(4000u32.to_le_bytes());
        state.handle_frame(&sent);
        assert!(state.prepared.is_none());

        let mut response = vec![0x8c, 0];
        response.extend(0xabcd_u32.to_le_bytes());
        response.extend([7, 7, 7, 7, 7, 7, 2]);
        response.extend([0; 9]);
        let events = state.handle_frame(&response);
        assert_eq!(events.len(), 2);
        let Event::AclResponse(acl) = &events[1] else {
            panic!()
        };
        assert_eq!(acl.pubkey_prefix, [1, 2, 3, 4, 5, 6]);
        assert_eq!(acl.entries.len(), 1);
        // matched once
        assert_eq!(state.handle_frame(&response).len(), 1);
    }

    #[test]
    fn a_neighbours_answer_cut_short_comes_only_raw() {
        // meshcore_py reports the neighbours it could read
        let mut state = State::new();
        state.prepared = Some(PreparedRequest {
            kind: RequestKind::Binary(BinaryReqType::Neighbours),
            pubkey_prefix: [1, 2, 3, 4, 5, 6],
            timeout: None,
            min_timeout: Duration::ZERO,
            timeout_factor: 1,
            neighbours_prefix_len: 4,
        });
        let mut sent = vec![6, 0];
        sent.extend(0xabcd_u32.to_le_bytes());
        sent.extend(4000u32.to_le_bytes());
        state.handle_frame(&sent);

        // two neighbours announced, the second without its SNR
        let mut response = vec![0x8c, 0];
        response.extend(0xabcd_u32.to_le_bytes());
        response.extend(2i16.to_le_bytes());
        response.extend(2i16.to_le_bytes());
        response.extend([0xa1; 4]);
        response.extend(30i32.to_le_bytes());
        response.push(8);
        response.extend([0xa2; 4]);
        response.extend(30i32.to_le_bytes());
        let events = state.handle_frame(&response);
        assert_eq!(events.len(), 1);
        let Event::BinaryResponse(raw) = &events[0] else {
            panic!()
        };
        assert_eq!(raw.data, response[6..]);
    }

    #[test]
    fn another_device_does_not_get_the_state_of_the_one_before() {
        let mut state = State::new();
        state.parser.decrypt_channels = true;
        state.handle_frame(&self_info_frame());
        state.handle_frame(&contact_frame(3, 1, "a"));
        state.handle_frame(&contact_frame(0x8a, 5, "new"));
        state.handle_frame(&[9, 0x10, 0, 0, 0]);
        state.contacts_lastmod = 5;
        state.contacts_listed = true;
        state.contacts_dirty = false;
        let channel = crate::ChannelInfo {
            channel_idx: 0,
            channel_name: "#x".into(),
            channel_secret: [1; 16],
        };
        state.parser.add_channel(&channel);
        // the same device, as after a reconnect
        state.handle_frame(&self_info_frame());
        assert_eq!(state.contacts.len(), 1);
        assert!(!state.contacts_dirty);
        assert!(state.parser.knows_channels());
        // one with another key
        let mut other = self_info_frame();
        other[4] = 8;
        state.handle_frame(&other);
        assert_eq!(state.self_info.as_ref().unwrap().public_key.prefix()[0], 8);
        assert_eq!(state.name_len, 4);
        assert_eq!(state.contacts, Vec::<Contact>::new());
        assert_eq!(state.pending_contacts, Vec::<Contact>::new());
        assert_eq!((state.contacts_lastmod, state.time), (0, 0));
        assert!(state.contacts_dirty && !state.contacts_listed);
        assert!(!state.parser.knows_channels());
        // the client's own settings stay
        assert!(state.parser.decrypt_channels && state.parser.keep_channel_secrets());
    }

    #[test]
    fn time_and_self_info_are_tracked() {
        let mut state = State::new();
        state.handle_frame(&[9, 0x10, 0, 0, 0]);
        assert_eq!(state.time, 16);

        assert_eq!(
            state.handle_frame(&self_info_frame())[0].kind(),
            EventKind::SelfInfo
        );
        let info = state.self_info.as_ref().unwrap();
        assert_eq!(info.public_key, PublicKey::new([7; 32]));
        assert_eq!((info.radio_sf, info.radio_cr), (11, 5));
        assert_eq!(info.name, "Node");
    }
}
