//! Serial/USB transport implementation.
//!
//! This module provides serial port communication for `MeshCore` devices
//! connected via USB.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Mutex, mpsc};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

use crate::error::{Error, Result};
use crate::protocol::{FrameDecoder, encode_frame};
use crate::transport::Transport;

/// Default baud rate for `MeshCore` devices.
pub const DEFAULT_BAUD_RATE: u32 = 115_200;

/// Default connection delay.
pub const DEFAULT_CONNECTION_DELAY: Duration = Duration::from_millis(300);

/// Configuration for serial transport.
#[derive(Debug, Clone)]
pub struct SerialConfig {
    /// Serial port path (e.g., "/dev/ttyUSB0" or "COM3").
    pub port: String,
    /// Baud rate.
    pub baud_rate: u32,
    /// Delay after connection before sending commands.
    pub connection_delay: Duration,
}

impl SerialConfig {
    /// Creates a new serial configuration with default settings.
    #[must_use]
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud_rate: DEFAULT_BAUD_RATE,
            connection_delay: DEFAULT_CONNECTION_DELAY,
        }
    }

    /// Sets the baud rate.
    #[must_use]
    pub const fn baud_rate(mut self, rate: u32) -> Self {
        self.baud_rate = rate;
        self
    }

    /// Sets the connection delay.
    #[must_use]
    pub const fn connection_delay(mut self, delay: Duration) -> Self {
        self.connection_delay = delay;
        self
    }
}

/// Serial transport for `MeshCore` communication.
///
/// Uses split read/write halves to allow concurrent reading and writing.
pub struct SerialTransport {
    config: SerialConfig,
    writer: Option<Arc<Mutex<WriteHalf<SerialStream>>>>,
    reader: Option<ReadHalf<SerialStream>>,
    decoder: FrameDecoder,
    frame_tx: Option<mpsc::Sender<Bytes>>,
}

impl SerialTransport {
    /// Creates a new serial transport with the given configuration.
    #[must_use]
    pub fn new(config: SerialConfig) -> Self {
        Self {
            config,
            writer: None,
            reader: None,
            decoder: FrameDecoder::new(),
            frame_tx: None,
        }
    }

    /// Creates a new serial transport for the given port with default settings.
    #[must_use]
    pub fn with_port(port: impl Into<String>) -> Self {
        Self::new(SerialConfig::new(port))
    }

    /// Sets the frame receiver channel.
    ///
    /// Received frames will be sent to this channel.
    pub fn set_frame_sender(&mut self, tx: mpsc::Sender<Bytes>) {
        self.frame_tx = Some(tx);
    }

    /// Takes the reader half for use in a background task.
    ///
    /// This can only be called once after connecting.
    pub fn take_reader(&mut self) -> Option<ReadHalf<SerialStream>> {
        self.reader.take()
    }

    /// Gets the frame decoder.
    pub fn decoder_mut(&mut self) -> &mut FrameDecoder {
        &mut self.decoder
    }

    /// Gets the frame sender channel.
    #[must_use]
    pub fn frame_tx(&self) -> Option<mpsc::Sender<Bytes>> {
        self.frame_tx.clone()
    }

    /// Runs the read loop with a given reader, processing incoming data.
    ///
    /// This should be spawned as a separate task.
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails or the connection is lost.
    pub async fn run_read_loop_with_reader(
        mut reader: ReadHalf<SerialStream>,
        mut decoder: FrameDecoder,
        frame_tx: mpsc::Sender<Bytes>,
    ) -> Result<()> {
        let mut buf = [0u8; 1024];

        loop {
            let n = match reader.read(&mut buf).await {
                Ok(0) => {
                    tracing::debug!("serial port closed");
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        "serial port closed",
                    )));
                }
                Ok(n) => n,
                Err(e) => {
                    tracing::error!("serial read error: {}", e);
                    return Err(Error::Io(e));
                }
            };

            tracing::trace!("received {} bytes", n);
            decoder.feed(&buf[..n]);

            // Process all complete frames
            loop {
                match decoder.decode() {
                    Ok(Some(frame)) => {
                        tracing::trace!("decoded frame: {} bytes", frame.len());
                        if frame_tx.send(frame).await.is_err() {
                            tracing::debug!("frame receiver dropped");
                            return Ok(());
                        }
                    }
                    Ok(None) => break, // Need more data
                    Err(e) => {
                        tracing::warn!("frame decode error: {}", e);
                        // Continue processing - the decoder skips invalid bytes
                    }
                }
            }
        }
    }
}

impl Transport for SerialTransport {
    fn connect(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.writer.is_some() {
                return Ok(());
            }

            tracing::info!("connecting to serial port: {}", self.config.port);

            let mut stream = tokio_serial::new(&self.config.port, self.config.baud_rate)
                .open_native_async()
                .map_err(Error::Serial)?;

            // Set RTS to false, matching Python behavior
            // This is important for proper device initialization
            if let Err(e) = tokio_serial::SerialPort::write_request_to_send(&mut stream, false) {
                tracing::warn!("failed to set RTS: {}", e);
            }

            // Wait for device to be ready
            tokio::time::sleep(self.config.connection_delay).await;

            // Drain any stale data from the device buffer
            // Some devices send data shortly after connection opens
            let mut buf = [0u8; 1024];
            let mut total_drained = 0usize;

            // Try draining for up to 500ms with multiple read attempts
            let drain_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
            while tokio::time::Instant::now() < drain_deadline {
                match tokio::time::timeout(Duration::from_millis(20), stream.read(&mut buf)).await {
                    Ok(Ok(n)) if n > 0 => {
                        total_drained += n;
                    }
                    _ => {
                        // Brief pause then try again
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }

            if total_drained > 0 {
                tracing::debug!("drained {} stale bytes from buffer", total_drained);
            }

            // Split the stream into read and write halves
            let (reader, writer) = tokio::io::split(stream);
            self.reader = Some(reader);
            self.writer = Some(Arc::new(Mutex::new(writer)));
            self.decoder.clear();

            tracing::info!("connected to serial port");
            Ok(())
        })
    }

    fn disconnect(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            if self.writer.is_some() || self.reader.is_some() {
                tracing::info!("disconnecting from serial port");
                self.writer = None;
                self.reader = None;
            }
            Ok(())
        })
    }

    fn send(&mut self, data: Bytes) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        let writer = self.writer.clone();
        Box::pin(async move {
            let writer = writer.ok_or(Error::NotConnected)?;
            let mut writer = writer.lock().await;

            let frame = encode_frame(&data);
            tracing::trace!("sending frame: {} bytes", frame.len());

            writer.write_all(&frame).await.map_err(Error::Io)?;
            writer.flush().await.map_err(Error::Io)?;

            Ok(())
        })
    }

    fn is_connected(&self) -> bool {
        self.writer.is_some()
    }
}

/// Lists available serial ports.
///
/// # Errors
///
/// Returns an error if the port list cannot be retrieved.
pub fn list_ports() -> Result<Vec<String>> {
    let ports = tokio_serial::available_ports().map_err(Error::Serial)?;
    Ok(ports.into_iter().map(|p| p.port_name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serial_config_defaults() {
        let config = SerialConfig::new("/dev/ttyUSB0");
        assert_eq!(config.port, "/dev/ttyUSB0");
        assert_eq!(config.baud_rate, DEFAULT_BAUD_RATE);
    }

    #[test]
    fn test_serial_config_builder() {
        let config = SerialConfig::new("/dev/ttyUSB0")
            .baud_rate(9600)
            .connection_delay(Duration::from_secs(1));
        assert_eq!(config.baud_rate, 9600);
        assert_eq!(config.connection_delay, Duration::from_secs(1));
    }

    #[test]
    #[ignore = "Requires /sys/class/tty - not available in sandboxed builds"]
    fn test_list_ports() {
        // Just verify it doesn't panic
        let _ = list_ports();
    }
}
