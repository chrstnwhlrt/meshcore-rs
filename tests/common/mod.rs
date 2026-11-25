//! A simulated companion device behind a mock transport.

#![allow(dead_code)]

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use meshcore::transport::BoxFuture;
use meshcore::{FrameWriter, Link, LinkEvent, MeshCore, Options, PublicKey, Transport};
use tokio::sync::mpsc;

pub type Responder = Arc<dyn Fn(&[u8]) -> Vec<Vec<u8>> + Send + Sync>;

/// The test's handle on the simulated device.
#[derive(Clone)]
pub struct Device {
    commands: Arc<Mutex<Vec<Vec<u8>>>>,
    current: Arc<Mutex<Option<mpsc::Sender<LinkEvent>>>>,
    opens: Arc<AtomicUsize>,
    close_hangs: Arc<AtomicBool>,
    close_takes: Arc<Mutex<Duration>>,
    closes: Arc<AtomicUsize>,
    write_hangs: Arc<AtomicBool>,
    write_takes: Arc<Mutex<Duration>>,
    during_open: Arc<Mutex<Option<BoxFuture<'static, ()>>>>,
    exclusive: Arc<AtomicBool>,
    in_use: Arc<AtomicBool>,
}

/// The device's port from the start of an open to the end of closing.
struct Port(Arc<AtomicBool>);

impl Drop for Port {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Device {
    /// Makes opening fail while the device is in use, as a serial port's does.
    pub fn one_link_at_a_time(&self) {
        self.exclusive.store(true, Ordering::SeqCst);
    }

    /// Whether a link is open, being opened or not closed yet.
    pub fn in_use(&self) -> bool {
        self.in_use.load(Ordering::SeqCst)
    }

    /// Takes the port for an open.
    fn port(&self) -> meshcore::Result<Port> {
        let in_use = self.in_use.swap(true, Ordering::SeqCst);
        if in_use && self.exclusive.load(Ordering::SeqCst) {
            return Err(std::io::Error::from(std::io::ErrorKind::ResourceBusy).into());
        }
        Ok(Port(Arc::clone(&self.in_use)))
    }

    /// Runs `action` inside the next open of the transport, before the link
    /// is up: a call that comes in while a connect or reconnect opens the link.
    pub fn during_next_open(&self, action: impl Future<Output = ()> + Send + 'static) {
        *self.during_open.lock().unwrap() = Some(Box::pin(action));
    }

    /// Makes closing the link never finish, as a slow BLE disconnect.
    pub fn hang_on_close(&self) {
        self.close_hangs.store(true, Ordering::SeqCst);
    }

    /// Makes closing the next link that closes take this long.
    pub fn slow_close(&self, time: Duration) {
        *self.close_takes.lock().unwrap() = time;
    }

    /// Makes writing a frame take this long, as over BLE.
    pub fn slow_write(&self, time: Duration) {
        *self.write_takes.lock().unwrap() = time;
    }

    /// Makes writing a frame never finish, as when the device takes no bytes.
    pub fn hang_on_write(&self) {
        self.write_hangs.store(true, Ordering::SeqCst);
    }

    /// Every frame the client sent, in order.
    pub fn commands(&self) -> Vec<Vec<u8>> {
        self.commands.lock().unwrap().clone()
    }

    /// The frames the client sent with this command code.
    pub fn commands_with(&self, code: u8) -> Vec<Vec<u8>> {
        self.commands()
            .into_iter()
            .filter(|c| c.first() == Some(&code))
            .collect()
    }

    /// Lets the simulated device catch up with frames sent without a reply.
    pub async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    /// Whether the client holds the link opened last.
    pub fn has_client(&self) -> bool {
        let current = self.current.lock().unwrap();
        current.as_ref().is_some_and(|sender| !sender.is_closed())
    }

    /// How many links have been closed to the end.
    pub fn closes(&self) -> usize {
        self.closes.load(Ordering::SeqCst)
    }

    /// How often the transport was opened.
    pub fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }

    /// Sends a frame to the client unasked.
    pub async fn push(&self, frame: Vec<u8>) {
        let sender = self.current.lock().unwrap().clone().expect("not connected");
        sender.send(LinkEvent::Frame(frame)).await.unwrap();
    }

    /// Sends a frame to the client, if a link is up and has room for it.
    pub fn try_push(&self, frame: Vec<u8>) {
        if let Some(sender) = &*self.current.lock().unwrap() {
            let _ = sender.try_send(LinkEvent::Frame(frame));
        }
    }

    /// Drops the link as a lost connection would.
    pub async fn drop_link(&self) {
        let sender = self.current.lock().unwrap().take().expect("not connected");
        sender
            .send(LinkEvent::Closed("test_disconnect".into()))
            .await
            .unwrap();
    }

    /// Drops the link, if one is up.
    pub fn try_drop_link(&self) {
        if let Some(sender) = self.current.lock().unwrap().take() {
            let _ = sender.try_send(LinkEvent::Closed("test_disconnect".into()));
        }
    }
}

struct MockTransport {
    responder: Responder,
    device: Device,
}

struct MockWriter {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    device: Device,
    port: Option<Port>,
}

impl FrameWriter for MockWriter {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, meshcore::Result<()>> {
        Box::pin(async move {
            if self.device.write_hangs.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let time = *self.device.write_takes.lock().unwrap();
            if !time.is_zero() {
                tokio::time::sleep(time).await;
            }
            self.frames
                .send(frame.to_vec())
                .map_err(|_| meshcore::Error::NotConnected)
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if self.device.close_hangs.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            let time = std::mem::take(&mut *self.device.close_takes.lock().unwrap());
            if !time.is_zero() {
                tokio::time::sleep(time).await;
            }
            self.port = None;
            self.device.closes.fetch_add(1, Ordering::SeqCst);
        })
    }
}

impl Transport for MockTransport {
    fn open(&self) -> BoxFuture<'_, meshcore::Result<Link>> {
        Box::pin(async move {
            let port = self.device.port()?;
            let action = self.device.during_open.lock().unwrap().take();
            if let Some(action) = action {
                action.await;
            }
            let (to_client, events) = mpsc::channel(256);
            let (writer, mut from_client) = mpsc::unbounded_channel::<Vec<u8>>();
            *self.device.current.lock().unwrap() = Some(to_client.clone());
            self.device.opens.fetch_add(1, Ordering::SeqCst);
            let responder = Arc::clone(&self.responder);
            let commands = Arc::clone(&self.device.commands);
            tokio::spawn(async move {
                while let Some(command) = from_client.recv().await {
                    commands.lock().unwrap().push(command.clone());
                    for frame in responder(&command) {
                        if to_client.send(LinkEvent::Frame(frame)).await.is_err() {
                            return;
                        }
                    }
                }
            });
            Ok(Link {
                info: "mock".into(),
                events,
                writer: Box::new(MockWriter {
                    frames: writer,
                    device: self.device.clone(),
                    port: Some(port),
                }),
            })
        })
    }

    fn describe(&self) -> String {
        "mock".into()
    }
}

/// A client on a simulated device; `respond` answers commands the default
/// answers do not cover (`APP_START`, `DEVICE_QUERY`).
pub fn client(
    options: Options,
    respond: impl Fn(&[u8]) -> Vec<Vec<u8>> + Send + Sync + 'static,
) -> (MeshCore, Device) {
    client_on_device(options, move |command| match command.first() {
        Some(1) => vec![self_info_frame("Node")],
        Some(22) => vec![device_info_frame(1)],
        _ => respond(command),
    })
}

/// A client on a simulated device that answers only `APP_START`, and that
/// only when `answers` says so.
pub fn client_answering_app_start(
    answers: impl Fn() -> bool + Send + Sync + 'static,
) -> (MeshCore, Device) {
    client_on_device(Options::default(), move |command| {
        if command.first() == Some(&1) && answers() {
            vec![self_info_frame("Node")]
        } else {
            vec![]
        }
    })
}

/// A client on a simulated device that answers as `responder` does.
pub fn client_on_device(
    options: Options,
    responder: impl Fn(&[u8]) -> Vec<Vec<u8>> + Send + Sync + 'static,
) -> (MeshCore, Device) {
    let device = Device {
        commands: Arc::default(),
        current: Arc::default(),
        opens: Arc::default(),
        close_hangs: Arc::default(),
        close_takes: Arc::default(),
        closes: Arc::default(),
        write_hangs: Arc::default(),
        write_takes: Arc::default(),
        during_open: Arc::default(),
        exclusive: Arc::default(),
        in_use: Arc::default(),
    };
    let transport = MockTransport {
        responder: Arc::new(responder),
        device: device.clone(),
    };
    (MeshCore::new(transport, options), device)
}

/// Polls a future once; `true` when it is done.
pub async fn poll_once<T>(future: &mut Pin<Box<impl Future<Output = T>>>) -> bool {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_ready())).await
}

/// Random numbers, the same in every run (xorshift).
pub struct Random(u64);

impl Random {
    /// Numbers that depend on a seed (not 0).
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub const fn below(&mut self, limit: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % limit
    }

    pub fn millis(&mut self, limit: u64) -> Duration {
        Duration::from_millis(self.below(limit))
    }

    /// One of 16 keys.
    pub fn key(&mut self) -> PublicKey {
        PublicKey::new([self.below(16) as u8; 32])
    }
}

pub const KEY: [u8; 32] = [0x11; 32];

pub fn self_info_frame(name: &str) -> Vec<u8> {
    let mut f = vec![5, 1, 22, 22];
    f.extend([0x22; 32]);
    f.extend(53_551_100i32.to_le_bytes());
    f.extend(9_993_700i32.to_le_bytes());
    f.extend([0, 1, 0b10_01_10, 0]); // multi acks, loc policy, telemetry modes, manual add
    f.extend(869_525u32.to_le_bytes());
    f.extend(250_000u32.to_le_bytes());
    f.extend([11, 5]);
    f.extend(name.as_bytes());
    f
}

pub fn device_info_frame(path_hash_mode: u8) -> Vec<u8> {
    let mut f = vec![13, 14, 175, 40];
    f.extend(0u32.to_le_bytes());
    let mut build = [0u8; 12];
    build[..11].copy_from_slice(b"15 Oct 2026");
    f.extend(build);
    let mut model = [0u8; 40];
    model[..6].copy_from_slice(b"Heltec");
    f.extend(model);
    let mut ver = [0u8; 20];
    ver[..7].copy_from_slice(b"v1.18.0");
    f.extend(ver);
    f.extend([0, path_hash_mode]);
    f
}

pub fn contact_frame(code: u8, key: [u8; 32], name: &str, path_len: u8, path: &[u8]) -> Vec<u8> {
    let mut f = vec![code];
    f.extend(key);
    f.extend([2, 0, path_len]);
    let mut field = [0u8; 64];
    field[..path.len()].copy_from_slice(path);
    f.extend(field);
    let mut name_field = [0u8; 32];
    name_field[..name.len()].copy_from_slice(name.as_bytes());
    f.extend(name_field);
    f.extend(1000u32.to_le_bytes());
    f.extend(0i32.to_le_bytes());
    f.extend(0i32.to_le_bytes());
    f.extend(2000u32.to_le_bytes());
    f
}

pub fn sent_frame(flood: bool, tag: u32, timeout_ms: u32) -> Vec<u8> {
    let mut f = vec![6, u8::from(flood)];
    f.extend(tag.to_le_bytes());
    f.extend(timeout_ms.to_le_bytes());
    f
}

pub fn ok() -> Vec<u8> {
    vec![0]
}

pub fn err(code: u8) -> Vec<u8> {
    vec![1, code]
}

pub fn ack_frame(code: u32) -> Vec<u8> {
    let mut f = vec![0x82];
    f.extend(code.to_le_bytes());
    f.extend(321u32.to_le_bytes());
    f
}

pub fn binary_response(tag: u32, data: &[u8]) -> Vec<u8> {
    let mut f = vec![0x8c, 0];
    f.extend(tag.to_le_bytes());
    f.extend(data);
    f
}

/// 56 bytes of status fields (with the receive error count).
pub fn status_fields(battery_mv: u16) -> Vec<u8> {
    let mut f = battery_mv.to_le_bytes().to_vec();
    f.extend([0u8; 50]);
    f.extend(7u32.to_le_bytes());
    f
}
