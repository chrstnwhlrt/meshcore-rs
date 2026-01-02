//! # meshcore
//!
//! A Rust client library for `MeshCore` mesh networking devices.
//!
//! This library provides async communication with `MeshCore` devices over USB/Serial.
//!
//! ## Features
//!
//! - Async/await based API using Tokio
//! - Event-driven architecture for handling device notifications
//! - Type-safe protocol implementation
//! - Comprehensive error handling
//!
//! ## Quick Start
//!
//! ```no_run
//! use meshcore::MeshCore;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), meshcore::Error> {
//!     // Connect to a `MeshCore` device
//!     let mut client = MeshCore::serial("/dev/ttyUSB0");
//!     let info = client.connect().await?;
//!
//!     println!("Connected to: {}", info.name);
//!     println!("Public key: {}", info.public_key);
//!
//!     // Get battery status
//!     let battery = client.get_battery().await?;
//!     println!("Battery: {}mV", battery.millivolts);
//!
//!     // Disconnect
//!     client.disconnect().await?;
//!     Ok(())
//! }
//! ```
//!
//! ## Architecture
//!
//! The library is organized into several modules:
//!
//! - [`protocol`] - Low-level protocol types (frames, packets, commands)
//! - [`types`] - Data structures (contacts, devices, messages, statistics)
//! - [`transport`] - Transport implementations (currently USB/Serial)
//! - [`event`] - Async event system for handling notifications
//! - [`commands`] - Command handler for device operations
//! - [`client`] - High-level [`MeshCore`] client

pub mod client;
pub mod commands;
pub mod error;
pub mod event;
pub mod protocol;
pub mod transport;
pub mod types;

// Re-exports for convenience
pub use client::MeshCore;
pub use commands::ContactUpdateParams;
pub use error::{Error, FrameError, Result};
pub use event::{Event, EventDispatcher, EventFilter, StatsData, Subscription};
pub use protocol::{BinaryReqType, CommandOpcode, PacketType, StatsType};
pub use transport::{SerialTransport, serial::list_ports};
pub use types::{
    Acknowledgment, BatteryStatus, Channel, ChannelMessage, Contact, ContactFlags, ContactMessage,
    ContactType, CoreStats, DeviceInfo, DeviceStatus, PacketStats, PublicKey, RadioConfig,
    RadioStats, SelfInfo, SignalQuality, Telemetry, TelemetryMode, TelemetryReading,
    TelemetryValue, TextType,
};
