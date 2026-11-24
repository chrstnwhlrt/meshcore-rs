//! A simulated companion device behind a mock transport.

#![allow(dead_code)]

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use meshcore::transport::BoxFuture;
use meshcore::{FrameWriter, Link, LinkEvent, MeshCore, Options, Transport};
use tokio::sync::mpsc;

pub type Responder = Arc<dyn Fn(&[u8]) -> Vec<Vec<u8>> + Send + Sync>;

/// The test's handle on the simulated device.
#[derive(Clone)]
pub struct Device {
    commands: Arc<Mutex<Vec<Vec<u8>>>>,
    current: Arc<Mutex<Option<mpsc::Sender<LinkEvent>>>>,
    opens: Arc<AtomicUsize>,
    close_hangs: Arc<AtomicBool>,
    during_open: Arc<Mutex<Option<BoxFuture<'static, ()>>>>,
}

impl Device {
    /// Runs `action` inside the next open of the transport, before the link
    /// is up: a call that comes in while a reconnect opens the link.
    pub fn during_next_open(&self, action: impl Future<Output = ()> + Send + 'static) {
        *self.during_open.lock().unwrap() = Some(Box::pin(action));
    }

    /// Makes closing the link never finish, as a slow BLE disconnect.
    pub fn hang_on_close(&self) {
        self.close_hangs.store(true, Ordering::SeqCst);
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
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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

    /// Drops the link as a lost connection would.
    pub async fn drop_link(&self) {
        let sender = self.current.lock().unwrap().take().expect("not connected");
        sender
            .send(LinkEvent::Closed("test_disconnect".into()))
            .await
            .unwrap();
    }
}

struct MockTransport {
    responder: Responder,
    device: Device,
}

struct MockWriter {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    close_hangs: Arc<AtomicBool>,
}

impl FrameWriter for MockWriter {
    fn write_frame<'a>(&'a mut self, frame: &'a [u8]) -> BoxFuture<'a, meshcore::Result<()>> {
        let sent = self
            .frames
            .send(frame.to_vec())
            .map_err(|_| meshcore::Error::NotConnected);
        Box::pin(async move { sent })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        if self.close_hangs.load(Ordering::SeqCst) {
            Box::pin(std::future::pending())
        } else {
            Box::pin(async {})
        }
    }
}

impl Transport for MockTransport {
    fn open(&self) -> BoxFuture<'_, meshcore::Result<Link>> {
        Box::pin(async move {
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
                    close_hangs: Arc::clone(&self.device.close_hangs),
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
    let device = Device {
        commands: Arc::default(),
        current: Arc::default(),
        opens: Arc::default(),
        close_hangs: Arc::default(),
        during_open: Arc::default(),
    };
    let responder: Responder = Arc::new(move |command: &[u8]| match command.first() {
        Some(1) => vec![self_info_frame("Node")],
        Some(22) => vec![device_info_frame(1)],
        _ => respond(command),
    });
    let transport = MockTransport {
        responder,
        device: device.clone(),
    };
    (MeshCore::new(transport, options), device)
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
