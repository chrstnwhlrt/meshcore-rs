//! Serial (USB) transport.

use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio_serial::{SerialPort, SerialPortBuilderExt};

use crate::error::Result;
use crate::transport::{
    BoxFuture, LINK_CAPACITY, Link, StreamWriter, Transport, spawn_stream_reader,
};

/// The baud rate MeshCore companion radios use.
pub const DEFAULT_BAUD_RATE: u32 = 115_200;

/// Serial port settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    /// The port, e.g. `/dev/ttyUSB0` or `COM3`.
    pub port: String,
    /// The baud rate. Boards with native USB ignore it, but those on the
    /// Adafruit nRF52 core (such as the LilyGO T-Echo) start their bootloader
    /// in place of the firmware when DTR drops at 1200 baud, as it does when
    /// the port closes, and stay there until reset: do not use 1200 with them.
    pub baud_rate: u32,
    /// DTR after opening; `None` leaves the line as the OS sets it.
    pub dtr: Option<bool>,
    /// RTS after opening; `None` leaves the line as the OS sets it.
    pub rts: Option<bool>,
    /// How long to wait after opening before reading.
    pub settle_delay: Duration,
    /// How long to read and drop stale bytes after the settle delay.
    pub drain_duration: Duration,
}

impl SerialConfig {
    /// Default settings for a port: 115200 baud, DTR and RTS untouched.
    #[must_use]
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud_rate: DEFAULT_BAUD_RATE,
            dtr: None,
            rts: None,
            settle_delay: Duration::from_millis(300),
            drain_duration: Duration::from_millis(500),
        }
    }

    /// Sets the baud rate (not 1200 for an nRF52 board: see the field).
    #[must_use]
    pub const fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }

    /// Sets DTR after opening.
    #[must_use]
    pub const fn dtr(mut self, dtr: Option<bool>) -> Self {
        self.dtr = dtr;
        self
    }

    /// Sets RTS after opening.
    #[must_use]
    pub const fn rts(mut self, rts: Option<bool>) -> Self {
        self.rts = rts;
        self
    }

    /// Sets how long to wait after opening (default 300 ms; boards that
    /// reset on open need it).
    #[must_use]
    pub const fn settle_delay(mut self, delay: Duration) -> Self {
        self.settle_delay = delay;
        self
    }

    /// Sets how long to read and drop stale bytes after the settle delay
    /// (default 500 ms).
    #[must_use]
    pub const fn drain_duration(mut self, duration: Duration) -> Self {
        self.drain_duration = duration;
        self
    }
}

/// Connects over a serial port.
#[derive(Debug, Clone)]
pub struct SerialTransport {
    config: SerialConfig,
}

impl SerialTransport {
    /// A transport for the given settings.
    #[must_use]
    pub const fn new(config: SerialConfig) -> Self {
        Self { config }
    }

    /// The settings.
    #[must_use]
    pub const fn config(&self) -> &SerialConfig {
        &self.config
    }
}

impl Transport for SerialTransport {
    fn open(&self) -> BoxFuture<'_, Result<Link>> {
        Box::pin(async move {
            let config = &self.config;
            log::debug!(target: "meshcore", "opening serial port {}", config.port);
            let mut stream =
                tokio_serial::new(&config.port, config.baud_rate).open_native_async()?;
            if let Some(dtr) = config.dtr {
                stream.write_data_terminal_ready(dtr)?;
            }
            if let Some(rts) = config.rts {
                stream.write_request_to_send(rts)?;
            }
            tokio::time::sleep(config.settle_delay).await;

            // Some boards print boot output or hold stale frames; drop them.
            let mut drained = 0usize;
            let mut buf = [0u8; 1024];
            let deadline = crate::util::deadline_after(config.drain_duration);
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(Duration::from_millis(20), stream.read(&mut buf)).await {
                    Ok(Ok(n)) if n > 0 => drained += n,
                    _ => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
            if drained > 0 {
                log::debug!(target: "meshcore", "dropped {drained} stale bytes");
            }

            let (reader, writer) = tokio::io::split(stream);
            let (sender, events) = mpsc::channel(LINK_CAPACITY);
            let reader_task = spawn_stream_reader(reader, sender.clone(), "serial", None);
            log::info!(target: "meshcore", "Serial Connection started");
            Ok(Link {
                info: config.port.clone(),
                events,
                writer: Box::new(StreamWriter::new(writer, "serial", reader_task, sender)),
            })
        })
    }

    fn describe(&self) -> String {
        self.config.port.clone()
    }
}

/// The names of the serial ports the system has.
///
/// # Errors
///
/// [`Error::Serial`](crate::Error::Serial) when the ports cannot be listed,
/// as on Linux without a readable `/sys/class/tty` (in some containers).
pub fn list_ports() -> Result<Vec<String>> {
    // There the serialport crate would panic.
    #[cfg(target_os = "linux")]
    if std::fs::read_dir("/sys/class/tty").is_err() {
        return Err(tokio_serial::Error::new(
            tokio_serial::ErrorKind::Unknown,
            "the serial ports cannot be listed without a readable /sys/class/tty",
        )
        .into());
    }
    Ok(tokio_serial::available_ports()?
        .into_iter()
        .map(|port| port.port_name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_leave_lines_alone() {
        let config = SerialConfig::new("/dev/ttyUSB0");
        assert_eq!(config.baud_rate, 115_200);
        assert_eq!((config.dtr, config.rts), (None, None));
        assert_eq!(config.settle_delay, Duration::from_millis(300));
        assert_eq!(config.drain_duration, Duration::from_millis(500));
        let config = config.baud_rate(9600).dtr(Some(true));
        assert_eq!(config.baud_rate, 9600);
        assert_eq!((config.dtr, config.rts), (Some(true), None));
    }

    #[test]
    fn ports_are_listed_or_the_reason_given() {
        // never a panic, also without /sys/class/tty (as in the Nix sandbox)
        match list_ports() {
            Ok(ports) => assert!(ports.iter().all(|port| !port.is_empty())),
            Err(err) => assert!(matches!(err, crate::Error::Serial(_)), "{err}"),
        }
    }

    #[test]
    fn config_builders_set_the_delays() {
        let config = SerialConfig::new("COM3")
            .rts(Some(false))
            .settle_delay(Duration::ZERO)
            .drain_duration(Duration::from_millis(50));
        assert_eq!(config.port, "COM3");
        assert_eq!(config.rts, Some(false));
        assert_eq!(config.settle_delay, Duration::ZERO);
        assert_eq!(config.drain_duration, Duration::from_millis(50));
    }
}
