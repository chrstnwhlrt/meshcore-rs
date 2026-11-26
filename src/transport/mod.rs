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
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{Error, Result};
use crate::protocol::frame::{FrameDecoder, encode};

/// A boxed future, as the object-safe transport traits return.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a link reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEvent {
    /// A complete frame from the device.
    Frame(Vec<u8>),
    /// The link is down; no further events follow. A write that is not done
    /// by then is given up.
    Closed(String),
}

/// Writes frames to the device.
pub trait FrameWriter: Send {
    /// Writes one frame (the payload; the transport adds any framing). When
    /// it cannot, it also reports the link [closed](LinkEvent::Closed), so that
    /// the client takes it down. When the link drops with a frame that went
    /// out, as with a command that restarts the device, it reports that too,
    /// but no error. The future is dropped unfinished only when the link is
    /// taken down.
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>>;

    /// Closes the link: when the client disconnects or is dropped, and after
    /// the link went down.
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
    /// Opens a link. The one before is closed by then, unless closing it
    /// takes more than 5 s. The client runs this in a task of its own and drops
    /// it unfinished when the caller of `connect` gives up, or when the client
    /// is connected, disconnected or dropped meanwhile.
    fn open(&self) -> BoxFuture<'_, Result<Link>>;

    /// A short description for logs (serial port, `host:port`, BLE device).
    fn describe(&self) -> String;
}

/// How many received frames a link buffers before the reader waits.
pub(crate) const LINK_CAPACITY: usize = 256;

/// How long writing to a byte stream may take before the link counts as lost
/// (as long as over BLE, where meshcore_py sets it). The system takes a frame
/// at once unless its buffer is full: the device has stopped reading.
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads a framed byte stream into link events until it ends. `received`
/// counts the frames (the TCP transport's liveness check uses it); `name`
/// prefixes the close reason.
pub(crate) fn spawn_stream_reader<R>(
    mut reader: R,
    events: mpsc::Sender<LinkEvent>,
    name: &'static str,
    received: Option<Arc<AtomicU32>>,
) -> JoinHandle<()>
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
                // meshcore_py reports any loss on the read side as a disconnect.
                Err(err) => {
                    log::debug!(target: "meshcore", "{name} read failed: {err}");
                    break format!("{name}_disconnect");
                }
            }
        };
        log::debug!(target: "meshcore", "link closed: {reason}");
        let _ = events.send(LinkEvent::Closed(reason)).await;
    })
}

/// Writes frames to a byte stream with the `<` framing. A write that fails
/// takes the link down, as in meshcore_py, and so does one that does not
/// finish, as there over BLE.
pub(crate) struct StreamWriter<W> {
    /// `None` once closed.
    writer: Option<W>,
    /// As [`spawn_stream_reader`] gets it.
    name: &'static str,
    reader_task: Option<JoinHandle<()>>,
    events: mpsc::Sender<LinkEvent>,
}

impl<W> StreamWriter<W> {
    pub(crate) const fn new(
        writer: W,
        name: &'static str,
        reader_task: JoinHandle<()>,
        events: mpsc::Sender<LinkEvent>,
    ) -> Self {
        Self {
            writer: Some(writer),
            name,
            reader_task: Some(reader_task),
            events,
        }
    }

    /// Reports the link lost.
    pub(crate) async fn report_lost(&mut self, reason: String) {
        let _ = self.events.send(LinkEvent::Closed(reason)).await;
        if let Some(task) = &self.reader_task {
            task.abort();
        }
    }
}

impl<W: AsyncWrite + Unpin> StreamWriter<W> {
    /// Writes a frame that has its framing.
    pub(crate) async fn write(&mut self, framed: &[u8]) -> Result<()> {
        let writer = self.writer.as_mut().ok_or(Error::NotConnected)?;
        // Not flushed: the bytes are the system's then, and flushing a serial
        // port blocks the thread until they are out.
        let written = tokio::time::timeout(STREAM_WRITE_TIMEOUT, writer.write_all(framed)).await;
        // meshcore_py's log texts, a timeout's as over BLE.
        let label = if self.name == "tcp" { "TCP" } else { "Serial" };
        let (reason, err) = match written {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(err)) => {
                log::warn!(target: "meshcore", "{label} write failed: {err}");
                (format!("{}_write_failed: {err}", self.name), err.into())
            }
            Err(_) => {
                log::warn!(
                    target: "meshcore",
                    "{label} write timed out after {:?}s",
                    STREAM_WRITE_TIMEOUT.as_secs_f64()
                );
                (
                    format!("{}_write_timeout", self.name),
                    Error::ConnectionLost,
                )
            }
        };
        self.report_lost(reason).await;
        Err(err)
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> FrameWriter for StreamWriter<W> {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let framed = encode(frame)?;
            self.write(&framed).await
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // The stream closes once both halves are gone, and only then can a
            // serial port be opened again. Closing one blocks the thread while
            // bytes are still on their way out.
            if let Some(task) = self.reader_task.take() {
                task.abort();
                let _ = task.await;
            }
            if let Some(writer) = self.writer.take() {
                let _ = tokio::task::spawn_blocking(move || drop(writer)).await;
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

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll};

    use super::*;

    type Write = dyn FnMut(&[u8]) -> Poll<std::io::Result<usize>> + Send;

    /// A port whose writes go this way, and that must not be flushed:
    /// flushing a real one blocks the thread.
    struct Port(Box<Write>);

    impl AsyncWrite for Port {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            (self.0)(buf)
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("flushed the port");
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            panic!("shut the port down, which flushes it");
        }
    }

    fn writer_on(
        write: impl FnMut(&[u8]) -> Poll<std::io::Result<usize>> + Send + 'static,
    ) -> (StreamWriter<Port>, mpsc::Receiver<LinkEvent>) {
        let (sender, events) = mpsc::channel(4);
        let reader = tokio::spawn(std::future::pending());
        let port = Port(Box::new(write));
        (StreamWriter::new(port, "serial", reader, sender), events)
    }

    #[tokio::test]
    async fn frames_are_written_without_flushing() {
        let (mut writer, mut events) = writer_on(|buf| Poll::Ready(Ok(buf.len())));
        writer.write_frame(&[22, 3]).await.unwrap();
        writer.close().await;
        assert!(events.try_recv().is_err());
        assert!(matches!(
            writer.write_frame(&[22, 3]).await,
            Err(Error::NotConnected)
        ));
    }

    #[tokio::test]
    async fn a_failed_write_takes_the_link_down() {
        // as on a port that was unplugged
        let (mut writer, mut events) =
            writer_on(|_| Poll::Ready(Err(std::io::Error::other("unplugged"))));
        assert!(matches!(
            writer.write_frame(&[22, 3]).await,
            Err(Error::Io(_))
        ));
        assert_eq!(
            events.recv().await,
            Some(LinkEvent::Closed("serial_write_failed: unplugged".into()))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_that_does_not_finish_takes_the_link_down() {
        // as when the device has stopped reading
        let (mut writer, mut events) = writer_on(|_| Poll::Pending);
        let started = tokio::time::Instant::now();
        assert!(matches!(
            writer.write_frame(&[22, 3]).await,
            Err(Error::ConnectionLost)
        ));
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        assert_eq!(
            events.recv().await,
            Some(LinkEvent::Closed("serial_write_timeout".into()))
        );
    }

    #[tokio::test]
    async fn frames_are_written_whole() {
        // a port that takes four bytes at a time
        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (mut writer, _events) = writer_on({
            let written = Arc::clone(&written);
            move |buf| {
                let len = buf.len().min(4);
                written.lock().unwrap().extend_from_slice(&buf[..len]);
                Poll::Ready(Ok(len))
            }
        });
        writer.write_frame(&[1, 2, 3]).await.unwrap();
        writer.write_frame(&[9]).await.unwrap();
        assert_eq!(
            *written.lock().unwrap(),
            [0x3c, 3, 0, 1, 2, 3, 0x3c, 1, 0, 9]
        );
    }
}
