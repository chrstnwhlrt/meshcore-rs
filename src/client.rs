//! Main [`MeshCore`] client implementation.
//!
//! This module provides the high-level [`MeshCore`] client that combines
//! transport, event handling, and commands into a unified interface.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::task::JoinHandle;

use crate::commands::CommandHandler;
use crate::error::{Error, Result};
use crate::event::{Event, EventDispatcher, StatsData, Subscription};
use crate::protocol::{
    PacketType, StatsType, parse_battery, parse_channel, parse_channel_message, parse_contact,
    parse_contact_message, parse_core_stats, parse_device_info, parse_device_status,
    parse_packet_stats, parse_radio_stats, parse_self_info,
};
use crate::transport::{SerialTransport, Transport, serial::SerialConfig};
use crate::types::{
    Acknowledgment, BatteryStatus, Channel, Contact, CoreStats, DeviceInfo, PacketStats, PublicKey,
    RadioStats, SelfInfo, Telemetry,
};

/// Gets the current Unix timestamp as a u32.
fn current_timestamp() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// Client for communicating with a `MeshCore` device.
pub struct MeshCore<T> {
    transport: Arc<Mutex<T>>,
    dispatcher: EventDispatcher,
    commands: CommandHandler<T>,

    // Internal state
    self_info: Arc<RwLock<Option<SelfInfo>>>,
    contacts: Arc<RwLock<HashMap<PublicKey, Contact>>>,

    // Background tasks
    read_task: Option<JoinHandle<()>>,
    process_task: Option<JoinHandle<()>>,
}

impl MeshCore<SerialTransport> {
    /// Creates a new client for a serial port.
    ///
    /// # Arguments
    ///
    /// * `port` - Serial port path (e.g., "/dev/ttyUSB0")
    ///
    /// # Returns
    ///
    /// A new client (not yet connected).
    #[must_use]
    pub fn serial(port: impl Into<String>) -> Self {
        let config = SerialConfig::new(port);
        Self::with_serial_config(config)
    }

    /// Creates a new client with custom serial configuration.
    #[must_use]
    pub fn with_serial_config(config: SerialConfig) -> Self {
        let transport = SerialTransport::new(config);
        Self::new(transport)
    }
}

impl<T: Transport + 'static> MeshCore<T> {
    /// Creates a new client with the given transport.
    fn new(transport: T) -> Self {
        let (dispatcher, _event_rx) = EventDispatcher::new(256);
        let transport = Arc::new(Mutex::new(transport));

        let commands = CommandHandler::new(Arc::clone(&transport), dispatcher.clone());

        Self {
            transport,
            dispatcher,
            commands,
            self_info: Arc::new(RwLock::new(None)),
            contacts: Arc::new(RwLock::new(HashMap::new())),
            read_task: None,
            process_task: None,
        }
    }

    /// Connects to the device and initializes the session.
    ///
    /// This will:
    /// 1. Open the transport connection
    /// 2. Start background read task
    /// 3. Send `AppStart` command
    /// 4. Store device info
    ///
    /// # Errors
    ///
    /// Returns an error if connection or initialization fails.
    pub async fn connect(&mut self) -> Result<SelfInfo> {
        // Connect transport
        {
            let mut transport = self.transport.lock().await;
            transport.connect().await?;
        }

        // Start read loop
        self.start_read_loop().await?;

        // Allow time for any stale data to be received and discarded
        // This is needed because USB serial buffers may hold old data
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Initialize with AppStart
        let event = self.commands.app_start().await?;

        let info = if let Event::SelfInfo(info) = event {
            let cloned = (*info).clone();
            let mut self_info = self.self_info.write().await;
            *self_info = Some(*info);
            cloned
        } else if let Event::Error { message } = event {
            return Err(Error::Protocol { message });
        } else {
            return Err(Error::Protocol {
                message: "unexpected response to AppStart".into(),
            });
        };

        // Dispatch connected event
        self.dispatcher.dispatch(Event::Connected);

        Ok(info)
    }

    /// Starts the background read loop.
    async fn start_read_loop(&mut self) -> Result<()> {
        let (frame_tx, mut frame_rx) = mpsc::channel::<Bytes>(256);

        // Take reader and decoder from transport (only works for SerialTransport)
        let reader_and_decoder = {
            let mut transport = self.transport.lock().await;
            if let Some(serial) =
                ((&mut *transport) as &mut dyn std::any::Any).downcast_mut::<SerialTransport>()
            {
                let reader = serial.take_reader();
                let decoder = std::mem::take(serial.decoder_mut());
                reader.map(|r| (r, decoder))
            } else {
                None
            }
        };

        // Spawn read task with the reader (doesn't hold transport lock)
        if let Some((reader, decoder)) = reader_and_decoder {
            let read_task = tokio::spawn(async move {
                if let Err(e) =
                    SerialTransport::run_read_loop_with_reader(reader, decoder, frame_tx).await
                {
                    tracing::error!("read loop error: {}", e);
                }
            });
            self.read_task = Some(read_task);
        }

        // Spawn frame processing task
        let dispatcher = self.dispatcher.clone();
        let self_info = Arc::clone(&self.self_info);
        let contacts = Arc::clone(&self.contacts);

        let process_task = tokio::spawn(async move {
            while let Some(frame) = frame_rx.recv().await {
                process_frame(&frame, &dispatcher, &self_info, &contacts).await;
            }
        });
        self.process_task = Some(process_task);

        Ok(())
    }

    /// Disconnects from the device.
    pub async fn disconnect(&mut self) -> Result<()> {
        // Stop background tasks
        if let Some(task) = self.read_task.take() {
            task.abort();
        }
        if let Some(task) = self.process_task.take() {
            task.abort();
        }

        // Disconnect transport
        {
            let mut transport = self.transport.lock().await;
            transport.disconnect().await?;
        }

        // Dispatch disconnected event
        self.dispatcher.dispatch(Event::Disconnected);

        Ok(())
    }

    /// Returns true if connected.
    pub async fn is_connected(&self) -> bool {
        let transport = self.transport.lock().await;
        transport.is_connected()
    }

    /// Returns the device info if available.
    pub async fn self_info(&self) -> Option<SelfInfo> {
        self.self_info.read().await.clone()
    }

    /// Returns all known contacts.
    pub async fn contacts(&self) -> HashMap<PublicKey, Contact> {
        self.contacts.read().await.clone()
    }

    /// Returns the command handler for direct command access.
    #[must_use]
    pub const fn commands(&self) -> &CommandHandler<T> {
        &self.commands
    }

    /// Subscribes to events.
    #[must_use]
    pub fn subscribe(&self) -> Subscription {
        self.dispatcher.subscribe(None)
    }

    // ==================== High-Level Device Methods ====================

    /// Gets the battery status.
    pub async fn get_battery(&self) -> Result<BatteryStatus> {
        let event = self.commands.get_battery().await?;
        if let Event::Battery(status) = event {
            Ok(status)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Gets device information.
    pub async fn get_device_info(&self) -> Result<DeviceInfo> {
        let event = self.commands.device_query().await?;
        if let Event::DeviceInfo(info) = event {
            Ok(*info)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Gets the current device time.
    pub async fn get_time(&self) -> Result<u32> {
        let event = self.commands.get_time().await?;
        if let Event::CurrentTime(time) = event {
            Ok(time)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Sets the device time to the current system time.
    pub async fn sync_time(&self) -> Result<()> {
        self.commands.set_time(current_timestamp()).await
    }

    /// Gets core statistics.
    pub async fn get_core_stats(&self) -> Result<CoreStats> {
        let event = self.commands.get_stats(StatsType::Core).await?;
        if let Event::Stats(stats) = event {
            if let StatsData::Core(core) = stats {
                return Ok(core);
            }
        } else if let Event::Error { message } = event {
            return Err(Error::Protocol { message });
        }
        Err(Error::Protocol {
            message: "unexpected response".into(),
        })
    }

    /// Gets radio statistics.
    pub async fn get_radio_stats(&self) -> Result<RadioStats> {
        let event = self.commands.get_stats(StatsType::Radio).await?;
        if let Event::Stats(stats) = event {
            if let StatsData::Radio(radio) = stats {
                return Ok(radio);
            }
        } else if let Event::Error { message } = event {
            return Err(Error::Protocol { message });
        }
        Err(Error::Protocol {
            message: "unexpected response".into(),
        })
    }

    /// Gets packet statistics.
    pub async fn get_packet_stats(&self) -> Result<PacketStats> {
        let event = self.commands.get_stats(StatsType::Packets).await?;
        if let Event::Stats(stats) = event {
            if let StatsData::Packets(packets) = stats {
                return Ok(packets);
            }
        } else if let Event::Error { message } = event {
            return Err(Error::Protocol { message });
        }
        Err(Error::Protocol {
            message: "unexpected response".into(),
        })
    }

    // ==================== High-Level Contact Methods ====================

    /// Gets the contact list from the device.
    ///
    /// Returns a copy of the contacts after fetching.
    pub async fn get_contacts(&self) -> Result<HashMap<PublicKey, Contact>> {
        self.commands.get_contacts(None).await?;

        // Wait for contact list to be received
        tokio::time::sleep(Duration::from_millis(500)).await;

        Ok(self.contacts.read().await.clone())
    }

    /// Gets a specific contact by public key.
    pub async fn get_contact(&self, public_key: &PublicKey) -> Option<Contact> {
        self.contacts.read().await.get(public_key).cloned()
    }

    // ==================== High-Level Messaging Methods ====================

    /// Sends a private message.
    ///
    /// Returns when the message has been acknowledged or times out.
    pub async fn send_message(&self, destination: &PublicKey, message: &str) -> Result<()> {
        let event = self
            .commands
            .send_message(destination, message, 0, current_timestamp())
            .await?;

        if let Event::MessageSent {
            expected_ack,
            timeout_ms,
        } = event
        {
            // Wait for ACK
            let timeout = Duration::from_millis(u64::from(timeout_ms));
            self.commands.wait_for_ack(expected_ack, timeout).await?;
            Ok(())
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Sends a channel message.
    pub async fn send_channel_message(&self, channel: u8, message: &str) -> Result<()> {
        let event = self
            .commands
            .send_channel_message(channel, message, current_timestamp())
            .await?;

        match event {
            Event::Ok => Ok(()),
            Event::Error { message } => Err(Error::Protocol { message }),
            _ => Err(Error::Protocol {
                message: "unexpected response".into(),
            }),
        }
    }

    /// Fetches all waiting messages.
    pub async fn fetch_messages(&self) -> Result<Vec<Event>> {
        let mut messages = Vec::new();

        loop {
            let event = self.commands.get_message().await?;
            match &event {
                Event::Error { message } => {
                    return Err(Error::Protocol {
                        message: message.clone(),
                    });
                }
                Event::ContactMessage(_) | Event::ChannelMessage(_) => {
                    messages.push(event);
                }
                _ => break,
            }
        }

        Ok(messages)
    }

    // ==================== High-Level Channel Methods ====================

    /// Gets channel information.
    pub async fn get_channel(&self, index: u8) -> Result<Channel> {
        let event = self.commands.get_channel(index).await?;
        if let Event::ChannelInfo(channel) = event {
            Ok(*channel)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    // ==================== High-Level Status Methods ====================

    /// Sends a status request to a remote device.
    ///
    /// Returns `MsgSent` with the expected ACK code. The actual `StatusResponse`
    /// will arrive as a push notification - subscribe to events to receive it.
    pub async fn request_remote_status(&self, destination: &PublicKey) -> Result<u32> {
        let event = self.commands.send_status_request(destination).await?;
        if let Event::MessageSent { expected_ack, .. } = event {
            Ok(expected_ack)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Sends a telemetry request to a remote device.
    ///
    /// Returns `MsgSent` with the expected ACK code. The actual `TelemetryResponse`
    /// will arrive as a push notification - subscribe to events to receive it.
    pub async fn request_remote_telemetry(&self, destination: &PublicKey) -> Result<u32> {
        let event = self.commands.send_telemetry_request(destination).await?;
        if let Event::MessageSent { expected_ack, .. } = event {
            Ok(expected_ack)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }

    /// Gets self telemetry.
    pub async fn get_self_telemetry(&self) -> Result<Telemetry> {
        let event = self.commands.get_self_telemetry().await?;
        if let Event::TelemetryResponse(telemetry) = event {
            Ok(*telemetry)
        } else if let Event::Error { message } = event {
            Err(Error::Protocol { message })
        } else {
            Err(Error::Protocol {
                message: "unexpected response".into(),
            })
        }
    }
}

/// Processes a received frame and dispatches the appropriate event.
#[allow(clippy::too_many_lines)]
async fn process_frame(
    frame: &[u8],
    dispatcher: &EventDispatcher,
    self_info: &Arc<RwLock<Option<SelfInfo>>>,
    contacts: &Arc<RwLock<HashMap<PublicKey, Contact>>>,
) {
    if frame.is_empty() {
        return;
    }

    let packet_type = frame[0];
    let data = &frame[1..];

    tracing::trace!(
        "processing packet type 0x{packet_type:02x}, {} bytes",
        data.len()
    );

    let event = match PacketType::from_byte(packet_type) {
        Some(PacketType::Ok) => Event::Ok,
        Some(PacketType::Error) => {
            let message = String::from_utf8_lossy(data).into_owned();
            Event::Error { message }
        }
        Some(PacketType::SelfInfo) => {
            match parse_self_info(data) {
                Ok(info) => {
                    // Update cached self_info
                    let mut cached = self_info.write().await;
                    *cached = Some(info.clone());
                    Event::SelfInfo(Box::new(info))
                }
                Err(e) => {
                    tracing::warn!("failed to parse SelfInfo: {}", e);
                    Event::Raw {
                        packet_type,
                        data: data.to_vec(),
                    }
                }
            }
        }
        Some(PacketType::DeviceInfo) => match parse_device_info(data) {
            Ok(info) => Event::DeviceInfo(Box::new(info)),
            Err(e) => {
                tracing::warn!("failed to parse DeviceInfo: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::Battery) => match parse_battery(data) {
            Ok(battery) => Event::Battery(battery),
            Err(e) => {
                tracing::warn!("failed to parse Battery: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::Contact) => match parse_contact(data) {
            Ok(contact) => {
                // Update contacts cache
                let mut cached = contacts.write().await;
                let key = contact.public_key.clone();
                cached.insert(key, contact.clone());
                Event::Contact(Box::new(contact))
            }
            Err(e) => {
                tracing::warn!("failed to parse Contact: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::PushNewAdvert) => match parse_contact(data) {
            Ok(contact) => {
                // Update contacts cache
                let mut cached = contacts.write().await;
                let key = contact.public_key.clone();
                cached.insert(key, contact.clone());
                Event::NewContactAdvert(Box::new(contact))
            }
            Err(e) => {
                tracing::warn!("failed to parse NewContactAdvert: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::Advertisement) => {
            // Simple advertisement - just a 32-byte public key
            if data.len() >= 32 {
                let mut key_bytes = [0u8; 32];
                key_bytes.copy_from_slice(&data[..32]);
                Event::Advertisement(PublicKey::from_bytes(&key_bytes))
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::ContactStart) => {
            // ContactStart contains the expected contact count
            let count = if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]])
            } else {
                0
            };
            Event::ContactListStart { count }
        }
        Some(PacketType::ContactEnd) => {
            // ContactEnd contains the last modification timestamp
            let last_modified = if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]])
            } else {
                0
            };
            Event::ContactListEnd { last_modified }
        }
        Some(PacketType::ContactMsgRecv) => match parse_contact_message(data, false) {
            Ok(msg) => Event::ContactMessage(Box::new(msg)),
            Err(e) => {
                tracing::warn!("failed to parse ContactMessage: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::ContactMsgRecvV3) => match parse_contact_message(data, true) {
            Ok(msg) => Event::ContactMessage(Box::new(msg)),
            Err(e) => {
                tracing::warn!("failed to parse ContactMessage v3: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::ChannelMsgRecv) => match parse_channel_message(data, false) {
            Ok(msg) => Event::ChannelMessage(Box::new(msg)),
            Err(e) => {
                tracing::warn!("failed to parse ChannelMessage: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::ChannelMsgRecvV3) => match parse_channel_message(data, true) {
            Ok(msg) => Event::ChannelMessage(Box::new(msg)),
            Err(e) => {
                tracing::warn!("failed to parse ChannelMessage v3: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::ChannelInfo) => match parse_channel(data) {
            Ok(channel) => Event::ChannelInfo(Box::new(channel)),
            Err(e) => {
                tracing::warn!("failed to parse Channel: {}", e);
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        },
        Some(PacketType::MsgSent) => {
            if data.len() >= 9 {
                // First byte is message type (currently unused)
                let expected_ack = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                let timeout_ms = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);
                Event::MessageSent {
                    expected_ack,
                    timeout_ms,
                }
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::Ack) => {
            if data.len() >= 4 {
                let code = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                Event::Ack(Acknowledgment { code })
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::NoMoreMsgs) => Event::NoMoreMessages,
        Some(PacketType::MessagesWaiting) => Event::MessagesWaiting,
        Some(PacketType::CurrentTime) => {
            if data.len() >= 4 {
                let time = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                Event::CurrentTime(time)
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::StatusResponse) => {
            // StatusResponse format: [reserved:1] [pubkey:6] [fields...]
            // Skip the reserved byte before parsing
            if data.len() > 1 {
                match parse_device_status(&data[1..]) {
                    Ok(status) => Event::StatusResponse(Box::new(status)),
                    Err(e) => {
                        tracing::warn!("failed to parse DeviceStatus: {}", e);
                        Event::Raw {
                            packet_type,
                            data: data.to_vec(),
                        }
                    }
                }
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::TelemetryResponse) => {
            // TelemetryResponse format: [reserved:1] [pubkey:6] [lpp_data...]
            // Skip reserved byte and pubkey, parse LPP data
            if data.len() > 7 {
                let lpp_data = &data[7..];
                let telemetry = Telemetry::parse_lpp(lpp_data);
                Event::TelemetryResponse(Box::new(telemetry))
            } else {
                Event::TelemetryResponse(Box::new(Telemetry::new()))
            }
        }
        Some(PacketType::Stats) => {
            // Stats type is in first byte, data follows
            if data.is_empty() {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            } else {
                let stats_type = crate::types::StatsType::from_byte(data[0]);
                let stats_data = &data[1..];

                let stats = match stats_type {
                    Some(crate::types::StatsType::Core) => {
                        parse_core_stats(stats_data).ok().map(StatsData::Core)
                    }
                    Some(crate::types::StatsType::Radio) => {
                        parse_radio_stats(stats_data).ok().map(StatsData::Radio)
                    }
                    Some(crate::types::StatsType::Packets) => {
                        parse_packet_stats(stats_data).ok().map(StatsData::Packets)
                    }
                    None => None,
                };

                if let Some(s) = stats {
                    Event::Stats(s)
                } else {
                    Event::Raw {
                        packet_type,
                        data: data.to_vec(),
                    }
                }
            }
        }
        Some(PacketType::LoginSuccess) => Event::LoginSuccess,
        Some(PacketType::LoginFailed) => Event::LoginFailed,
        Some(PacketType::PrivateKey) => {
            // Private key is 64 bytes (seed + public key)
            if data.len() >= 64 {
                let mut key = [0u8; 64];
                key.copy_from_slice(&data[..64]);
                Event::PrivateKey(key)
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::Disabled) => Event::Disabled,
        Some(PacketType::Signature) => {
            // Signature is variable length - read all remaining bytes
            if data.is_empty() {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            } else {
                Event::Signature(data.to_vec())
            }
        }
        Some(PacketType::ContactUri) => {
            // ContactUri is raw binary data, formatted as "meshcore://<hex>"
            let hex = hex::encode(data);
            let uri = format!("meshcore://{hex}");
            Event::ContactUri(uri)
        }
        Some(PacketType::PathUpdate) => {
            // Path update contains a 32-byte public key
            if data.len() >= 32 {
                let mut key_bytes = [0u8; 32];
                key_bytes.copy_from_slice(&data[..32]);
                Event::PathUpdate(PublicKey::from_bytes(&key_bytes))
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        Some(PacketType::RawData) => Event::RawData(data.to_vec()),
        Some(PacketType::LogData) => {
            let log = String::from_utf8_lossy(data).into_owned();
            Event::LogData(log)
        }
        Some(PacketType::TraceData) => Event::TraceData(data.to_vec()),
        Some(PacketType::CustomVars) => {
            let vars = String::from_utf8_lossy(data).into_owned();
            Event::CustomVars(vars)
        }
        Some(PacketType::BinaryResponse) => Event::BinaryResponse(data.to_vec()),
        Some(PacketType::PathDiscoveryResponse) => Event::PathDiscoveryResponse(data.to_vec()),
        Some(PacketType::ControlData) => Event::ControlData(data.to_vec()),
        Some(PacketType::SignStart) => {
            // SignStart has 1 reserved byte before the 4-byte max_length
            if data.len() >= 5 {
                let max_length = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                Event::SignStarted { max_length }
            } else {
                Event::Raw {
                    packet_type,
                    data: data.to_vec(),
                }
            }
        }
        _ => Event::Raw {
            packet_type,
            data: data.to_vec(),
        },
    };

    dispatcher.dispatch(event);
}

impl<T> Drop for MeshCore<T> {
    fn drop(&mut self) {
        // Abort background tasks
        if let Some(task) = self.read_task.take() {
            task.abort();
        }
        if let Some(task) = self.process_task.take() {
            task.abort();
        }
    }
}
