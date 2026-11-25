//! Commands about the companion device itself.

use std::time::Duration;

use super::{MeshCore, reply};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::protocol::codes::{CommandType, StatsType};
use crate::types::{
    BatteryStatus, ChannelInfo, CoreStats, CustomVars, DeviceInfo, FreqRange, OtherParams,
    PacketStats, PrivateKey, PublicKey, RadioStats, SelfInfo, TelemetryResponse, TuningParams,
    channel_secret_from_name,
};

/// The `APP_START` frame as meshcore_py sends it: the command, seven reserved
/// bytes (the firmware ignores them; meshcore_py sends 3 and six spaces) and
/// the app name.
const APP_START: &[u8] = b"\x01\x03      mccli";

impl MeshCore {
    /// Starts the session (`CMD_APP_START`) and returns the device's own
    /// information. Also stops a contact list the device is still sending.
    pub async fn send_appstart(&self) -> Result<SelfInfo> {
        self.send_appstart_timeout(self.default_timeout()).await
    }

    pub(crate) async fn send_appstart_timeout(&self, timeout: Duration) -> Result<SelfInfo> {
        self.request(
            APP_START,
            timeout,
            reply!(Event::SelfInfo(info) => (**info).clone()),
        )
        .await
    }

    /// Queries the firmware (`CMD_DEVICE_QUERY`, protocol version 3). From then
    /// on until it restarts, the device sends messages as v3 frames, with SNR.
    pub async fn send_device_query(&self) -> Result<DeviceInfo> {
        let frame = [CommandType::DeviceQuery.code(), 0x03];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::DeviceInfo(info) => (**info).clone()),
        )
        .await
    }

    /// Runs a CLI command (at least 2 bytes) on the device (firmware code 14
    /// and later) and returns its reply.
    pub async fn run_cli_command(&self, command: &str) -> Result<String> {
        if command.len() < 2 || command.contains('\0') {
            return Err(Error::invalid(format!(
                "CLI command {command:?}, the firmware takes 2 bytes and more, no NUL"
            )));
        }
        let mut frame = vec![CommandType::RunCliCommand.code()];
        frame.extend_from_slice(command.as_bytes());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::CliReply(text) => text.clone()),
        )
        .await
    }

    /// Sends a raw mesh packet with priority 0.
    pub async fn send_raw_packet(&self, packet: &[u8]) -> Result<()> {
        self.send_raw_packet_with_priority(packet, 0).await
    }

    /// Sends a raw mesh packet (header, path, payload; 2 bytes or more) with a
    /// priority.
    pub async fn send_raw_packet_with_priority(&self, packet: &[u8], priority: u8) -> Result<()> {
        if packet.len() < 2 {
            return Err(Error::invalid("a raw packet has 2 bytes or more"));
        }
        let mut frame = vec![CommandType::SendRawPacket.code(), priority];
        frame.extend_from_slice(packet);
        self.request_ok(&frame).await.map(drop)
    }

    /// Sends an advert, zero-hop or flooded.
    pub async fn send_advert(&self, flood: bool) -> Result<()> {
        let frame: &[u8] = if flood {
            &[CommandType::SendSelfAdvert.code(), 0x01]
        } else {
            &[CommandType::SendSelfAdvert.code()]
        };
        self.request_ok(frame).await.map(drop)
    }

    /// Sets the node's name. The firmware keeps the first 31 bytes, so the
    /// name is cut there, at a character boundary.
    pub async fn set_name(&self, name: &str) -> Result<()> {
        let name = crate::util::cut_at_char_boundary(name, 31);
        if name.is_empty() {
            return Err(Error::invalid("the node name is empty"));
        }
        if name.contains('\0') {
            return Err(Error::invalid("the node name holds a NUL byte"));
        }
        let mut frame = vec![CommandType::SetAdvertName.code()];
        frame.extend_from_slice(name.as_bytes());
        self.request_ok(&frame).await?;
        if let Some(info) = &mut self.state().self_info {
            info.name = name.to_string();
        }
        Ok(())
    }

    /// Sets the node's location in degrees. They go out in millionths, rounded
    /// with halves to even as meshcore_py rounds them. A latitude beyond -90
    /// to 90 or a longitude beyond -180 to 180 after rounding, which the
    /// firmware would refuse, is refused before sending.
    pub async fn set_coords(&self, latitude: f64, longitude: f64) -> Result<()> {
        let lat = (latitude * 1e6).round_ties_even();
        let lon = (longitude * 1e6).round_ties_even();
        if !(-90e6..=90e6).contains(&lat) || !(-180e6..=180e6).contains(&lon) {
            return Err(Error::invalid(format!(
                "coordinates {latitude}, {longitude} out of range"
            )));
        }
        let (lat, lon) = (lat as i32, lon as i32);
        let mut frame = vec![CommandType::SetAdvertLatLon.code()];
        frame.extend_from_slice(&lat.to_le_bytes());
        frame.extend_from_slice(&lon.to_le_bytes());
        frame.extend_from_slice(&0i32.to_le_bytes());
        self.request_ok(&frame).await?;
        if let Some(info) = &mut self.state().self_info {
            info.latitude_e6 = lat;
            info.longitude_e6 = lon;
        }
        Ok(())
    }

    /// Reboots the device. The device does not answer, so this only sends.
    pub async fn reboot(&self) -> Result<()> {
        self.send_only(b"\x13reboot").await
    }

    /// Battery voltage and storage use.
    pub async fn get_bat(&self) -> Result<BatteryStatus> {
        let frame = [CommandType::GetBattAndStorage.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::Battery(b) => *b),
        )
        .await
    }

    /// The device's clock (Unix seconds).
    pub async fn get_time(&self) -> Result<u32> {
        let frame = [CommandType::GetDeviceTime.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::CurrentTime(t) => *t),
        )
        .await
    }

    /// Sets the device's clock. The firmware refuses a time before its own
    /// with [`ErrorCode::IllegalArg`](crate::ErrorCode::IllegalArg).
    pub async fn set_time(&self, time: u32) -> Result<()> {
        let mut frame = vec![CommandType::SetDeviceTime.code()];
        frame.extend_from_slice(&time.to_le_bytes());
        self.request_ok(&frame).await.map(drop)
    }

    /// Sets the TX power in dBm (the firmware takes -9 up to its maximum).
    pub async fn set_tx_power(&self, dbm: i8) -> Result<()> {
        let mut frame = vec![CommandType::SetRadioTxPower.code()];
        frame.extend_from_slice(&i32::from(dbm).to_le_bytes());
        self.request_ok(&frame).await?;
        if let Some(info) = &mut self.state().self_info {
            info.tx_power = dbm;
        }
        Ok(())
    }

    /// Sets the radio: frequency in MHz, bandwidth in kHz, spreading factor,
    /// coding rate (5-8) and, on firmware code 9 and later, whether to repeat.
    /// The frequency goes out in whole kHz and the bandwidth in whole Hz,
    /// rounded (meshcore_py cuts off the rest).
    pub async fn set_radio(
        &self,
        freq_mhz: f64,
        bw_khz: f64,
        sf: u8,
        cr: u8,
        repeat: Option<bool>,
    ) -> Result<()> {
        #[allow(clippy::cast_sign_loss)] // checked to be in range first
        let to_u32 = |value: f64, what: &str| -> Result<u32> {
            let scaled = (value * 1000.0).round();
            if scaled.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&scaled) {
                Ok(scaled as u32)
            } else {
                Err(Error::invalid(format!("{what} {value} out of range")))
            }
        };
        let freq = to_u32(freq_mhz, "frequency")?;
        let bw = to_u32(bw_khz, "bandwidth")?;
        let mut frame = vec![CommandType::SetRadioParams.code()];
        frame.extend_from_slice(&freq.to_le_bytes());
        frame.extend_from_slice(&bw.to_le_bytes());
        frame.extend_from_slice(&[sf, cr]);
        if let Some(repeat) = repeat {
            frame.push(u8::from(repeat));
        }
        self.request_ok(&frame).await?;
        // The firmware switches the radio at once.
        if let Some(info) = &mut self.state().self_info {
            info.radio_freq = f64::from(freq) / 1000.0;
            info.radio_bw = f64::from(bw) / 1000.0;
            info.radio_sf = sf;
            info.radio_cr = cr;
        }
        Ok(())
    }

    /// Sets the RX delay base and the airtime factor, both times 1000.
    pub async fn set_tuning(&self, rx_delay: u32, airtime_factor: u32) -> Result<()> {
        let mut frame = vec![CommandType::SetTuningParams.code()];
        frame.extend_from_slice(&rx_delay.to_le_bytes());
        frame.extend_from_slice(&airtime_factor.to_le_bytes());
        frame.extend_from_slice(&[0, 0]);
        self.request_ok(&frame).await.map(drop)
    }

    /// The RX delay base and airtime factor, both times 1000.
    pub async fn get_tuning(&self) -> Result<TuningParams> {
        let frame = [CommandType::GetTuningParams.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::TuningParams(t) => *t),
        )
        .await
    }

    /// Writes all settings of `CMD_SET_OTHER_PARAMS` (meshcore_py's
    /// `set_other_params_from_infos`).
    pub async fn set_other_params(&self, params: &OtherParams) -> Result<()> {
        let frame = [
            CommandType::SetOtherParams.code(),
            u8::from(params.manual_add_contacts),
            params.telemetry_mode_byte(),
            params.adv_loc_policy,
            params.multi_acks,
        ];
        self.request_ok(&frame).await?;
        if let Some(info) = &mut self.state().self_info {
            info.apply_other_params(*params);
        }
        Ok(())
    }

    /// Changes one of the `CMD_SET_OTHER_PARAMS` settings and keeps the others.
    ///
    /// The others come from the self info held since connecting, not from a
    /// new `APP_START`: that would stop a contact list the device is sending.
    /// `APP_START` is sent only when no self info is held.
    async fn set_other_param(&self, change: impl FnOnce(&mut OtherParams)) -> Result<()> {
        let held = self.state().self_info.as_ref().map(SelfInfo::other_params);
        let mut params = match held {
            Some(params) => params,
            None => self.send_appstart().await?.other_params(),
        };
        change(&mut params);
        self.set_other_params(&params).await
    }

    /// Sets the telemetry mode for base data (0 deny, 1 by contact flags, 2 allow all).
    pub async fn set_telemetry_mode_base(&self, mode: u8) -> Result<()> {
        self.set_other_param(|p| p.telemetry_mode_base = mode & 0b11)
            .await
    }

    /// Sets the telemetry mode for the location.
    pub async fn set_telemetry_mode_loc(&self, mode: u8) -> Result<()> {
        self.set_other_param(|p| p.telemetry_mode_loc = mode & 0b11)
            .await
    }

    /// Sets the telemetry mode for environment sensors.
    pub async fn set_telemetry_mode_env(&self, mode: u8) -> Result<()> {
        self.set_other_param(|p| p.telemetry_mode_env = mode & 0b11)
            .await
    }

    /// Sets whether new contacts are added only manually.
    pub async fn set_manual_add_contacts(&self, manual: bool) -> Result<()> {
        self.set_other_param(|p| p.manual_add_contacts = manual)
            .await
    }

    /// Sets whether adverts carry the location.
    pub async fn set_advert_loc_policy(&self, policy: u8) -> Result<()> {
        self.set_other_param(|p| p.adv_loc_policy = policy).await
    }

    /// Sets the number of extra ACK transmissions.
    pub async fn set_multi_acks(&self, multi_acks: u8) -> Result<()> {
        self.set_other_param(|p| p.multi_acks = multi_acks).await
    }

    /// Sets the BLE PIN: 0 or six digits.
    pub async fn set_devicepin(&self, pin: u32) -> Result<()> {
        let mut frame = vec![CommandType::SetDevicePin.code()];
        frame.extend_from_slice(&pin.to_le_bytes());
        self.request_ok(&frame).await.map(drop)
    }

    /// The device's own telemetry.
    pub async fn get_self_telemetry(&self) -> Result<TelemetryResponse> {
        let frame = [CommandType::SendTelemetryReq.code(), 0, 0, 0];
        // The answer carries the device's own key prefix; an answer to an
        // earlier send_telemetry_req carries the other node's.
        let own = self.self_info().map(|info| info.public_key.prefix());
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::TelemetryResponse(t)
                if t.tag.is_none() && own.is_none_or(|own| t.pubkey_prefix == own)
                => (**t).clone()),
        )
        .await
    }

    /// The custom variables, in the device's order.
    pub async fn get_custom_vars(&self) -> Result<CustomVars> {
        let frame = [CommandType::GetCustomVars.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::CustomVars(v) => v.clone()),
        )
        .await
    }

    /// Sets a custom variable. The firmware takes `name:value` from 3 bytes
    /// on, so name and value need 2 bytes together, and splits it at the
    /// first `:`, so the name has none.
    pub async fn set_custom_var(&self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() {
            return Err(Error::invalid("the custom variable has no name"));
        }
        if name.contains(':') {
            return Err(Error::invalid(format!(
                "custom variable name {name:?} with a ':', the firmware would split it there"
            )));
        }
        if name.contains('\0') || value.contains('\0') {
            return Err(Error::invalid(
                "a custom variable holds a NUL byte, the firmware would end it there",
            ));
        }
        if name.len() + value.len() < 2 {
            return Err(Error::invalid(format!(
                "custom variable {name:?} = {value:?}, name and value need 2 bytes together"
            )));
        }
        let mut frame = vec![CommandType::SetCustomVar.code()];
        frame.extend_from_slice(name.as_bytes());
        frame.push(b':');
        frame.extend_from_slice(value.as_bytes());
        self.request_ok(&frame).await.map(drop)
    }

    /// Reads a channel. Its secret is kept to name and decrypt RX log
    /// packets (see [`set_keep_channel_secrets`](Self::set_keep_channel_secrets)).
    pub async fn get_channel(&self, index: u8) -> Result<ChannelInfo> {
        let frame = [CommandType::GetChannel.code(), index];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::ChannelInfo(c) if c.channel_idx == index => (**c).clone()),
        )
        .await
    }

    /// Sets a channel. A name starting with `#`, or no secret, takes the
    /// secret from the name's SHA-256 (as meshcore_py). The firmware keeps
    /// the first 31 bytes of the name. As with [`get_channel`](Self::get_channel),
    /// the secret is kept for the RX log.
    pub async fn set_channel(&self, index: u8, name: &str, secret: Option<[u8; 16]>) -> Result<()> {
        // The device would show the name up to the NUL, the key comes from all of it.
        if name.contains('\0') {
            return Err(Error::invalid("the channel name holds a NUL byte"));
        }
        let secret = match secret {
            Some(secret) if !name.starts_with('#') => secret,
            _ => channel_secret_from_name(name),
        };
        let kept = crate::util::cut_at_char_boundary(name, 31);
        let mut name_field = [0u8; 32];
        name_field[..kept.len()].copy_from_slice(kept.as_bytes());
        let mut frame = vec![CommandType::SetChannel.code(), index];
        frame.extend_from_slice(&name_field);
        frame.extend_from_slice(&secret);
        self.request_ok(&frame).await?;
        self.state().parser.add_channel(&ChannelInfo {
            channel_idx: index,
            channel_name: kept.to_string(),
            channel_secret: secret,
        });
        Ok(())
    }

    /// Exports the private key; [`Error::Disabled`] when the firmware does not
    /// allow it.
    pub async fn export_private_key(&self) -> Result<PrivateKey> {
        let frame = [CommandType::ExportPrivateKey.code()];
        let key = self
            .request(&frame, self.default_timeout(), |event| match event {
                Event::PrivateKey(key) => Some(Some((**key).clone())),
                Event::Disabled => Some(None),
                _ => None,
            })
            .await?;
        key.ok_or(Error::Disabled)
    }

    /// Imports a private key (the 64 bytes
    /// [`export_private_key`](Self::export_private_key) gives);
    /// [`Error::Disabled`] when the firmware does not allow it.
    pub async fn import_private_key(&self, key: &[u8; 64]) -> Result<()> {
        let mut frame = vec![CommandType::ImportPrivateKey.code()];
        frame.extend_from_slice(key);
        let done = self
            .request(&frame, self.default_timeout(), |event| match event {
                Event::Ok { .. } => Some(true),
                Event::Disabled => Some(false),
                _ => None,
            })
            .await?;
        if done { Ok(()) } else { Err(Error::Disabled) }
    }

    /// Starts a signing session; returns the most bytes that can be signed.
    pub async fn sign_start(&self) -> Result<u32> {
        let frame = [CommandType::SignStart.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::SignStart { max_length } => *max_length),
        )
        .await
    }

    /// Adds data (not empty) to the signing session. A missing reply after
    /// 5 s is taken as received (as meshcore_py).
    pub async fn sign_data(&self, chunk: &[u8]) -> Result<()> {
        if chunk.is_empty() {
            return Err(Error::invalid("the chunk to sign is empty"));
        }
        let mut frame = vec![CommandType::SignData.code()];
        frame.extend_from_slice(chunk);
        match self
            .request(
                &frame,
                Duration::from_secs(5),
                reply!(Event::Ok { .. } => ()),
            )
            .await
        {
            Err(Error::Timeout) => {
                log::warn!(
                    target: "meshcore",
                    "sign_data OK response not received (timeout), but continuing - data may \
                     have been processed by device"
                );
                Ok(())
            }
            other => other,
        }
    }

    /// Ends the signing session and returns the signature. Without a timeout
    /// it waits `max(3 x default, 15 s)` plus up to 5 s for large data.
    pub async fn sign_finish(
        &self,
        timeout: Option<Duration>,
        data_size: usize,
    ) -> Result<Vec<u8>> {
        let timeout = timeout.unwrap_or_else(|| {
            let base = self
                .default_timeout()
                .saturating_mul(3)
                .max(Duration::from_secs(15));
            let bonus =
                Duration::from_millis(((data_size as u64).saturating_mul(1000) / 2048).min(5000));
            base.saturating_add(bonus)
        });
        let frame = [CommandType::SignFinish.code()];
        self.request(&frame, timeout, reply!(Event::Signature(s) => s.clone()))
            .await
    }

    /// Signs `data` on the device in chunks of `chunk_size` bytes (meshcore_py
    /// uses 120); a chunk and its command byte have to fit one frame.
    pub async fn sign(
        &self,
        data: &[u8],
        chunk_size: usize,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>> {
        let largest = crate::protocol::frame::DEVICE_MAX_FRAME_SIZE - 1;
        if chunk_size == 0 || chunk_size > largest {
            return Err(Error::invalid(format!(
                "chunk size {chunk_size}, it has to be 1 to {largest} bytes"
            )));
        }
        let max_length = self.sign_start().await?;
        if max_length > 0 && data.len() > max_length as usize {
            return Err(Error::invalid(format!(
                "{} bytes to sign, the device takes at most {max_length}",
                data.len()
            )));
        }
        let total = data.len().div_ceil(chunk_size);
        for (index, chunk) in data.chunks(chunk_size).enumerate() {
            if let Err(err) = self.sign_data(chunk).await {
                log::error!(
                    target: "meshcore",
                    "Error sending chunk {}/{total}: {}",
                    index + 1,
                    super::python_payload(&err)
                );
                return Err(err);
            }
        }
        self.sign_finish(timeout, data.len()).await
    }

    /// Whether the device holds a connection (keep-alive after a login) to a
    /// server.
    pub async fn has_connection(&self, server: &PublicKey) -> Result<bool> {
        let mut frame = vec![CommandType::HasConnection.code()];
        frame.extend_from_slice(server.as_bytes());
        self.request(&frame, self.default_timeout(), |event| match event {
            Event::Ok { .. } => Some(true),
            Event::Error(info) if info.code == Some(crate::ErrorCode::NotFound) => Some(false),
            _ => None,
        })
        .await
    }

    /// Step 1 of a factory reset: returns a token for
    /// [`confirm_factory_reset`](Self::confirm_factory_reset).
    #[must_use]
    pub fn request_factory_reset(&self) -> String {
        let token = format!(
            "{:08x}{:08x}",
            crate::util::random_u32(),
            crate::util::random_u32()
        );
        log::warn!(
            target: "meshcore",
            "Factory reset requested. Call confirm_factory_reset('{token}') to proceed. This \
             will ERASE ALL DATA on the device."
        );
        *super::lock(&self.inner.factory_reset_token) = Some(token.clone());
        token
    }

    /// Step 2 of a factory reset: erases the device and reboots it.
    ///
    /// The firmware may drop the link before it answers, so a missing reply
    /// counts as done.
    pub async fn confirm_factory_reset(&self, token: &str) -> Result<()> {
        {
            let mut stored = super::lock(&self.inner.factory_reset_token);
            if stored.as_deref() != Some(token) {
                return Err(Error::invalid(
                    "invalid or expired factory reset token, call request_factory_reset first",
                ));
            }
            *stored = None;
        }
        // meshcore_py's text, dash included.
        log::warn!(target: "meshcore", "Executing factory reset — all device data will be erased");
        match self
            .request(
                b"\x33reset",
                self.default_timeout(),
                reply!(Event::Ok { .. } => ()),
            )
            .await
        {
            Err(Error::Timeout | Error::ConnectionLost) => Ok(()),
            other => other,
        }
    }

    /// Core statistics.
    pub async fn get_stats_core(&self) -> Result<CoreStats> {
        let frame = [CommandType::GetStats.code(), StatsType::Core.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::StatsCore(s) => *s),
        )
        .await
    }

    /// Radio statistics.
    pub async fn get_stats_radio(&self) -> Result<RadioStats> {
        let frame = [CommandType::GetStats.code(), StatsType::Radio.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::StatsRadio(s) => *s),
        )
        .await
    }

    /// Packet statistics.
    pub async fn get_stats_packets(&self) -> Result<PacketStats> {
        let frame = [CommandType::GetStats.code(), StatsType::Packets.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::StatsPackets(s) => *s),
        )
        .await
    }

    /// The frequency ranges the device may repeat on.
    pub async fn get_allowed_repeat_freq(&self) -> Result<Vec<FreqRange>> {
        let frame = [CommandType::GetAllowedRepeatFreq.code()];
        self.request(
            &frame,
            self.default_timeout(),
            reply!(Event::AllowedRepeatFreq(f) => f.clone()),
        )
        .await
    }

    /// Sets the path hash mode: bytes per hop minus one (0-2).
    pub async fn set_path_hash_mode(&self, mode: u8) -> Result<()> {
        let frame = [CommandType::SetPathHashMode.code(), 0, mode];
        self.request_ok(&frame).await.map(drop)
    }

    /// The path hash mode (0 when the firmware does not report one).
    pub async fn get_path_hash_mode(&self) -> Result<u8> {
        Ok(self.send_device_query().await?.path_hash_mode.unwrap_or(0))
    }
}
