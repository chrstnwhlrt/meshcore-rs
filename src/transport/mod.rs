//! Transports: how frames get to and from the device.
//!
//! A [`Transport`] opens a [`Link`]: a channel of received frames and a
//! [`FrameWriter`]. Serial and TCP both carry a byte stream with the same
//! framing (see [`protocol::frame`](crate::protocol::frame)); BLE (feature
//! `ble`, Linux) carries one frame per write or notification. Own transports
//! (another stream, a test double) implement [`Transport`] and go to
//! [`MeshCore::new`](crate::MeshCore::new).

#[cfg(all(feature = "ble", target_os = "linux"))]
pub mod ble;
pub mod serial;
pub mod tcp;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::error::Result;
use crate::protocol::frame::{FrameDecoder, encode};

/// A boxed future, as the object-safe transport traits return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a link reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// A complete frame from the device.
    Frame(Vec<u8>),
    /// The link is down; no further events follow.
    Closed(String),
}

/// Writes frames to the device.
pub trait FrameWriter: Send {
    /// Writes one frame (the payload; the transport adds any framing).
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    /// Closes the link.
    fn close(&mut self) -> BoxFuture<'_, ()>;
}

/// An open connection to the device.
pub struct Link {
    /// Where the link goes (serial port, `host:port` or BLE address).
    pub info: String,
    /// Frames from the device, ending with [`LinkEvent::Closed`].
    pub events: mpsc::Receiver<LinkEvent>,
    /// Writes frames to the device.
    pub writer: Box<dyn FrameWriter>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// A way to reach the device.
pub trait Transport: Send + Sync + 'static {
    /// Opens a link.
    fn open(&self) -> BoxFuture<'_, Result<Link>>;

    /// A short description for logs (serial port, `host:port`, BLE device).
    fn describe(&self) -> String;
}

/// How many received frames a link buffers before the reader waits.
pub(crate) const LINK_CAPACITY: usize = 256;

/// Reads a framed byte stream into link events until it ends. `received`
/// counts the frames (the TCP transport's liveness check uses it); `name`
/// prefixes the close reason.
pub(crate) fn spawn_stream_reader<R>(
    mut reader: R,
    events: mpsc::Sender<LinkEvent>,
    name: &'static str,
    received: Option<Arc<AtomicU32>>,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut decoder = FrameDecoder::new();
        let mut buf = [0u8; 1024];
        let reason = loop {
            match reader.read(&mut buf).await {
                Ok(0) => break format!("{name}_disconnect"),
                Ok(n) => {
                    log::trace!(target: "meshcore", "received {n} bytes");
                    for frame in decoder.feed(&buf[..n]) {
                        if let Some(counter) = &received {
                            counter.fetch_add(1, Ordering::Relaxed);
                        }
                        if events.send(LinkEvent::Frame(frame)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => break format!("{name}_read_failed: {err}"),
            }
        };
        log::debug!(target: "meshcore", "link closed: {reason}");
        let _ = events.send(LinkEvent::Closed(reason)).await;
    })
}

/// Writes frames to a byte stream with the `<` framing.
pub(crate) struct StreamWriter<W> {
    writer: W,
    reader_task: Option<tokio::task::JoinHandle<()>>,
}

impl<W> StreamWriter<W> {
    pub(crate) const fn new(writer: W, reader_task: tokio::task::JoinHandle<()>) -> Self {
        Self {
            writer,
            reader_task: Some(reader_task),
        }
    }
}

impl<W: AsyncWrite + Unpin + Send> FrameWriter for StreamWriter<W> {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let framed = encode(frame)?;
            self.writer.write_all(&framed).await?;
            self.writer.flush().await?;
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

impl<W> Drop for StreamWriter<W> {
    fn drop(&mut self) {
        if let Some(task) = self.reader_task.take() {
            task.abort();
        }
    }
}
