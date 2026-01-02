//! Transport layer for `MeshCore` communication.
//!
//! This module provides the abstraction for different transport methods.
//! Currently only USB/Serial is implemented.

pub mod serial;

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;

use crate::error::Result;

/// Trait for transport implementations.
pub trait Transport: Send + Sync {
    /// Connects to the device.
    fn connect(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Disconnects from the device.
    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Sends data to the device.
    fn send(&mut self, data: Bytes) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;

    /// Returns true if connected.
    fn is_connected(&self) -> bool;
}

pub use serial::SerialTransport;
