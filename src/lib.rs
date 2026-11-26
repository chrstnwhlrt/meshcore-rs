//! # meshcore
//!
//! A client library for [MeshCore](https://meshcore.co.uk) companion radios
//! over serial (USB), TCP (WiFi, Ethernet) and BLE, ported from the Python
//! library [meshcore_py](https://github.com/meshcore-dev/meshcore_py).
//!
//! ```no_run
//! use meshcore::{MeshCore, Options, SerialConfig};
//!
//! # async fn demo() -> meshcore::Result<()> {
//! let mc = MeshCore::create_serial(SerialConfig::new("/dev/ttyUSB0"), Options::default()).await?;
//! println!("connected to {}", mc.self_info().map(|i| i.name).unwrap_or_default());
//!
//! let battery = mc.get_bat().await?;
//! println!("battery: {} mV", battery.level);
//!
//! for contact in mc.get_contacts(0).await?.contacts {
//!     println!("{} {}", contact.public_key, contact.name);
//! }
//! mc.disconnect().await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## How it works
//!
//! - Every command is an async method on [`MeshCore`], named as in meshcore_py
//!   (a few are this library's own), that waits for the device's reply, if it
//!   sends one, and returns it typed. Device errors come back as
//!   [`Error::Device`] with the firmware's [`ErrorCode`]. One command is in
//!   flight at a time.
//! - Input the firmware would refuse, cut short or read differently, such as
//!   a text that is too long, empty or holds a NUL byte, or a setting out of
//!   its range, is refused before sending with [`Error::InvalidArgument`]
//!   where the client can tell without asking the device. The README lists
//!   the cases meshcore_py sends.
//! - Every frame the device sends becomes an [`Event`] (an empty frame or a
//!   push that cannot be read is only logged); [`MeshCore::subscribe`]
//!   delivers them, [`MeshCore::wait_for`] waits for one.
//! - Answers from remote nodes (status, telemetry, neighbours, ...) are matched
//!   to their request by tag; the `*_sync` methods wait for them.
//! - The client keeps the device's self info, a contact cache, contacts heard
//!   but not added, and the channel secrets for decoding the RX log.
//! - It logs through the `log` crate with the target `meshcore`; where
//!   meshcore_py logs the same case at info level or above, the message has
//!   its wording (a command without a link and input refused before sending
//!   fail without a line).
//!
//! ## Transports
//!
//! [`MeshCore::create_serial`] for USB/serial companions and
//! [`MeshCore::create_tcp`] for WiFi and Ethernet companions are always built;
//! `MeshCore::create_ble` for BLE companions comes with the `ble` feature.
//! Other links implement [`Transport`].
//!
//! ## Features
//!
//! - `ble`: BLE transport over the Nordic UART service through BlueZ (Linux
//!   only, needs libdbus; without effect on other systems).
//! - `serde`: `serde::Serialize` with meshcore_py's payload field names.

pub mod client;
pub mod error;
pub mod event;
pub mod hex;
pub mod lpp;
mod packet;
pub mod protocol;
#[cfg(feature = "serde")]
mod serde_impl;
mod state;
pub mod transport;
pub mod types;
mod util;

pub use client::{
    FloodScope, MAX_CHANNEL_DATA_LENGTH, MAX_TEXT_LENGTH, MeshCore, Options, RequestOptions,
    RetryOptions, TraceRequest, parse_trace_path,
};
pub use error::{Error, ErrorCode, Result};
pub use event::{ErrorInfo, Event, EventKind, Subscription};
pub use lpp::{LppReading, LppType, LppValue, Telemetry};
pub use protocol::{
    AnonReqType, BinaryReqType, CommandType, ControlType, PacketType, PathLen, StatsType,
};
#[cfg(all(feature = "ble", target_os = "linux"))]
pub use transport::ble::{BleConfig, BleDevice, BleTransport};
pub use transport::serial::{SerialConfig, SerialTransport, list_ports};
pub use transport::tcp::TcpTransport;
pub use transport::{FrameWriter, Link, LinkEvent, Transport};
pub use types::*;

// The README's examples compile as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
