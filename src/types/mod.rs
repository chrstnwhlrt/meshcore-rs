//! Data types for `MeshCore` entities.
//!
//! This module contains the core data structures used throughout the library:
//! - Contacts and public keys
//! - Device information
//! - Messages
//! - Statistics
//! - Telemetry

pub mod contact;
pub mod device;
pub mod message;
pub mod stats;
pub mod telemetry;

pub use contact::{Contact, ContactFlags, ContactType, PublicKey};
pub use device::{BatteryStatus, Channel, DeviceInfo, RadioConfig, SelfInfo, TelemetryMode};
pub use message::{Acknowledgment, ChannelMessage, ContactMessage, SignalQuality, TextType};
pub use stats::{CoreStats, DeviceStatus, PacketStats, RadioStats, StatsType};
pub use telemetry::{Telemetry, TelemetryReading, TelemetryValue};
