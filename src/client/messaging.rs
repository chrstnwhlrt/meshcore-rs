//! Messaging commands, logins, path discovery, traces and flood scopes.

use std::time::Duration;

use super::binary::RequestOptions;
use super::{MeshCore, reply, wait_response};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::protocol::codes::{CommandType, ControlType};
use crate::protocol::path::PathLen;
use crate::types::{
    ContactType, DefaultFloodScope, Destination, LoginSuccess, PathDiscovery, PublicKey,
    ReceivedMessage, SentInfo, TextType, TraceData,
};
use crate::util::{random_u32, unix_now};

/// The largest channel datagram payload (`MAX_CHANNEL_DATA_LENGTH` of the
/// firmware with 172-byte frames, which all versions accept).
pub const MAX_CHANNEL_DATA_LENGTH: usize = 163;

/// The longest text of a message or CLI command in bytes (the firmware's
/// `MAX_TEXT_LEN`). In a channel message the sender's name and `": "` count
/// against it too.
///
/// Companion firmware before 1.16 takes frames of 172 bytes instead of 176.
/// A device with such firmware sends a private message of at most 159 bytes
/// (the command for a longer one does not fit), and of one it receives it
/// passes at most 156 bytes to its app (159 without v3 frames).
pub const MAX_TEXT_LENGTH: usize = 160;

/// Refuses a text the firmware would reject or cut short.
fn check_text(text: &str, limit: usize, what: &str) -> Result<()> {
    if text.is_empty() {
        return Err(Error::invalid(format!("the {what} is empty")));
    }
    // The firmware handles texts as C strings and would end it there.
    if text.contains('\0') {
        return Err(Error::invalid(format!("the {what} holds a NUL byte")));
    }
    if text.len() > limit {
        return Err(Error::invalid(format!(
            "{what} of {} bytes, at most {limit} fit",
            text.len()
        )));
    }
    Ok(())
}

/// Settings of [`MeshCore::send_msg_with_retry`] (defaults as meshcore_py).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryOptions {
    /// The message timestamp; the same on every attempt (default: now).
    pub timestamp: Option<u32>,
    /// Attempts in all.
    pub max_attempts: u32,
    /// Attempts once the message is flooded.
    pub max_flood_attempts: u32,
    /// After this many attempts the path is reset and the message flooded.
    pub flood_after: u32,
    /// How long to wait for the ACK of an attempt (default: the device's
    /// suggestion plus a fifth).
    pub timeout: Option<Duration>,
    /// The least time to wait for an ACK.
    pub min_timeout: Duration,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self {
            timestamp: None,
            max_attempts: 3,
            max_flood_attempts: 2,
            flood_after: 2,
            timeout: None,
            min_timeout: Duration::ZERO,
        }
    }
}

/// A flood scope for [`MeshCore::set_flood_scope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FloodScope {
    /// Back to the default scope.
    Default,
    /// Flood without any scope.
    Unscoped,
    /// A named scope; its key is the first 16 bytes of the SHA-256 of the
    /// name with a leading `#` (added when missing).
    Name(String),
    /// A raw scope key.
    Key([u8; 16]),
}

impl FloodScope {
    /// meshcore_py's string form: `""`, `"0"` or `"None"` for the default,
    /// `"*"` for unscoped, anything else is a name.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "" | "0" | "None" => Self::Default,
            "*" => Self::Unscoped,
            name => Self::Name(name.to_string()),
        }
    }

    /// The transport key of a scope name.
    #[must_use]
    pub fn key_for_name(name: &str) -> [u8; 16] {
        crate::types::channel_secret_from_name(&hashtag(name))
    }
}

fn hashtag(name: &str) -> String {
    if name.starts_with('#') {
        name.to_string()
    } else {
        format!("#{name}")
    }
}

/// A trace request for [`MeshCore::send_trace`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraceRequest {
    /// The auth code (meshcore_py default 0).
    pub auth_code: u32,
    /// The tag; random when `None`.
    pub tag: Option<u32>,
    /// The flags: the low 2 bits give the hash size `1 << n`. `None` means 0.
    pub flags: Option<u8>,
    /// The hop hashes, each `1 << (flags & 3)` bytes.
    pub path: Vec<u8>,
}

/// Parses a trace path as meshcore_py takes it: comma-separated hex hashes
/// (`"23,5f,3a"`) up to the first empty element, all as long as the first
/// element as written (1, 2, 4 or 8 bytes). Returns the path and the flags
/// that give its hash size.
///
/// # Errors
///
/// [`Error::InvalidArgument`] for bad hex or mixed or unsupported hash sizes,
/// an empty first element included.
pub fn parse_trace_path(text: &str) -> Result<(Vec<u8>, u8)> {
    // meshcore_py takes the hash size from the first element before trimming.
    let hash_len = text.split(',').next().map_or(0, str::len) / 2;
    let flags = match hash_len {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        other => {
            return Err(Error::invalid(format!(
                "unsupported trace hash size {other}"
            )));
        }
    };
    let mut path = Vec::new();
    // meshcore_py ends the path at the first empty element.
    for part in text.split(',').map(str::trim).take_while(|p| !p.is_empty()) {
        if part.len() != hash_len * 2 {
            return Err(Error::invalid(format!(
                "trace hash {part:?} is not {} hex digits",
                hash_len * 2
            )));
        }
        path.extend(
            crate::hex::decode(part)
                .ok_or_else(|| Error::invalid(format!("{part:?} is not hex")))?,
        );
    }
    Ok((path, flags))
}

/// The path length byte and path for commands that take an optional path.
fn path_with_len(path: &[u8], hash_mode: u8) -> Result<Vec<u8>> {
    let hash_size = usize::from(hash_mode) + 1;
    if !path.len().is_multiple_of(hash_size) {
        return Err(Error::invalid("path is not a whole number of hops"));
    }
    let hops = u8::try_from(path.len() / hash_size).map_err(|_| Error::invalid("path too long"))?;
    let mut out = vec![PathLen::new(hops, hash_mode)?.as_byte()];
    out.extend_from_slice(path);
    Ok(out)
}

impl MeshCore {
    /// The next queued message, or `None` when there are no more.
    pub async fn get_msg(&self) -> Result<Option<ReceivedMessage>> {
        self.get_msg_timeout(self.default_timeout()).await
    }

    /// [`get_msg`](Self::get_msg) with a timeout.
    pub async fn get_msg_timeout(&self, timeout: Duration) -> Result<Option<ReceivedMessage>> {
        let frame = [CommandType::SyncNextMessage.code()];
        self.request(&frame, timeout, |event| match event {
            Event::ContactMessage(m) => Some(Some(ReceivedMessage::Contact((**m).clone()))),
            Event::ChannelMessage(m) => Some(Some(ReceivedMessage::Channel((**m).clone()))),
            Event::ChannelData(d) => Some(Some(ReceivedMessage::ChannelData((**d).clone()))),
            Event::NoMoreMessages => Some(None),
            _ => None,
        })
        .await
    }

    /// Fetches all queued messages.
    pub async fn fetch_messages(&self) -> Result<Vec<ReceivedMessage>> {
        let mut messages = Vec::new();
        while let Some(message) = self.get_msg().await? {
            messages.push(message);
        }
        Ok(messages)
    }

    /// Sends a private message of at most [`MAX_TEXT_LENGTH`] bytes (2 fewer
    /// from the fifth attempt on). The reply carries the ACK code to expect.
    pub async fn send_msg(
        &self,
        dst: impl Into<Destination>,
        text: &str,
        timestamp: Option<u32>,
        attempt: u8,
    ) -> Result<SentInfo> {
        let limit = if attempt > 3 {
            MAX_TEXT_LENGTH - 2
        } else {
            MAX_TEXT_LENGTH
        };
        check_text(text, limit, "message")?;
        let dst = dst.into();
        let mut frame = vec![
            CommandType::SendTxtMsg.code(),
            TextType::Plain.code(),
            attempt,
        ];
        frame.extend_from_slice(&timestamp.unwrap_or_else(unix_now).to_le_bytes());
        frame.extend_from_slice(&dst.prefix());
        frame.extend_from_slice(text.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Sends a CLI command (at most [`MAX_TEXT_LENGTH`] bytes) to a node.
    /// Companions (chat nodes) get `TXT_TYPE_CLI_COMMAND`, others
    /// `TXT_TYPE_CLI_DATA`. Without a type, the destination's or the cached
    /// contact's type is used; unknown is taken for a repeater.
    pub async fn send_cmd(
        &self,
        dst: impl Into<Destination>,
        command: &str,
        timestamp: Option<u32>,
        dst_type: Option<ContactType>,
    ) -> Result<SentInfo> {
        check_text(command, MAX_TEXT_LENGTH, "command")?;
        let dst = dst.into();
        let dst_type = dst_type
            .or_else(|| dst.contact_type())
            .or_else(|| {
                self.state()
                    .contact_by_prefix(dst.key_bytes())
                    .map(|c| c.contact_type)
            })
            .unwrap_or_else(|| {
                log::warn!(
                    target: "meshcore",
                    "Can't determine destination type, please ensure contacts first or specify \
                     dst_type when calling `send_cmd`. Assuming it's a repeater."
                );
                ContactType::Repeater
            });
        let txt_type = if dst_type == ContactType::Chat {
            TextType::CliCommand
        } else {
            TextType::CliData
        };
        let mut frame = vec![CommandType::SendTxtMsg.code(), txt_type.code(), 0];
        frame.extend_from_slice(&timestamp.unwrap_or_else(unix_now).to_le_bytes());
        frame.extend_from_slice(&dst.prefix());
        frame.extend_from_slice(command.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Sends a message and waits for its ACK, retrying; after
    /// `flood_after` attempts the path is reset and the message flooded. A
    /// key prefix needs its contact for the reset: when even the contacts
    /// read then do not hold it, the attempts go on without the reset.
    /// Returns the [`SentInfo`] of the attempt that was acknowledged, or
    /// `None` when none was, also when attempts could not be sent (their
    /// errors are logged). Fails only with [`Error::InvalidArgument`], for a
    /// text the firmware would not take.
    pub async fn send_msg_with_retry(
        &self,
        dst: impl Into<Destination>,
        text: &str,
        options: RetryOptions,
    ) -> Result<Option<SentInfo>> {
        // From the fifth attempt on the text has to be 2 bytes shorter: check
        // the strictest limit the attempts may reach before sending any.
        let limit = if options.max_attempts > 4 {
            MAX_TEXT_LENGTH - 2
        } else {
            MAX_TEXT_LENGTH
        };
        check_text(text, limit, "message")?;
        let dst = dst.into();
        let mut full_key = dst.public_key().or_else(|| {
            self.state()
                .contact_by_prefix(dst.key_bytes())
                .map(|c| c.public_key)
        });
        let timestamp = options.timestamp.unwrap_or_else(unix_now);
        // Subscribed before sending: the ACK can come right behind MSG_SENT.
        let mut sub = self.subscribe();
        let mut sent: Vec<SentInfo> = Vec::new();
        let mut attempts = 0u32;
        let mut flood_attempts = 0u32;
        let mut flood = false;

        while attempts < options.max_attempts
            && (!flood || flood_attempts < options.max_flood_attempts)
        {
            // The log texts at info and above are meshcore_py's, spelling
            // included, so that both read the same.
            if attempts == options.flood_after && !flood {
                if full_key.is_none() {
                    log::info!(
                        target: "meshcore",
                        "Don't have full key, retreiving contacts so we can reset path"
                    );
                    if let Err(err) = self.get_contacts(0).await {
                        log::debug!(target: "meshcore", "could not read contacts: {err}");
                    }
                    full_key = self
                        .state()
                        .contact_by_prefix(dst.key_bytes())
                        .map(|c| c.public_key);
                }
                match full_key {
                    Some(key) => {
                        log::info!(target: "meshcore", "Resetting path");
                        match self.reset_path(&key).await {
                            Ok(()) => flood = true,
                            Err(err) => {
                                log::debug!(target: "meshcore", "could not reset path: {err}");
                                log::error!(
                                    target: "meshcore",
                                    "Couldn't reset path Event(type=<EventType.ERROR: \
                                     'command_error'>, payload={}, attributes={{}}) continuing ...",
                                    super::python_payload(&err)
                                );
                            }
                        }
                    }
                    None => {
                        log::error!(target: "meshcore", "no full key to reset the path, continuing");
                    }
                }
            }
            if attempts > 0 {
                log::info!(target: "meshcore", "Retry sending msg: {}", attempts + 1);
            }
            let attempt = u8::try_from(attempts).unwrap_or(u8::MAX);
            match self
                .send_msg(dst.clone(), text, Some(timestamp), attempt)
                .await
            {
                // A text that does not fit will not fit on the next attempt.
                Err(err @ Error::InvalidArgument(_)) => return Err(err),
                Err(err) => {
                    log::debug!(target: "meshcore", "could not send the message: {err}");
                    log::error!(
                        target: "meshcore",
                        "Failed to send message: {}",
                        super::python_payload(&err)
                    );
                }
                Ok(info) => {
                    flood = info.flood;
                    sent.push(info);
                    let wait = options
                        .timeout
                        .unwrap_or_else(|| {
                            Duration::from_micros(u64::from(info.suggested_timeout) * 1200)
                        })
                        .max(options.min_timeout);
                    // An ACK for any earlier attempt counts too.
                    let acked = super::wait_event(&mut sub, wait, |event| {
                        matches!(event, Event::Ack(ack) if sent.iter().any(|s| s.expected_ack == ack.code))
                    })
                    .await;
                    if let Some(Event::Ack(ack)) = acked {
                        return Ok(sent.into_iter().find(|s| s.expected_ack == ack.code));
                    }
                }
            }
            attempts += 1;
            if flood {
                flood_attempts += 1;
            }
        }
        Ok(None)
    }

    /// Sends a channel message. The device sends it as `"<node name>: <text>"`
    /// in at most [`MAX_TEXT_LENGTH`] bytes and would cut a longer text short,
    /// so a text that does not fit is refused.
    pub async fn send_chan_msg(
        &self,
        channel: u8,
        text: &str,
        timestamp: Option<u32>,
    ) -> Result<()> {
        // An empty channel message is fine with the firmware ("<name>: "). The
        // node name is known once connected; before, the limit is the one for
        // the shortest name.
        let name_len = self
            .state()
            .self_info
            .as_ref()
            .map_or(0, |info| info.name.len());
        if !text.is_empty() {
            let limit = MAX_TEXT_LENGTH.saturating_sub(name_len + 2);
            check_text(text, limit, "channel message")?;
        }
        let mut frame = vec![
            CommandType::SendChannelTxtMsg.code(),
            TextType::Plain.code(),
            channel,
        ];
        frame.extend_from_slice(&timestamp.unwrap_or_else(unix_now).to_le_bytes());
        frame.extend_from_slice(text.as_bytes());
        self.request_ok(&frame).await.map(drop)
    }

    /// Sends a login to a repeater, room server or sensor without waiting for
    /// the answer. The password has at most 15 bytes, the most the firmware
    /// sends (empty for a guest login). Like meshcore_py, it logs a hint to
    /// use [`send_login_sync`](Self::send_login_sync).
    pub async fn send_login(&self, dst: &PublicKey, password: &str) -> Result<SentInfo> {
        log::warn!(target: "meshcore", "*** please consider using send_login_sync instead of send_login");
        self.send_login_raw(dst, password).await
    }

    async fn send_login_raw(&self, dst: &PublicKey, password: &str) -> Result<SentInfo> {
        if password.len() > 15 || password.contains('\0') {
            return Err(Error::invalid(
                "a login password has at most 15 bytes and no NUL, the firmware cuts it there",
            ));
        }
        let mut frame = vec![CommandType::SendLogin.code()];
        frame.extend_from_slice(dst.as_bytes());
        frame.extend_from_slice(password.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Logs in and waits for the answer; [`Error::LoginFailed`] when refused.
    pub async fn send_login_sync(
        &self,
        dst: &PublicKey,
        password: &str,
        options: RequestOptions,
    ) -> Result<LoginSuccess> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let mut sub = self.subscribe();
        let sent = self.send_login_raw(dst, password).await?;
        let prefix = dst.prefix();
        let matches = |p: Option<[u8; 6]>| p.is_none_or(|p| p == prefix);
        wait_response(
            &mut sub,
            options.wait(sent.suggested_timeout),
            |event| match event {
                Event::LoginSuccess(login) if matches(login.pubkey_prefix) => Some(Ok(*login)),
                Event::LoginFailed(failed) if matches(failed.pubkey_prefix) => {
                    Some(Err(Error::LoginFailed))
                }
                _ => None,
            },
        )
        .await?
    }

    /// Ends the keep-alive connection to a server.
    pub async fn send_logout(&self, dst: &PublicKey) -> Result<()> {
        let mut frame = vec![CommandType::Logout.code()];
        frame.extend_from_slice(dst.as_bytes());
        self.request_ok(&frame).await.map(drop)
    }

    /// Asks a node for its status the old way; the answer arrives as
    /// [`Event::StatusResponse`]. Prefer [`req_status_sync`](Self::req_status_sync)
    /// (this logs a hint to it, as meshcore_py).
    pub async fn send_statusreq(&self, dst: &PublicKey) -> Result<SentInfo> {
        log::warn!(target: "meshcore", "*** please consider using req_status_sync instead of send_statusreq");
        let mut frame = vec![CommandType::SendStatusReq.code()];
        frame.extend_from_slice(dst.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Asks a node for telemetry the old way; the answer arrives as
    /// [`Event::TelemetryResponse`]. Prefer [`req_telemetry_sync`](Self::req_telemetry_sync)
    /// (this logs a hint to it, as meshcore_py).
    pub async fn send_telemetry_req(&self, dst: &PublicKey) -> Result<SentInfo> {
        log::warn!(
            target: "meshcore",
            "*** please consider using req_telemetry_sync instead of send_telemetry_req"
        );
        let mut frame = vec![CommandType::SendTelemetryReq.code(), 0, 0, 0];
        frame.extend_from_slice(dst.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Starts a path discovery; the answer arrives as [`Event::PathResponse`].
    /// Like meshcore_py, it logs a hint to use
    /// [`send_path_discovery_sync`](Self::send_path_discovery_sync).
    pub async fn send_path_discovery(&self, dst: &PublicKey) -> Result<SentInfo> {
        log::warn!(
            target: "meshcore",
            "*** please consider using send_path_discovery_sync instead of send_path_discovery"
        );
        self.send_path_discovery_raw(dst).await
    }

    async fn send_path_discovery_raw(&self, dst: &PublicKey) -> Result<SentInfo> {
        let mut frame = vec![CommandType::SendPathDiscoveryReq.code(), 0];
        frame.extend_from_slice(dst.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Discovers the paths to and from a node and waits for the answer.
    pub async fn send_path_discovery_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<PathDiscovery> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let mut sub = self.subscribe();
        let sent = self.send_path_discovery_raw(dst).await?;
        let prefix = dst.prefix();
        wait_response(
            &mut sub,
            options.wait(sent.suggested_timeout),
            reply!(Event::PathResponse(p) if p.pubkey_prefix == prefix => (**p).clone()),
        )
        .await
    }

    /// Sends a trace along a path; the answer arrives as [`Event::TraceData`]
    /// with the tag in [`SentInfo::expected_ack`].
    pub async fn send_trace(&self, request: &TraceRequest) -> Result<SentInfo> {
        // The firmware traces only along a path of at least one hop.
        if request.path.is_empty() {
            return Err(Error::invalid("a trace needs a path of at least one hop"));
        }
        let tag = request.tag.unwrap_or_else(random_u32);
        let mut frame = vec![CommandType::SendTracePath.code()];
        frame.extend_from_slice(&tag.to_le_bytes());
        frame.extend_from_slice(&request.auth_code.to_le_bytes());
        frame.push(request.flags.unwrap_or(0));
        frame.extend_from_slice(&request.path);
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::MessageSent(s) => *s),
        )
        .await
    }

    /// Sends a trace and waits for its result.
    pub async fn trace_sync(
        &self,
        request: &TraceRequest,
        options: RequestOptions,
    ) -> Result<TraceData> {
        let mut sub = self.subscribe();
        let sent = self.send_trace(request).await?;
        wait_response(
            &mut sub,
            options.wait(sent.suggested_timeout),
            reply!(Event::TraceData(t) if t.tag == sent.expected_ack => (**t).clone()),
        )
        .await
    }

    /// Sends raw data (at least 4 bytes) direct along a path (empty: zero-hop),
    /// which takes whole hops as in
    /// [`change_contact_path`](Self::change_contact_path).
    pub async fn send_raw_data(&self, payload: &[u8], path: &[u8], hash_mode: u8) -> Result<()> {
        if payload.len() < 4 {
            return Err(Error::invalid("raw data needs at least 4 bytes"));
        }
        let mut frame = vec![CommandType::SendRawData.code()];
        frame.extend(path_with_len(path, hash_mode)?);
        frame.extend_from_slice(payload);
        self.request_ok(&frame).await.map(drop)
    }

    /// Sends a binary datagram to a channel, flooded or along a path of whole
    /// hops as in [`change_contact_path`](Self::change_contact_path).
    /// `data_type` 0 is reserved; the payload takes at most
    /// [`MAX_CHANNEL_DATA_LENGTH`] bytes, fewer with a path (the command has
    /// to fit the device's 176-byte frame).
    pub async fn send_channel_data(
        &self,
        channel: u8,
        data_type: u16,
        payload: &[u8],
        path: Option<&[u8]>,
        hash_mode: u8,
    ) -> Result<()> {
        if data_type == 0 {
            return Err(Error::invalid("data type 0 is reserved"));
        }
        if payload.len() > MAX_CHANNEL_DATA_LENGTH {
            return Err(Error::invalid(format!(
                "payload of {} bytes, at most {MAX_CHANNEL_DATA_LENGTH}",
                payload.len()
            )));
        }
        let mut frame = vec![CommandType::SendChannelData.code(), channel];
        match path {
            None => frame.push(PathLen::NONE.as_byte()),
            Some(path) => frame.extend(path_with_len(path, hash_mode)?),
        }
        frame.extend_from_slice(&data_type.to_le_bytes());
        frame.extend_from_slice(payload);
        self.request_ok(&frame).await.map(drop)
    }

    /// Sets the scope of flooded messages.
    pub async fn set_flood_scope(&self, scope: &FloodScope) -> Result<()> {
        let mut frame = vec![CommandType::SetFloodScope.code()];
        match scope {
            FloodScope::Unscoped => frame.push(1),
            FloodScope::Default => {
                frame.push(0);
                frame.extend_from_slice(&[0; 16]);
            }
            FloodScope::Name(name) => {
                frame.push(0);
                frame.extend_from_slice(&FloodScope::key_for_name(name));
            }
            FloodScope::Key(key) => {
                frame.push(0);
                frame.extend_from_slice(key);
            }
        }
        self.request_ok(&frame).await.map(drop)
    }

    /// [`set_flood_scope`](Self::set_flood_scope) with meshcore_py's string form.
    pub async fn set_flood_scope_str(&self, scope: &str) -> Result<()> {
        self.set_flood_scope(&FloodScope::parse(scope)).await
    }

    /// Back to the default flood scope.
    pub async fn reset_flood_scope(&self) -> Result<()> {
        self.set_flood_scope(&FloodScope::Default).await
    }

    /// Floods without any scope.
    pub async fn force_unscoped(&self) -> Result<()> {
        self.set_flood_scope(&FloodScope::Unscoped).await
    }

    /// Sets the default flood scope by name (a `#` is added when missing);
    /// `None`, `""`, `"0"`, `"None"` or `"*"` clear it.
    pub async fn set_default_flood_scope(&self, scope: Option<&str>) -> Result<()> {
        let mut frame = vec![CommandType::SetDefaultFloodScope.code()];
        if let Some(scope) = scope.filter(|s| !matches!(*s, "" | "0" | "None" | "*")) {
            let name = hashtag(scope);
            if name.len() >= 31 {
                return Err(Error::invalid("a scope name must be shorter than 31 bytes"));
            }
            // The device would keep the name up to the NUL, the key comes from all of it.
            if name.contains('\0') {
                return Err(Error::invalid("the scope name holds a NUL byte"));
            }
            let mut field = [0u8; 31];
            field[..name.len()].copy_from_slice(name.as_bytes());
            frame.extend_from_slice(&field);
            frame.extend_from_slice(&FloodScope::key_for_name(&name));
        }
        self.request_ok(&frame).await.map(drop)
    }

    /// Clears the default flood scope.
    pub async fn reset_default_flood_scope(&self) -> Result<()> {
        self.set_default_flood_scope(None).await
    }

    /// The default flood scope, `None` when unset.
    pub async fn get_default_flood_scope(&self) -> Result<Option<DefaultFloodScope>> {
        let frame = [CommandType::GetDefaultFloodScope.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::DefaultFloodScope(s) => s.clone()),
        )
        .await
    }

    /// Sends control data zero-hop; the type needs its top bit set.
    pub async fn send_control_data(&self, control_type: u8, payload: &[u8]) -> Result<()> {
        if control_type & 0x80 == 0 {
            return Err(Error::invalid(format!(
                "control type {control_type:#04x} without its top bit"
            )));
        }
        let mut frame = vec![CommandType::SendControlData.code(), control_type];
        frame.extend_from_slice(payload);
        self.request_ok(&frame).await.map(drop)
    }

    /// Asks nodes nearby to identify themselves; answers arrive as
    /// [`Event::DiscoverResponse`] with the returned tag. `filter` is a bit
    /// mask of node types (`1 << type`).
    pub async fn send_node_discover_req(
        &self,
        filter: u8,
        prefix_only: bool,
        tag: Option<u32>,
        since: Option<u32>,
    ) -> Result<u32> {
        let tag = tag.unwrap_or_else(random_u32);
        let mut payload = vec![filter];
        payload.extend_from_slice(&tag.to_le_bytes());
        if let Some(since) = since {
            payload.extend_from_slice(&since.to_le_bytes());
        }
        let control_type = ControlType::NodeDiscoverReq.code() | u8::from(prefix_only);
        self.send_control_data(control_type, &payload).await?;
        Ok(tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_paths() {
        // meshcore_py's send_trace gives the same for each of these
        let parsed = [
            ("23,5f,3a", Some((vec![0x23, 0x5f, 0x3a], 0))),
            ("2301,5f02", Some((vec![0x23, 1, 0x5f, 2], 1))),
            ("23,5f,", Some((vec![0x23, 0x5f], 0))),
            ("23,,5f", Some((vec![0x23], 0))),
            (" 23, 5f", Some((vec![0x23, 0x5f], 0))),
            ("AB,cd", Some((vec![0xab, 0xcd], 0))),
            ("  23,5f", None),
            ("", None),
            (",23", None),
            ("23,5f02", None),
            ("235f02", None),
            ("23,zz", None),
        ];
        for (text, expected) in parsed {
            assert_eq!(parse_trace_path(text).ok(), expected, "{text:?}");
        }
        let Err(Error::InvalidArgument(message)) = parse_trace_path("abc") else {
            panic!()
        };
        assert_eq!(message, "trace hash \"abc\" is not 2 hex digits");
    }

    #[test]
    fn flood_scope_strings() {
        assert_eq!(FloodScope::parse("0"), FloodScope::Default);
        assert_eq!(FloodScope::parse("*"), FloodScope::Unscoped);
        assert_eq!(
            FloodScope::key_for_name("test"),
            FloodScope::key_for_name("#test")
        );
    }

    #[test]
    fn optional_paths() {
        assert_eq!(path_with_len(&[], 0).unwrap(), vec![0]);
        assert_eq!(
            path_with_len(&[0xaa, 0xbb], 1).unwrap(),
            vec![0x41, 0xaa, 0xbb]
        );
        assert!(path_with_len(&[0xaa], 1).is_err());
    }
}
