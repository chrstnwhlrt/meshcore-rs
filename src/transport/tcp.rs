//! TCP transport for companion radios on WiFi.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::protocol::frame::encode;
use crate::transport::{
    BoxFuture, FrameWriter, LINK_CAPACITY, Link, LinkEvent, Transport, spawn_stream_reader,
};

/// When the next frame would make this many more sent than received, the
/// connection is taken for dead instead of sending it (as meshcore_py's TCP
/// transport does).
const NO_RESPONSE_THRESHOLD: u32 = 5;

/// Connects over TCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpTransport {
    host: String,
    port: u16,
    connect_timeout: Duration,
}

impl TcpTransport {
    /// A transport to `host:port`.
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            connect_timeout: Duration::from_secs(10),
        }
    }

    /// Sets how long to wait for the TCP connection (default 10 s).
    #[must_use]
    pub const fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

impl Transport for TcpTransport {
    fn open(&self) -> BoxFuture<'_, Result<Link>> {
        Box::pin(async move {
            let target = self.describe();
            log::info!(target: "meshcore", "connecting to {target}");
            let stream = tokio::time::timeout(
                self.connect_timeout,
                TcpStream::connect((self.host.as_str(), self.port)),
            )
            .await
            .map_err(|_| Error::Timeout)??;
            stream.set_nodelay(true)?;
            let (reader, writer) = stream.into_split();
            let (sender, events) = mpsc::channel(LINK_CAPACITY);
            let received = Arc::new(AtomicU32::new(0));
            let reader_task =
                spawn_stream_reader(reader, sender.clone(), "tcp", Some(Arc::clone(&received)));
            log::info!(target: "meshcore", "TCP connection started");
            Ok(Link {
                info: target,
                events,
                writer: Box::new(TcpWriter {
                    writer,
                    reader_task: Some(reader_task),
                    events: sender,
                    sent: 0,
                    received,
                }),
            })
        })
    }

    fn describe(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

struct TcpWriter {
    writer: OwnedWriteHalf,
    reader_task: Option<tokio::task::JoinHandle<()>>,
    events: mpsc::Sender<LinkEvent>,
    sent: u32,
    received: Arc<AtomicU32>,
}

impl FrameWriter for TcpWriter {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.sent = self.sent.wrapping_add(1);
            let received = self.received.load(Ordering::Relaxed);
            // More frames received than sent (pushes) counts as answered.
            if self.sent.saturating_sub(received) >= NO_RESPONSE_THRESHOLD {
                log::debug!(
                    target: "meshcore",
                    "TCP link taken for dead: {} frames sent, {received} received",
                    self.sent
                );
                if let Some(task) = self.reader_task.take() {
                    task.abort();
                }
                let _ = self
                    .events
                    .send(LinkEvent::Closed("tcp_no_response".into()))
                    .await;
                return Err(Error::ConnectionLost);
            }
            let framed = encode(frame)?;
            self.writer.write_all(&framed).await?;
            Ok(())
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.writer.shutdown().await;
            if let Some(task) = self.reader_task.take() {
                task.abort();
            }
        })
    }
}

impl Drop for TcpWriter {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}
