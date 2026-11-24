//! Binary and anonymous requests to remote nodes.
//!
//! The answer to a request carries the tag of the request's `MSG_SENT`
//! reply. The reader binds the tag to the request as soon as `MSG_SENT`
//! arrives, so the answer is parsed by its type ([`Event::StatusResponse`],
//! [`Event::TelemetryResponse`], ...) besides the raw [`Event::BinaryResponse`].

use std::time::Duration;

use super::{MeshCore, reply, wait_response};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::lpp::Telemetry;
use crate::protocol::bytes::lossy_ignore;
use crate::protocol::codes::{AnonReqType, BinaryReqType, CommandType};
use crate::protocol::path::{PathLen, encode_reply_path};
use crate::state::{PreparedRequest, RequestKind};
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
        self.send_binary_req_with(dst, request_type, data, options, 0)
            .await
    }

    async fn send_binary_req_with(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
        neighbours_prefix_len: u8,
    ) -> Result<SentInfo> {
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
    async fn send_tracked(&self, frame: &[u8], prepared: PreparedRequest) -> Result<SentInfo> {
        let _guard = self.inner.cmd_lock.lock().await;
        self.state().prepared = Some(prepared);
        let result = self
            .request_locked(
                frame,
                self.default_timeout(),
                reply!(Event::MessageSent(s) => *s),
            )
            .await;
        if result.is_err() {
            self.state().prepared = None;
        }
        result
    }

    /// Sends a binary request and waits for its typed answer.
    async fn binary_request<T>(
        &self,
        dst: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
        options: RequestOptions,
        neighbours_prefix_len: u8,
        extract: impl Fn(&Event, u32) -> Option<T>,
    ) -> Result<T> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let mut sub = self.subscribe();
        let sent = self
            .send_binary_req_with(dst, request_type, data, options, neighbours_prefix_len)
            .await?;
        let tag = sent.expected_ack;
        wait_response(&mut sub, options.wait(sent.suggested_timeout), |event| {
            extract(event, tag)
        })
        .await
    }

    /// A node's status.
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
            0,
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
            0,
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
            0,
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
            0,
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
        self.send_binary_req_with(
            dst,
            BinaryReqType::Neighbours,
            &data,
            options,
            query.pubkey_prefix_length,
        )
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
            query.pubkey_prefix_length,
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
        self.binary_request(
            dst,
            request_type,
            data,
            options,
            0,
            |event, tag| match event {
                Event::BinaryResponse(r) if r.tag == tag => Some((**r).clone()),
                _ => None,
            },
        )
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
            return Err(Error::protocol("subscription answer too short"));
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
    /// and no contact. The answer arrives as [`Event::BinaryResponse`].
    ///
    /// The request carries the reply path: the contact's out path, or
    /// zero-hop. A contact the cache holds as flood is read from the device
    /// first; if the device holds it as flood too, it is set to zero-hop for
    /// the request and reset to flood afterwards.
    pub async fn send_anon_req(
        &self,
        dst: &PublicKey,
        request_type: AnonReqType,
        options: RequestOptions,
    ) -> Result<SentInfo> {
        let mut contact = self.contact(dst);
        if contact
            .as_ref()
            .is_some_and(crate::types::Contact::is_flood)
        {
            // The cache can be out of date: read the device's own record.
            match self.get_contact_by_key(dst).await {
                Ok(device_contact) => contact = Some(device_contact),
                Err(Error::Device(crate::ErrorCode::NotFound)) => contact = None,
                Err(err) => {
                    log::error!(target: "meshcore", "could not read the contact from the device: {err}");
                    return Err(err);
                }
            }
        }

        let mut zero_hop = false;
        let reply_path = match &contact {
            None => {
                log::debug!(target: "meshcore", "no contact, asking for a zero-hop reply");
                encode_reply_path(PathLen::ZERO_HOP, &[])
            }
            Some(contact) if contact.is_flood() => {
                log::info!(target: "meshcore", "no path set, trying zero hop");
                if let Err(err) = self.change_contact_path(contact, &[], None).await {
                    // Flooded, the server would drop the request: fail now.
                    log::error!(target: "meshcore", "could not set a zero-hop path: {err}");
                    return Err(err);
                }
                zero_hop = true;
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
        if zero_hop && let Err(err) = self.reset_path(dst).await {
            log::warn!(target: "meshcore", "could not reset the path to flood: {err}");
        }
        let mut sent = result?;
        // the reply comes back over every hop of the reply path
        sent.suggested_timeout = sent.suggested_timeout.saturating_mul(emitted_hops + 1);
        Ok(sent)
    }

    async fn anon_answer(
        &self,
        dst: &PublicKey,
        request_type: AnonReqType,
        options: RequestOptions,
    ) -> Result<BinaryResponse> {
        let _mesh = self.inner.mesh_lock.lock().await;
        let mut sub = self.subscribe();
        let sent = self.send_anon_req(dst, request_type, options).await?;
        wait_response(
            &mut sub,
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

    /// The regions a node serves.
    pub async fn req_regions_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<String> {
        let answer = self.anon_answer(dst, AnonReqType::Regions, options).await?;
        // the answer repeats a 4-byte tag first
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

    /// A node's name and owner info.
    pub async fn req_owner_sync(
        &self,
        dst: &PublicKey,
        options: RequestOptions,
    ) -> Result<AnonOwner> {
        let answer = self.anon_answer(dst, AnonReqType::Owner, options).await?;
        let text = lossy_ignore(answer.data.get(4..).unwrap_or_default());
        let (name, owner) = text.split_once('\n').unwrap_or((text.as_str(), ""));
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

    /// A node's basic answer (its clock), raw.
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
