//! Command handlers for `MeshCore` operations.
//!
//! This module provides high-level command functions that handle
//! the request/response protocol with the device.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::event::{Event, EventDispatcher, EventFilter};
use crate::protocol::{BinaryReqType, CommandOpcode, ControlDataType, PacketType, StatsType};
use crate::transport::Transport;
use crate::types::PublicKey;

/// Coordinate scaling factor (multiply by 1e6 for storage).
const COORD_SCALE: f64 = 1_000_000.0;

/// Parameters for updating a contact.
#[derive(Debug, Clone)]
pub struct ContactUpdateParams<'a> {
    /// Contact's public key.
    pub public_key: &'a PublicKey,
    /// Contact type (0 = room, 1 = base, etc.).
    pub contact_type: u8,
    /// Contact flags.
    pub flags: u8,
    /// Path length (-1 = no path, 0+ = number of hops).
    pub path_len: i8,
    /// Path data (repeater public key prefixes).
    pub path: &'a [u8],
    /// Contact name.
    pub name: &'a str,
    /// Last advertisement timestamp.
    pub last_advert: u32,
    /// Latitude in decimal degrees.
    pub latitude: Option<f64>,
    /// Longitude in decimal degrees.
    pub longitude: Option<f64>,
}

/// Default command timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Command handler for `MeshCore` operations.
pub struct CommandHandler<T> {
    transport: Arc<Mutex<T>>,
    dispatcher: EventDispatcher,
    timeout: Duration,
    binary_tag: AtomicU32,
}

impl<T: Transport> CommandHandler<T> {
    /// Creates a new command handler.
    #[must_use]
    pub fn new(transport: Arc<Mutex<T>>, dispatcher: EventDispatcher) -> Self {
        Self {
            transport,
            dispatcher,
            timeout: DEFAULT_TIMEOUT,
            binary_tag: AtomicU32::new(1),
        }
    }

    /// Sets the command timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Gets the next binary request tag.
    fn next_tag(&self) -> u32 {
        self.binary_tag.fetch_add(1, Ordering::SeqCst)
    }

    /// Sends a raw command and waits for specific response types.
    async fn send_and_wait(&self, data: Bytes, expected: &[PacketType]) -> Result<Event> {
        // IMPORTANT: Subscribe BEFORE sending to avoid race conditions.
        // With broadcast channels, events are only delivered to subscribers
        // that exist at the time of dispatch. If we send first and then
        // subscribe, a fast response could be dispatched before our
        // subscription is created, causing us to miss it.
        let filter = EventFilter::packet_types(expected.to_vec());
        let mut subscription = self.dispatcher.subscribe(None);

        // Send the command
        {
            let mut transport = self.transport.lock().await;
            transport.send(data).await?;
        }

        // Wait for matching response with timeout
        let timeout = self.timeout;
        tokio::select! {
            biased;
            result = async {
                loop {
                    if let Some(event) = subscription.recv().await {
                        if filter.matches(&event) {
                            return Some(event);
                        }
                    } else {
                        return None;
                    }
                }
            } => result.ok_or_else(|| Error::Timeout {
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            }),
            () = tokio::time::sleep(timeout) => Err(Error::Timeout {
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            }),
        }
    }

    /// Sends a command and expects OK/Error response.
    async fn send_expect_ok(&self, data: Bytes) -> Result<()> {
        let event = self
            .send_and_wait(data, &[PacketType::Ok, PacketType::Error])
            .await?;
        match event {
            Event::Ok => Ok(()),
            Event::Error { message } => Err(Error::Protocol { message }),
            _ => Err(Error::Protocol {
                message: "unexpected response".into(),
            }),
        }
    }

    /// Sends a command without waiting for response.
    ///
    /// Use this for "set" commands where the device processes the command
    /// but response timing is unreliable.
    async fn send_fire_and_forget(&self, data: Bytes) -> Result<()> {
        {
            let mut transport = self.transport.lock().await;
            transport.send(data).await?;
        }
        // Small delay to let device process
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(())
    }

    // ==================== Device Commands ====================

    /// Initializes the connection and retrieves device info.
    ///
    /// Sends the "mccli" marker to identify as a `MeshCore` CLI client.
    /// Returns `SelfInfo` with device configuration.
    pub async fn app_start(&self) -> Result<Event> {
        // Format: 0x01 0x03 <6 spaces> "mccli"
        // The 0x03 byte and spaces are part of the protocol handshake
        let data = Bytes::from_static(&[
            CommandOpcode::AppStart as u8,
            0x03,
            b' ',
            b' ',
            b' ',
            b' ',
            b' ',
            b' ',
            b'm',
            b'c',
            b'c',
            b'l',
            b'i',
        ]);
        self.send_and_wait(data, &[PacketType::SelfInfo, PacketType::Error])
            .await
    }

    /// Gets the current device time.
    pub async fn get_time(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::GetTime as u8]);
        self.send_and_wait(data, &[PacketType::CurrentTime, PacketType::Error])
            .await
    }

    /// Sets the device time.
    ///
    /// Note: Fire-and-forget command. Use `get_time` to verify.
    pub async fn set_time(&self, timestamp: u32) -> Result<()> {
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(CommandOpcode::SetTime as u8);
        buf.put_u32_le(timestamp);
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Gets the battery status.
    pub async fn get_battery(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::GetBattery as u8]);
        self.send_and_wait(data, &[PacketType::Battery, PacketType::Error])
            .await
    }

    /// Queries device information.
    pub async fn device_query(&self) -> Result<Event> {
        // Sub-type 0x03 requests full device info
        let data = Bytes::from_static(&[CommandOpcode::DeviceQuery as u8, 0x03]);
        self.send_and_wait(data, &[PacketType::DeviceInfo, PacketType::Error])
            .await
    }

    /// Sends an advertisement.
    ///
    /// If `flood` is true, sends a flood advertisement.
    pub async fn send_advert(&self, flood: bool) -> Result<()> {
        let data = if flood {
            Bytes::from_static(&[CommandOpcode::SendAdvert as u8, 0x01])
        } else {
            Bytes::from_static(&[CommandOpcode::SendAdvert as u8])
        };
        self.send_expect_ok(data).await
    }

    /// Sets the device name.
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    pub async fn set_name(&self, name: &str) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1 + name.len());
        buf.put_u8(CommandOpcode::SetName as u8);
        buf.put_slice(name.as_bytes());
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets the device coordinates.
    ///
    /// Coordinates are in decimal degrees.
    /// Valid range: latitude -90 to 90, longitude -180 to 180.
    ///
    /// # Protocol Limitation
    ///
    /// The protocol uses 0 as a sentinel value meaning "no coordinate set".
    /// Setting coordinates to exactly (0.0, 0.0) will be interpreted by
    /// the device as "no location" when read back.
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    ///
    /// # Errors
    ///
    /// Returns an error if coordinates are out of valid range.
    pub async fn set_coords(&self, latitude: f64, longitude: f64) -> Result<()> {
        if !(-90.0..=90.0).contains(&latitude) {
            return Err(Error::Protocol {
                message: format!("latitude {latitude} out of range (-90 to 90)"),
            });
        }
        if !(-180.0..=180.0).contains(&longitude) {
            return Err(Error::Protocol {
                message: format!("longitude {longitude} out of range (-180 to 180)"),
            });
        }
        // GPS coordinates in microdegrees fit comfortably in i32:
        // latitude: -90 to 90 → -90_000_000 to 90_000_000
        // longitude: -180 to 180 → -180_000_000 to 180_000_000
        let lat_encoded = (latitude * COORD_SCALE).round() as i32;
        let lon_encoded = (longitude * COORD_SCALE).round() as i32;

        let mut buf = BytesMut::with_capacity(13);
        buf.put_u8(CommandOpcode::SetCoords as u8);
        buf.put_i32_le(lat_encoded);
        buf.put_i32_le(lon_encoded);
        buf.put_i32_le(0); // Reserved/altitude field
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets the TX power in dBm.
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    pub async fn set_tx_power(&self, power: i32) -> Result<()> {
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(CommandOpcode::SetTxPower as u8);
        buf.put_i32_le(power);
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets radio parameters.
    ///
    /// # Arguments
    ///
    /// * `freq_mhz` - Frequency in MHz (e.g., 868.0, typical range 433-928)
    /// * `bw_khz` - Bandwidth in kHz (e.g., 125.0, typical range 7.8-500)
    /// * `sf` - Spreading factor (6-12)
    /// * `cr` - Coding rate (5-8)
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    pub async fn set_radio(&self, freq_mhz: f64, bw_khz: f64, sf: u8, cr: u8) -> Result<()> {
        // LoRa frequencies in kHz fit in u32 (433 MHz = 433_000 kHz)
        // Bandwidth in Hz also fits (500 kHz = 500_000 Hz)
        // Convert via i64 to avoid sign_loss warning, then clamp to u32 range
        let freq_encoded = u32::try_from(((freq_mhz * 1000.0).round() as i64).max(0)).unwrap_or(0);
        let bw_encoded = u32::try_from(((bw_khz * 1000.0).round() as i64).max(0)).unwrap_or(0);

        let mut buf = BytesMut::with_capacity(11);
        buf.put_u8(CommandOpcode::SetRadio as u8);
        buf.put_u32_le(freq_encoded);
        buf.put_u32_le(bw_encoded);
        buf.put_u8(sf);
        buf.put_u8(cr);
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets tuning parameters.
    ///
    /// # Arguments
    ///
    /// * `rx_delay` - Receive delay
    /// * `af` - Antenna factor
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    pub async fn set_tuning(&self, rx_delay: i32, af: i32) -> Result<()> {
        let mut buf = BytesMut::with_capacity(11);
        buf.put_u8(CommandOpcode::SetTuning as u8);
        buf.put_i32_le(rx_delay);
        buf.put_i32_le(af);
        buf.put_u8(0); // Reserved
        buf.put_u8(0); // Reserved
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets the device PIN (for BLE pairing).
    ///
    /// Note: Fire-and-forget command.
    pub async fn set_device_pin(&self, pin: u32) -> Result<()> {
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(CommandOpcode::SetDevicePin as u8);
        buf.put_u32_le(pin);
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Sets other device parameters.
    ///
    /// # Arguments
    ///
    /// * `manual_add_contacts` - Require manual approval for new contacts
    /// * `telemetry_mode` - Telemetry mode byte (env:2 | loc:2 | base:2 bits)
    /// * `advert_loc_policy` - Advertisement location policy
    /// * `multi_acks` - Multi-ACK setting
    ///
    /// Note: Fire-and-forget command. Use `device_query` to verify.
    pub async fn set_other_params(
        &self,
        manual_add_contacts: bool,
        telemetry_mode: u8,
        advert_loc_policy: u8,
        multi_acks: u8,
    ) -> Result<()> {
        let mut buf = BytesMut::with_capacity(5);
        buf.put_u8(CommandOpcode::SetOtherParams as u8);
        buf.put_u8(u8::from(manual_add_contacts));
        buf.put_u8(telemetry_mode);
        buf.put_u8(advert_loc_policy);
        buf.put_u8(multi_acks);
        self.send_fire_and_forget(buf.freeze()).await
    }

    /// Reboots the device.
    pub async fn reboot(&self) -> Result<()> {
        // The "reboot" string is required as a safety measure
        let data = Bytes::from_static(&[
            CommandOpcode::Reboot as u8,
            b'r',
            b'e',
            b'b',
            b'o',
            b'o',
            b't',
        ]);
        self.send_expect_ok(data).await
    }

    /// Exports the device's private key.
    pub async fn export_private_key(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::ExportPrivateKey as u8]);
        self.send_and_wait(
            data,
            &[
                PacketType::PrivateKey,
                PacketType::Error,
                PacketType::Disabled,
            ],
        )
        .await
    }

    /// Imports a private key.
    pub async fn import_private_key(&self, key: &[u8; 32]) -> Result<()> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::ImportPrivateKey as u8);
        buf.put_slice(key);
        self.send_expect_ok(buf.freeze()).await
    }

    /// Gets device statistics.
    pub async fn get_stats(&self, stats_type: StatsType) -> Result<Event> {
        let data = Bytes::from(vec![CommandOpcode::GetStats as u8, stats_type as u8]);
        self.send_and_wait(data, &[PacketType::Stats, PacketType::Error])
            .await
    }

    /// Gets custom variables.
    pub async fn get_custom_vars(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::GetCustomVars as u8]);
        self.send_and_wait(data, &[PacketType::CustomVars, PacketType::Error])
            .await
    }

    /// Sets a custom variable.
    ///
    /// Note: Fire-and-forget command. Use `get_custom_vars` to verify.
    pub async fn set_custom_var(&self, key: &str, value: &str) -> Result<()> {
        let kv = format!("{key}:{value}");
        let mut buf = BytesMut::with_capacity(1 + kv.len());
        buf.put_u8(CommandOpcode::SetCustomVar as u8);
        buf.put_slice(kv.as_bytes());
        self.send_fire_and_forget(buf.freeze()).await
    }

    // ==================== Contact Commands ====================

    /// Gets the contact list.
    ///
    /// Optionally pass a `last_modified` timestamp to only get updated contacts.
    ///
    /// This triggers a sequence of events: `ContactListStart`, `Contact*`, `ContactListEnd`.
    /// The caller should subscribe to events to receive individual contacts.
    /// This method returns after the `ContactListEnd` event is received.
    pub async fn get_contacts(&self, last_modified: Option<u32>) -> Result<()> {
        let data = if let Some(ts) = last_modified {
            let mut buf = BytesMut::with_capacity(5);
            buf.put_u8(CommandOpcode::GetContacts as u8);
            buf.put_u32_le(ts);
            buf.freeze()
        } else {
            Bytes::from_static(&[CommandOpcode::GetContacts as u8])
        };

        // Send the command - contacts arrive as a sequence of events
        // ending with ContactEnd
        let event = self
            .send_and_wait(data, &[PacketType::ContactEnd, PacketType::Error])
            .await?;

        match event {
            Event::ContactListEnd { .. } => Ok(()),
            Event::Error { message } => Err(Error::Protocol { message }),
            _ => Err(Error::Protocol {
                message: "unexpected response to GetContacts".into(),
            }),
        }
    }

    /// Updates a contact.
    pub async fn update_contact(&self, params: &ContactUpdateParams<'_>) -> Result<()> {
        let mut buf = BytesMut::with_capacity(146);
        buf.put_u8(CommandOpcode::UpdateContact as u8);
        buf.put_slice(params.public_key.as_bytes());
        buf.put_u8(params.contact_type);
        buf.put_u8(params.flags);
        buf.put_i8(params.path_len);

        // Path: 64 bytes, zero-padded
        let path_len_actual = params.path.len().min(64);
        buf.put_slice(&params.path[..path_len_actual]);
        buf.put_bytes(0, 64 - path_len_actual);

        // Name: 32 bytes, zero-padded
        let name_bytes = params.name.as_bytes();
        let name_len = name_bytes.len().min(32);
        buf.put_slice(&name_bytes[..name_len]);
        buf.put_bytes(0, 32 - name_len);

        // Additional fields (GPS in microdegrees, same range validation as set_coords)
        buf.put_u32_le(params.last_advert);
        buf.put_i32_le(
            params
                .latitude
                .map_or(0, |v| (v * COORD_SCALE).round() as i32),
        );
        buf.put_i32_le(
            params
                .longitude
                .map_or(0, |v| (v * COORD_SCALE).round() as i32),
        );

        self.send_expect_ok(buf.freeze()).await
    }

    /// Removes a contact.
    pub async fn remove_contact(&self, public_key: &PublicKey) -> Result<()> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::RemoveContact as u8);
        buf.put_slice(public_key.as_bytes());
        self.send_expect_ok(buf.freeze()).await
    }

    /// Resets the path for a contact.
    pub async fn reset_path(&self, public_key: &PublicKey) -> Result<()> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::ResetPath as u8);
        buf.put_slice(public_key.as_bytes());
        self.send_expect_ok(buf.freeze()).await
    }

    /// Shares a contact (generates URI).
    pub async fn share_contact(&self, public_key: &PublicKey) -> Result<()> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::ShareContact as u8);
        buf.put_slice(public_key.as_bytes());
        self.send_expect_ok(buf.freeze()).await
    }

    /// Exports a contact (or self if no key provided).
    ///
    /// Returns a `ContactUri` event with the contact card URI.
    pub async fn export_contact(&self, public_key: Option<&PublicKey>) -> Result<Event> {
        let data = if let Some(key) = public_key {
            let mut buf = BytesMut::with_capacity(33);
            buf.put_u8(CommandOpcode::ExportContact as u8);
            buf.put_slice(key.as_bytes());
            buf.freeze()
        } else {
            Bytes::from_static(&[CommandOpcode::ExportContact as u8])
        };
        self.send_and_wait(data, &[PacketType::ContactUri, PacketType::Error])
            .await
    }

    /// Imports a contact from card data.
    pub async fn import_contact(&self, card_data: &[u8]) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1 + card_data.len());
        buf.put_u8(CommandOpcode::ImportContact as u8);
        buf.put_slice(card_data);
        self.send_expect_ok(buf.freeze()).await
    }

    // ==================== Messaging Commands ====================

    /// Sends a private message to a contact.
    ///
    /// Returns an event with the expected ACK code and timeout.
    pub async fn send_message(
        &self,
        destination: &PublicKey,
        message: &str,
        attempt: u8,
        timestamp: u32,
    ) -> Result<Event> {
        let prefix = destination.prefix();
        let mut buf = BytesMut::with_capacity(14 + message.len());
        buf.put_u8(CommandOpcode::SendMessage as u8);
        buf.put_u8(0x00); // Message type: private
        buf.put_u8(attempt);
        buf.put_u32_le(timestamp);
        buf.put_slice(&prefix);
        buf.put_slice(message.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    /// Sends a command to a contact.
    pub async fn send_command(
        &self,
        destination: &PublicKey,
        command: &str,
        timestamp: u32,
    ) -> Result<Event> {
        let prefix = destination.prefix();
        let mut buf = BytesMut::with_capacity(14 + command.len());
        buf.put_u8(CommandOpcode::SendMessage as u8);
        buf.put_u8(0x01); // Message type: command
        buf.put_u8(0x00); // Attempt counter (always 0 for commands)
        buf.put_u32_le(timestamp);
        buf.put_slice(&prefix);
        buf.put_slice(command.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    /// Sends a channel message.
    pub async fn send_channel_message(
        &self,
        channel_index: u8,
        message: &str,
        timestamp: u32,
    ) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(8 + message.len());
        buf.put_u8(CommandOpcode::SendChannelMsg as u8);
        buf.put_u8(0x00); // Reserved byte
        buf.put_u8(channel_index);
        buf.put_u32_le(timestamp);
        buf.put_slice(message.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::Ok, PacketType::Error])
            .await
    }

    /// Gets the next waiting message.
    pub async fn get_message(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::GetMessage as u8]);
        self.send_and_wait(
            data,
            &[
                PacketType::ContactMsgRecv,
                PacketType::ContactMsgRecvV3,
                PacketType::ChannelMsgRecv,
                PacketType::ChannelMsgRecvV3,
                PacketType::NoMoreMsgs,
                PacketType::Error,
            ],
        )
        .await
    }

    /// Sends a login request to a contact (for room servers).
    ///
    /// Returns `MsgSent` immediately. `LoginSuccess` or `LoginFailed` will arrive
    /// as push notifications when the room server responds.
    pub async fn send_login(&self, destination: &PublicKey, password: &str) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(33 + password.len());
        buf.put_u8(CommandOpcode::SendLogin as u8);
        buf.put_slice(destination.as_bytes());
        buf.put_slice(password.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    /// Sends a logout request.
    pub async fn send_logout(&self, destination: &PublicKey) -> Result<()> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::SendLogout as u8);
        buf.put_slice(destination.as_bytes());
        self.send_expect_ok(buf.freeze()).await
    }

    /// Sends a status request to a contact.
    ///
    /// Returns `MsgSent` immediately. The actual `StatusResponse` will arrive
    /// as a push notification when the contact responds.
    pub async fn send_status_request(&self, destination: &PublicKey) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(33);
        buf.put_u8(CommandOpcode::SendStatusReq as u8);
        buf.put_slice(destination.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    // ==================== Channel Commands ====================

    /// Gets channel information.
    pub async fn get_channel(&self, index: u8) -> Result<Event> {
        let data = Bytes::from(vec![CommandOpcode::GetChannel as u8, index]);
        self.send_and_wait(data, &[PacketType::ChannelInfo, PacketType::Error])
            .await
    }

    /// Sets channel configuration.
    ///
    /// Note: Fire-and-forget command. Use `get_channel` to verify.
    pub async fn set_channel(&self, index: u8, name: &str, secret: &[u8; 16]) -> Result<()> {
        let mut buf = BytesMut::with_capacity(50);
        buf.put_u8(CommandOpcode::SetChannel as u8);
        buf.put_u8(index);

        // Name: 32 bytes, null-padded
        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(32);
        buf.put_slice(&name_bytes[..name_len]);
        buf.put_bytes(0, 32 - name_len);

        buf.put_slice(secret);

        self.send_fire_and_forget(buf.freeze()).await
    }

    // ==================== Binary Request Commands ====================

    /// Sends a binary status request.
    pub async fn binary_status_request(&self, destination: &PublicKey) -> Result<Event> {
        self.binary_request(destination, BinaryReqType::Status, &[])
            .await
    }

    /// Sends a binary keep-alive request.
    pub async fn binary_keep_alive(&self, destination: &PublicKey) -> Result<Event> {
        self.binary_request(destination, BinaryReqType::KeepAlive, &[])
            .await
    }

    /// Sends a binary telemetry request.
    pub async fn binary_telemetry_request(&self, destination: &PublicKey) -> Result<Event> {
        self.binary_request(destination, BinaryReqType::Telemetry, &[])
            .await
    }

    /// Sends a min/max/avg (MMA) data request.
    pub async fn binary_mma_request(&self, destination: &PublicKey) -> Result<Event> {
        self.binary_request(destination, BinaryReqType::Mma, &[])
            .await
    }

    /// Sends an access control list (ACL) request.
    pub async fn binary_acl_request(&self, destination: &PublicKey) -> Result<Event> {
        self.binary_request(destination, BinaryReqType::Acl, &[])
            .await
    }

    /// Sends a neighbours list request.
    ///
    /// # Arguments
    ///
    /// * `destination` - Target device public key
    /// * `max_results` - Maximum number of neighbours to return
    /// * `offset` - Pagination offset
    /// * `order_by` - Sort field
    /// * `prefix_len` - Public key prefix length (4, 6, 8, or 32)
    pub async fn binary_neighbours_request(
        &self,
        destination: &PublicKey,
        max_results: u8,
        offset: u16,
        order_by: u8,
        prefix_len: u8,
    ) -> Result<Event> {
        let seed = self.next_tag();
        let mut data = BytesMut::with_capacity(10);
        data.put_u8(0); // Version
        data.put_u8(max_results);
        data.put_u16_le(offset);
        data.put_u8(order_by);
        data.put_u8(prefix_len);
        data.put_u32_le(seed);

        self.binary_request(destination, BinaryReqType::Neighbours, &data)
            .await
    }

    /// Sends a generic binary request.
    ///
    /// Returns `MsgSent` immediately with an `expected_ack` tag.
    /// The actual `BinaryResponse` will arrive as a push notification
    /// when the contact responds. Use `wait_for_ack` to wait for it.
    pub async fn binary_request(
        &self,
        destination: &PublicKey,
        request_type: BinaryReqType,
        data: &[u8],
    ) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(34 + data.len());
        buf.put_u8(CommandOpcode::BinaryReq as u8);
        buf.put_slice(destination.as_bytes());
        buf.put_u8(request_type as u8);
        buf.put_slice(data);

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    // ==================== Telemetry Commands ====================

    /// Requests self telemetry.
    pub async fn get_self_telemetry(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::Telemetry as u8, 0x00, 0x00, 0x00]);
        self.send_and_wait(data, &[PacketType::TelemetryResponse, PacketType::Error])
            .await
    }

    /// Requests telemetry from a contact.
    pub async fn send_telemetry_request(&self, destination: &PublicKey) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(36);
        buf.put_u8(CommandOpcode::Telemetry as u8);
        buf.put_u8(0x00); // Reserved
        buf.put_u8(0x00); // Reserved
        buf.put_u8(0x00); // Reserved
        buf.put_slice(destination.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    // ==================== Path Discovery Commands ====================

    /// Sends a path discovery request.
    ///
    /// Returns `MsgSent` immediately. The actual `PathDiscoveryResponse` will arrive
    /// as a push notification when the contact responds.
    pub async fn path_discovery(&self, destination: &PublicKey) -> Result<Event> {
        let mut buf = BytesMut::with_capacity(34);
        buf.put_u8(CommandOpcode::PathDiscovery as u8);
        buf.put_u8(0x00); // Reserved
        buf.put_slice(destination.as_bytes());

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    /// Sends a trace path request to test routing through specific repeaters.
    ///
    /// # Arguments
    ///
    /// * `auth_code` - 32-bit authentication code
    /// * `tag` - Optional 32-bit tag to identify this trace (random if None)
    /// * `flags` - Flags byte
    /// * `path` - Repeater path (sequence of 6-byte pubkey prefixes, or comma-separated hex values)
    pub async fn send_trace(
        &self,
        auth_code: u32,
        tag: Option<u32>,
        flags: u8,
        path: &[u8],
    ) -> Result<Event> {
        let tag = tag.unwrap_or_else(|| self.next_tag());

        let mut buf = BytesMut::with_capacity(10 + path.len());
        buf.put_u8(CommandOpcode::SendTrace as u8);
        buf.put_u32_le(tag);
        buf.put_u32_le(auth_code);
        buf.put_u8(flags);
        buf.put_slice(path);

        self.send_and_wait(buf.freeze(), &[PacketType::MsgSent, PacketType::Error])
            .await
    }

    /// Sets the flood scope.
    ///
    /// Pass a 16-byte key to limit flood to a specific scope.
    /// Pass all zeros to disable scope limiting.
    pub async fn set_flood_scope(&self, scope_key: &[u8; 16]) -> Result<()> {
        let mut buf = BytesMut::with_capacity(18);
        buf.put_u8(CommandOpcode::SetFloodScope as u8);
        buf.put_u8(0x00); // Reserved byte
        buf.put_slice(scope_key);
        self.send_expect_ok(buf.freeze()).await
    }

    /// Sets the flood scope using a topic hash.
    ///
    /// The topic string is hashed with SHA256 and the first 16 bytes are used.
    #[cfg(feature = "sha2")]
    pub async fn set_flood_scope_topic(&self, topic: &str) -> Result<()> {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(topic.as_bytes());
        let hash = hasher.finalize();
        let mut key = [0u8; 16];
        key.copy_from_slice(&hash[..16]);
        self.set_flood_scope(&key).await
    }

    /// Clears the flood scope (allows all floods).
    pub async fn clear_flood_scope(&self) -> Result<()> {
        self.set_flood_scope(&[0u8; 16]).await
    }

    // ==================== Control Data Commands ====================

    /// Sends a node discovery request.
    ///
    /// # Arguments
    ///
    /// * `filter` - Device type filter
    /// * `prefix_only` - Return only pubkey prefixes (default true)
    /// * `tag` - Optional request tag for matching responses
    /// * `since` - Optional timestamp to get nodes updated since
    pub async fn node_discover(
        &self,
        filter: u8,
        prefix_only: bool,
        tag: Option<u32>,
        since: Option<u32>,
    ) -> Result<()> {
        // Build payload: [filter:1] [tag:4LE] [since:4LE if provided]
        let mut payload = BytesMut::with_capacity(9);
        payload.put_u8(filter);
        payload.put_u32_le(tag.unwrap_or_else(|| self.next_tag()));
        if let Some(ts) = since {
            payload.put_u32_le(ts);
        }

        // Control type with prefix_only flag
        let control_type = ControlDataType::NodeDiscoverReq as u8 | u8::from(prefix_only);

        let mut buf = BytesMut::with_capacity(2 + payload.len());
        buf.put_u8(CommandOpcode::SendControlData as u8);
        buf.put_u8(control_type);
        buf.put_slice(&payload);

        self.send_expect_ok(buf.freeze()).await
    }

    // ==================== Signature Commands ====================

    /// Starts a signature operation.
    pub async fn sign_start(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::SignStart as u8]);
        self.send_and_wait(data, &[PacketType::SignStart, PacketType::Error])
            .await
    }

    /// Sends data chunk for signing.
    pub async fn sign_data(&self, chunk: &[u8]) -> Result<()> {
        let mut buf = BytesMut::with_capacity(1 + chunk.len());
        buf.put_u8(CommandOpcode::SignData as u8);
        buf.put_slice(chunk);
        self.send_expect_ok(buf.freeze()).await
    }

    /// Finishes signing and gets the signature.
    pub async fn sign_finish(&self) -> Result<Event> {
        let data = Bytes::from_static(&[CommandOpcode::SignFinish as u8]);
        self.send_and_wait(data, &[PacketType::Signature, PacketType::Error])
            .await
    }

    // ==================== Utility Methods ====================

    /// Waits for a specific ACK code.
    pub async fn wait_for_ack(&self, code: u32, timeout: Duration) -> Result<Event> {
        let filter = EventFilter::ack(code);
        self.dispatcher
            .wait_for(filter, timeout)
            .await
            .ok_or_else(|| Error::Timeout {
                timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
            })
    }
}
