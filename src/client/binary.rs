//! Binary and anonymous requests to remote nodes.
//!
//! The answer to a request carries the tag of the request's `MSG_SENT`
//! reply. The reader binds the tag to the request as soon as `MSG_SENT`
//! arrives, so the answer is parsed by its type ([`Event::StatusResponse`],
//! [`Event::TelemetryResponse`], ...) besides the raw [`Event::BinaryResponse`].

use std::time::Duration;

use super::{MeshCore, reply, wait_reply, wait_response};
use crate::error::{Error, Result};
use crate::event::{Event, Subscription};
use crate::lpp::Telemetry;
use crate::protocol::bytes::lossy_ignore;
use crate::protocol::codes::{AnonReqType, BinaryReqType, CommandType};
use crate::protocol::path::{PathLen, encode_reply_path};
use crate::state::{PreparedRequest, RequestKind, State};
use crate::types::{
    AclEntry, AnonOwner, BinaryResponse, MmaEntry, NeighboursQuery, NeighboursResponse, OwnerInfo,
    PublicKey, SensorSubscription, SentInfo, StatusResponse,
};

/// How long to wait for a remote node's answer.
///
/// By default the device's suggested timeout plus a quarter, as meshcore_py's
/// `suggested_timeout / 800`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestOptions {
    /// A fixed timeout instead of the suggested one (`Duration::MAX` waits
    /// without limit).
    pub timeout: Option<Duration>,
    /// The least time to wait.
    pub min_timeout: Duration,
}

impl RequestOptions {
    /// A fixed timeout.
    #[must_use]
    pub const fn timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            min_timeout: Duration::ZERO,
        }
    }

    /// The suggested timeout, but at least `min_timeout`.
    #[must_use]
    pub const fn min_timeout(min_timeout: Duration) -> Self {
        Self {
            timeout: None,
            min_timeout,
        }
    }

    /// The wait for a request the device suggests `suggested_ms` for.
    #[must_use]
    pub fn wait(&self, suggested_ms: u32) -> Duration {
        self.timeout
            .unwrap_or_else(|| crate::util::suggested_wait(suggested_ms))
            .max(self.min_timeout)
    }
}

/// Sets a contact that went zero-hop for an anonymous request back to flood
/// when the request is given up halfway (its future dropped, as by a timeout
/// around it): the device would keep the zero-hop path. Armed while `client`
/// is set.
struct FloodAgain {
    client: Option<MeshCore>,
    key: PublicKey,
}

impl Drop for FloodAgain {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        let key = self.key;
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(err) = client.reset_path(&key).await {
                        log::warn!(target: "meshcore", "could not reset the path to flood: {err}");
                    }
                });
            }
            Err(_) => log::warn!(
                target: "meshcore",
                "no runtime to reset the path of {key} to flood"
            ),
        }
    }
}

impl MeshCore {
    /// Sends a binary request (`CMD_SEND_BINARY_REQ`). The answer arrives as
    /// [`Event::BinaryResponse`] and, for status, telemetry, MMA, ACL and
    /// neighbours requests, as the typed event, both with the tag
    /// [`SentInfo::expected_ack`].
    pub async fn send_binary_req(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
    ) -> Result<SentInfo> {
        let (sent, _) = self
            .send_binary_raw(dst, request_type, data, options)
            .await?;
        Ok(sent)
    }

    async fn send_binary_raw(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
    ) -> Result<(SentInfo, Subscription)> {
        // A neighbours answer lists key prefixes of the length the request
        // asks for, which the repeater reads from the sixth byte of the data.
        let neighbours_prefix_len = match request_type {
            BinaryReqType::Neighbours => data.get(5).copied().unwrap_or(0),
            _ => 0,
        };
        let mut frame = vec![CommandType::SendBinaryReq.code()];
        frame.extend_from_slice(dst.as_bytes());
        frame.push(request_type.code());
        frame.extend_from_slice(data);
        self.send_tracked(
            &frame,
            PreparedRequest {
                kind: RequestKind::Binary(request_type),
                pubkey_prefix: dst.prefix(),
                timeout: options.timeout,
                min_timeout: options.min_timeout,
                timeout_factor: 1,
                neighbours_prefix_len,
            },
        )
        .await
    }

    /// Sends a request whose answer is matched by tag; the tag is bound
    /// before any later frame is read.
    async fn send_tracked(
        &self,
        frame: &[u8],
        prepared: PreparedRequest,
    ) -> Result<(SentInfo, Subscription)> {
        let mut turn = self.turn().await;
        let expect = |state: &mut State| state.prepared = Some(prepared);
        let (link, mut events) = turn.send(frame, expect).await?;
        let sent = reply!(Event::MessageSent(sent) => *sent);
        let sent = wait_reply(link, &mut events, self.default_timeout(), sent).await?;
        Ok((sent, events))
    }

    /// Sends a binary request and waits for its typed answer.
    async fn binary_request<T>(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
        extract: impl Fn(&Event, u32) -> Option<T>,
    ) -> Result<T> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let (sent, events) = self
            .send_binary_raw(dst, request_type, data, options)
            .await?;
        let tag = sent.expected_ack;
        wait_response(events, options.wait(sent.suggested_timeout), |event| {
            extract(event, tag)
        })
        .await
    }

    /// The status of a repeater or room server.
    pub async fn req_status_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<StatusResponse> {
        self.binary_request(
            dst,
            BinaryReqType::Status,
            &[],
            options,
            |event, tag| match event {
                Event::StatusResponse(s) if s.tag == Some(tag) => Some((**s).clone()),
                _ => None,
            },
        )
        .await
    }

    /// A node's telemetry.
    pub async fn req_telemetry_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<Telemetry> {
        self.binary_request(
            dst,
            BinaryReqType::Telemetry,
            &[],
            options,
            |event, tag| match event {
                Event::TelemetryResponse(t) if t.tag == Some(tag) => Some(t.telemetry.clone()),
                _ => None,
            },
        )
        .await
    }

    /// A sensor's min/max/average series between `start` and `end` seconds ago.
    pub async fn req_mma_sync(
        &self,
        dst: &PublicKey,
        start: u32,
        end: u32,
        options: RequestOptions,
    ) -> Result<Vec<MmaEntry>> {
        let mut data = start.to_le_bytes().to_vec();
        data.extend_from_slice(&end.to_le_bytes());
        data.extend_from_slice(&[0, 0]);
        self.binary_request(
            dst,
            BinaryReqType::Mma,
            &data,
            options,
            |event, tag| match event {
                Event::MmaResponse(m) if m.tag == tag => Some(m.entries.clone()),
                _ => None,
            },
        )
        .await
    }

    /// A node's access control list (needs an admin login).
    pub async fn req_acl_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<Vec<AclEntry>> {
        self.binary_request(
            dst,
            BinaryReqType::Acl,
            &[0, 0],
            options,
            |event, tag| match event {
                Event::AclResponse(a) if a.tag == tag => Some(a.entries.clone()),
                _ => None,
            },
        )
        .await
    }

    fn neighbours_request(query: NeighboursQuery) -> Vec<u8> {
        let mut data = vec![0, query.count];
        data.extend_from_slice(&query.offset.to_le_bytes());
        data.push(query.order_by);
        data.push(query.pubkey_prefix_length);
        // a random blob keeps the packet hash unique
        data.extend_from_slice(&crate::util::random_u32().to_le_bytes());
        data
    }

    /// Sends a neighbours request without waiting; the answer arrives as
    /// [`Event::NeighboursResponse`].
    pub async fn req_neighbours_async(
        &self,
        dst: &PublicKey,
        query: NeighboursQuery,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        let data = Self::neighbours_request(query);
        self.send_binary_req(dst, BinaryReqType::Neighbours, &data, options)
            .await
    }

    /// A repeater's neighbours (one page).
    pub async fn req_neighbours_sync(
        &self,
        dst: &PublicKey,
        query: NeighboursQuery,
        options: RequestOptions,
    ) -> Result<NeighboursResponse> {
        let data = Self::neighbours_request(query);
        self.binary_request(
            dst,
            BinaryReqType::Neighbours,
            &data,
            options,
            |event, tag| match event {
                Event::NeighboursResponse(n) if n.tag == Some(tag) => Some((**n).clone()),
                _ => None,
            },
        )
        .await
    }

    /// All neighbours of a repeater, page by page. A failed page is tried
    /// once more; after that what was read is returned.
    pub async fn fetch_all_neighbours(
        &self,
        dst: &PublicKey,
        order_by: u8,
        pubkey_prefix_length: u8,
        options: RequestOptions,
    ) -> Result<NeighboursResponse> {
        let mut query = NeighboursQuery {
            count: 255,
            offset: 0,
            order_by,
            pubkey_prefix_length,
        };
        let mut all = self.req_neighbours_sync(dst, query, options).await?;
        all.tag = None;
        // later pages: requests close together and big answers, so wait longer
        let later = RequestOptions {
            timeout: options.timeout,
            min_timeout: options.min_timeout.saturating_add(Duration::from_secs(25)),
        };
        let mut retried = false;
        while all.results_count < all.neighbours_count {
            query.offset = u16::try_from(all.results_count).unwrap_or(u16::MAX);
            match self.req_neighbours_sync(dst, query, later).await {
                Ok(page) if page.results_count > 0 => {
                    retried = false;
                    // counts a node sends: no overflow on a bad one
                    all.results_count = all.results_count.saturating_add(page.results_count);
                    all.neighbours.extend(page.neighbours);
                }
                Err(err) if !retried => {
                    log::debug!(target: "meshcore", "neighbours page failed: {err}, retrying");
                    retried = true;
                }
                // an empty page or a second failure: return what was read
                Ok(_) | Err(_) => break,
            }
        }
        Ok(all)
    }

    /// Sends any binary request and waits for its raw answer.
    pub async fn req_binary_sync(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
    ) -> Result<BinaryResponse> {
        self.binary_request(dst, request_type, data, options, |event, tag| match event {
            Event::BinaryResponse(r) if r.tag == tag => Some((**r).clone()),
            _ => None,
        })
        .await
    }

    /// Sends a binary request and returns its raw answer (after the tag).
    async fn binary_answer(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
    ) -> Result<Vec<u8>> {
        Ok(self
            .req_binary_sync(dst, request_type, data, options)
            .await?
            .data)
    }

    /// A repeater's firmware version, name and owner (firmware level 2 and
    /// later; needs a login).
    pub async fn req_owner_info_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<OwnerInfo> {
        let data = self
            .binary_answer(dst, BinaryReqType::OwnerInfo, &[], options)
            .await?;
        let text = lossy_ignore(&data);
        let mut fields = text.trim_end_matches('\0').splitn(3, '\n');
        Ok(OwnerInfo {
            firmware_version: fields.next().unwrap_or_default().to_string(),
            name: fields.next().unwrap_or_default().to_string(),
            owner: fields.next().unwrap_or_default().to_string(),
        })
    }

    /// Subscribes to a sensor's telemetry pushes (needs a login). Pushes
    /// arrive as [`Event::BinaryResponse`] with `push_tag` as tag: a 4-byte
    /// timestamp, then LPP readings. `min_deltas` (3 to 14 bytes) are LPP
    /// encoded: channel, type and the least change that triggers a push.
    pub async fn req_subscribe_sync(
        &self,
        dst: &PublicKey,
        push_tag: [u8; 4],
        duration_secs: u16,
        min_deltas: &[u8],
        options: RequestOptions,
    ) -> Result<SensorSubscription> {
        if !(3..=14).contains(&min_deltas.len()) {
            return Err(Error::invalid("min_deltas must be 3 to 14 bytes"));
        }
        let mut data = push_tag.to_vec();
        data.extend_from_slice(&duration_secs.to_le_bytes());
        data.push(0);
        data.push(min_deltas.len() as u8);
        data.extend_from_slice(min_deltas);
        let answer = self
            .binary_answer(dst, BinaryReqType::Subscribe, &data, options)
            .await?;
        if answer.len() < 2 {
            // meshcore_py reads zeros for the missing bytes, without a line
            let err = Error::protocol("invalid_frame_length");
            log::error!(target: "meshcore", "Error parsing binary SUBSCRIBE response: {err}");
            return Err(err);
        }
        Ok(SensorSubscription {
            expiry_secs: u16::from_le_bytes([answer[0], answer[1]]),
            scope: lossy_ignore(answer.get(4..).unwrap_or_default())
                .trim_end_matches('\0')
                .to_string(),
        })
    }

    /// Ends a subscription to a sensor's telemetry pushes.
    pub async fn req_unsubscribe_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<()> {
        self.binary_answer(dst, BinaryReqType::Unsubscribe, &[], options)
            .await
            .map(drop)
    }

    /// Sends an anonymous request (`CMD_SEND_ANON_REQ`), which needs no login
    /// and, from firmware 1.16 on, no contact. Repeaters answer it when it
    /// comes direct; the answer arrives as [`Event::BinaryResponse`] and starts
    /// with the repeater's clock (4 bytes).
    ///
    /// The request carries the reply path: the out path of the device's own
    /// record of the node, which is read first, or zero-hop for a node that is
    /// no contact. A contact the device holds as flood is set to zero-hop for
    /// the request and reset to flood afterwards, also when the request is
    /// given up halfway (the reset then runs in the background) and when
    /// setting zero-hop got no answer, which the device may have taken anyway.
    /// The zero-hop path takes the device's hash mode, or mode 0 when the
    /// device does not tell it, as in meshcore_py: zero hops have no size a
    /// mode could give.
    pub async fn send_anon_req(
        &self,
        dst: &PublicKey,
        request_type: AnonReqType,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        let (sent, _) = self.send_anon_raw(dst, request_type, options).await?;
        Ok(sent)
    }

    async fn send_anon_raw(
        &self,
        dst: &PublicKey,
        request_type: AnonReqType,
        options: RequestOptions,
    ) -> Result<(SentInfo, Subscription)> {
        // The device sends along the path of its own record, and the answer
        // has to come back along it: the cache may lack the contact or hold
        // an older path.
        let contact = match self.get_contact_by_key(dst).await {
            Ok(contact) => Some(contact),
            Err(Error::Device(crate::ErrorCode::NotFound)) => None,
            Err(err) => {
                log::debug!(target: "meshcore", "could not read the contact: {err}");
                log::error!(
                    target: "meshcore",
                    "Could not read the contact from the device, aborting anon request"
                );
                return Err(err);
            }
        };

        // Set when the contact goes zero-hop for the request: it goes back to
        // flood afterwards, in the background if the request is given up.
        let mut flood_again = FloodAgain {
            client: None,
            key: *dst,
        };
        let reply_path = match &contact {
            None => {
                log::debug!(target: "meshcore", "no contact, asking for a zero-hop reply");
                encode_reply_path(PathLen::ZERO_HOP, &[])
            }
            Some(contact) if contact.is_flood() => {
                log::info!(target: "meshcore", "No path set trying zero hop");
                // Asked before the reset is armed: until the path is set,
                // nothing needs one.
                let hash_mode = self.get_path_hash_mode().await.unwrap_or_else(|err| {
                    log::debug!(target: "meshcore", "could not read the path hash mode: {err}, taking 0");
                    0
                });
                flood_again.client = Some(self.clone());
                if let Err(err) = self
                    .change_contact_path(contact, &[], Some(hash_mode))
                    .await
                {
                    // Without an answer that could be read the device may have
                    // taken the change.
                    if matches!(
                        err,
                        Error::Timeout | Error::ConnectionLost | Error::Protocol(_)
                    ) && let Err(reset_err) = self.reset_path(dst).await
                    {
                        log::warn!(
                            target: "meshcore",
                            "could not reset the path to flood: {reset_err}"
                        );
                    }
                    flood_again.client = None;
                    // Flooded, the server would drop the request: fail now.
                    log::debug!(target: "meshcore", "could not set a zero-hop path: {err}");
                    log::error!(
                        target: "meshcore",
                        "Could not set zero-hop path, aborting anon request"
                    );
                    return Err(err);
                }
                encode_reply_path(PathLen::ZERO_HOP, &[])
            }
            Some(contact) => encode_reply_path(contact.out_path_len, &contact.out_path),
        };
        let emitted_hops = u32::from(reply_path[0] & 63);

        let mut frame = vec![CommandType::SendAnonReq.code()];
        frame.extend_from_slice(dst.as_bytes());
        frame.push(request_type.code());
        frame.extend_from_slice(&reply_path);
        let result = self
            .send_tracked(
                &frame,
                PreparedRequest {
                    kind: RequestKind::Anon(request_type),
                    pubkey_prefix: dst.prefix(),
                    timeout: options.timeout,
                    min_timeout: options.min_timeout,
                    timeout_factor: emitted_hops + 1,
                    neighbours_prefix_len: 0,
                },
            )
            .await;
        if flood_again.client.is_some() {
            if let Err(err) = self.reset_path(dst).await {
                log::warn!(target: "meshcore", "could not reset the path to flood: {err}");
            }
            flood_again.client = None;
        }
        let (mut sent, events) = result?;
        // the reply comes back over every hop of the reply path
        sent.suggested_timeout = sent.suggested_timeout.saturating_mul(emitted_hops + 1);
        Ok((sent, events))
    }

    async fn anon_answer(
        &self,
        dst: &PublicKey,
        request_type: AnonReqType,
        options: RequestOptions,
    ) -> Result<BinaryResponse> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let (sent, events) = self.send_anon_raw(dst, request_type, options).await?;
        wait_response(
            events,
            options.wait(sent.suggested_timeout),
            reply!(Event::BinaryResponse(r) if r.tag == sent.expected_ack => (**r).clone()),
        )
        .await
    }

    /// Sends an anonymous regions request without waiting.
    pub async fn req_regions_async(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        self.send_anon_req(dst, AnonReqType::Regions, options).await
    }

    /// The regions whose floods a repeater forwards: their names without the
    /// leading `#`, separated by commas, `*` for floods without a scope.
    pub async fn req_regions_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<String> {
        let answer = self.anon_answer(dst, AnonReqType::Regions, options).await?;
        // the answer starts with the repeater's clock
        Ok(lossy_ignore(answer.data.get(4..).unwrap_or_default())
            .trim_matches('\0')
            .to_string())
    }

    /// Sends an anonymous owner request without waiting.
    pub async fn req_owner_async(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        self.send_anon_req(dst, AnonReqType::Owner, options).await
    }

    /// A repeater's name and owner info.
    pub async fn req_owner_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<AnonOwner> {
        let answer = self.anon_answer(dst, AnonReqType::Owner, options).await?;
        let text = lossy_ignore(answer.data.get(4..).unwrap_or_default());
        // After its clock the repeater sends "name\nowner", NUL padded.
        let (name, owner) = text
            .split_once('\n')
            .unwrap_or((text.trim_end_matches('\0'), ""));
        Ok(AnonOwner {
            name: name.to_string(),
            owner: owner.trim_matches('\0').to_string(),
        })
    }

    /// Sends an anonymous basic request without waiting.
    pub async fn req_basic_async(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        self.send_anon_req(dst, AnonReqType::Basic, options).await
    }

    /// A repeater's basic answer, raw: its clock (4 bytes), then a byte of
    /// flags (bit 0: it is a bridge, bit 1: over ESP-NOW, bit 7: forwarding is
    /// off).
    pub async fn req_basic_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<BinaryResponse> {
        self.anon_answer(dst, AnonReqType::Basic, options).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_waits() {
        assert_eq!(RequestOptions::default().wait(4000), Duration::from_secs(5));
        assert_eq!(
            RequestOptions::timeout(Duration::from_secs(2)).wait(4000),
            Duration::from_secs(2)
        );
        assert_eq!(
            RequestOptions::min_timeout(Duration::from_secs(9)).wait(4000),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn neighbours_request_layout() {
        let data = MeshCore::neighbours_request(NeighboursQuery {
            count: 20,
            offset: 300,
            order_by: 2,
            pubkey_prefix_length: 6,
        });
        assert_eq!(data[..6], [0, 20, 0x2c, 0x01, 2, 6]);
        assert_eq!(data.len(), 10);
    }
}
