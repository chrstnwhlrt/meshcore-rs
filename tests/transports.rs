//! The real transports end to end: TCP over a local socket and serial over a
//! pseudo terminal, with a simulated device speaking the companion framing on
//! the other end.
//!
//! These tests run on real time. Tokio's paused clock would let timeouts fire
//! while the kernel is still moving the bytes.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{device_info_frame, err, self_info_frame};
use meshcore::{Error, Event, MeshCore, Options, Subscription, TcpTransport, Transport};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

/// How long a test waits for something that should happen right away.
const PATIENCE: Duration = Duration::from_secs(5);

/// `APP_START` as the client sends it.
const APP_START: &[u8] = b"\x01\x03      mccli";

/// What the simulated device does with a command.
enum Reply {
    /// Writes these chunks, one write each.
    Write(Vec<Vec<u8>>),
    /// Closes the connection without answering.
    Close,
}

type Responder = Arc<dyn Fn(&[u8]) -> Reply + Send + Sync>;

/// A device frame with the `>` framing.
fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x3e];
    frame.extend(u16::try_from(payload.len()).unwrap().to_le_bytes());
    frame.extend(payload);
    frame
}

/// The default answers: session start, device info and the battery (3540 mV).
fn answer(command: &[u8]) -> Reply {
    let frame = match command.first() {
        Some(1) => self_info_frame("Node"),
        Some(22) => device_info_frame(1),
        Some(20) => vec![12, 0xd4, 0x0d],
        _ => err(1),
    };
    Reply::Write(vec![framed(&frame)])
}

/// Takes the next host frame (`<` framing) out of `buf`.
fn take_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buf.iter().position(|&b| b == 0x3c)?;
    buf.drain(..start);
    let len = usize::from(u16::from_le_bytes([*buf.get(1)?, *buf.get(2)?]));
    let frame = buf.get(3..3 + len)?.to_vec();
    buf.drain(..3 + len);
    Some(frame)
}

/// Runs the simulated device on a byte stream until the stream ends or a
/// reply closes it, recording every command.
async fn run_device<S>(stream: S, commands: Arc<Mutex<Vec<Vec<u8>>>>, respond: Responder)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        while let Some(command) = take_frame(&mut buf) {
            commands.lock().unwrap().push(command.clone());
            match respond(&command) {
                Reply::Write(chunks) => {
                    let pause = chunks.len() > 1;
                    for chunk in chunks {
                        writer.write_all(&chunk).await.unwrap();
                        writer.flush().await.unwrap();
                        if pause {
                            // separate writes, so the client sees them apart
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                }
                Reply::Close => return,
            }
        }
    }
}

/// Waits for the next `Disconnected` event and returns its reason.
async fn disconnected(events: &mut Subscription) -> String {
    timeout(PATIENCE, async {
        loop {
            if let Event::Disconnected { reason } = events.recv().await.unwrap() {
                return reason;
            }
        }
    })
    .await
    .expect("no Disconnected event")
}

/// Options with a short command timeout.
fn quick_options() -> Options {
    Options {
        default_timeout: Duration::from_millis(300),
        ..Options::default()
    }
}

// ------------------------------------------------------------------- TCP

/// A device behind a local TCP port, taking one connection after the other.
struct TcpDevice {
    port: u16,
    commands: Arc<Mutex<Vec<Vec<u8>>>>,
    connections: Arc<AtomicUsize>,
}

impl TcpDevice {
    async fn start(respond: impl Fn(&[u8]) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let commands = Arc::<Mutex<Vec<Vec<u8>>>>::default();
        let connections = Arc::<AtomicUsize>::default();
        let respond: Responder = Arc::new(respond);
        let (recorded, counted) = (Arc::clone(&commands), Arc::clone(&connections));
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                stream.set_nodelay(true).unwrap();
                counted.fetch_add(1, Ordering::SeqCst);
                run_device(stream, Arc::clone(&recorded), Arc::clone(&respond)).await;
            }
        });
        Self {
            port,
            commands,
            connections,
        }
    }

    fn commands(&self) -> Vec<Vec<u8>> {
        self.commands.lock().unwrap().clone()
    }

    async fn connect(&self, options: Options) -> MeshCore {
        timeout(
            PATIENCE,
            MeshCore::create_tcp("127.0.0.1", self.port, options),
        )
        .await
        .expect("connect took too long")
        .expect("connect failed")
    }
}

#[tokio::test]
async fn tcp_session_end_to_end() {
    let device = TcpDevice::start(answer).await;
    let mc = device.connect(Options::default()).await;
    assert!(mc.is_connected());
    assert_eq!(mc.self_info().unwrap().name, "Node");
    assert_eq!(
        mc.connection_info().unwrap(),
        format!("127.0.0.1:{}", device.port)
    );
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    assert!(matches!(mc.get_time().await, Err(Error::Device(_))));
    mc.disconnect().await.unwrap();
    assert!(!mc.is_connected());
    assert_eq!(device.commands()[0], APP_START);
    assert_eq!(device.commands().len(), 3);
}

#[tokio::test]
async fn tcp_client_made_first_and_connected_later() {
    let device = TcpDevice::start(answer).await;
    let mc = MeshCore::tcp("127.0.0.1", device.port);
    assert!(!mc.is_connected());
    let info = timeout(PATIENCE, mc.connect())
        .await
        .expect("connect took too long")
        .unwrap();
    assert_eq!(info.name, "Node");
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
}

#[tokio::test]
async fn tcp_frames_arrive_split_joined_and_behind_console_text() {
    let device = TcpDevice::start(|command| match command.first() {
        // console text, then the frame one byte per write
        Some(20) => {
            let mut bytes = b"radio: ok\r\n".to_vec();
            bytes.extend(framed(&[12, 0xd4, 0x0d]));
            Reply::Write(bytes.into_iter().map(|b| vec![b]).collect())
        }
        // a push and the answer in one write
        Some(22) => Reply::Write(vec![
            [framed(&[0x83]), framed(&device_info_frame(1))].concat(),
        ]),
        _ => answer(command),
    })
    .await;
    let mc = device.connect(Options::default()).await;
    let mut events = mc.subscribe();
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    assert_eq!(
        mc.send_device_query().await.unwrap().model.as_deref(),
        Some("Heltec")
    );
    let waiting = timeout(PATIENCE, async {
        loop {
            if let Event::MessagesWaiting = events.recv().await.unwrap() {
                return;
            }
        }
    });
    assert!(waiting.await.is_ok(), "the push in the same write got lost");
}

#[tokio::test]
async fn tcp_disconnect_by_the_device_is_reported() {
    let device = TcpDevice::start(|command| match command.first() {
        Some(5) => Reply::Close,
        _ => answer(command),
    })
    .await;
    let mc = device.connect(Options::default()).await;
    let mut events = mc.subscribe();
    assert!(matches!(mc.get_time().await, Err(Error::ConnectionLost)));
    assert_eq!(disconnected(&mut events).await, "tcp_disconnect");
    assert!(!mc.is_connected());
    assert!(matches!(mc.get_bat().await, Err(Error::NotConnected)));
}

#[tokio::test]
async fn tcp_reconnects_and_restarts_the_session() {
    let device = TcpDevice::start(|command| match command.first() {
        Some(5) => Reply::Close,
        _ => answer(command),
    })
    .await;
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let mc = device.connect(options).await;
    let mut events = mc.subscribe();
    assert!(mc.get_time().await.is_err());
    let reconnected = timeout(PATIENCE, async {
        loop {
            if let Event::Connected { reconnected, .. } = events.recv().await.unwrap() {
                return reconnected;
            }
        }
    })
    .await
    .expect("no reconnect");
    assert!(reconnected);
    assert_eq!(device.connections.load(Ordering::SeqCst), 2);
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    let app_starts = device
        .commands()
        .iter()
        .filter(|c| c.as_slice() == APP_START)
        .count();
    assert_eq!(app_starts, 2);
}

#[tokio::test]
async fn tcp_link_without_answers_is_dropped_after_five_frames() {
    // answers the session start, then nothing
    let device = TcpDevice::start(|command| match command.first() {
        Some(1) => answer(command),
        _ => Reply::Write(vec![]),
    })
    .await;
    let mc = device.connect(quick_options()).await;
    let mut events = mc.subscribe();
    // APP_START was answered; four more frames go out unanswered
    for _ in 0..4 {
        assert!(matches!(mc.get_bat().await, Err(Error::Timeout)));
    }
    // the fifth unanswered frame is not sent, the link is taken for dead
    assert!(matches!(mc.get_bat().await, Err(Error::ConnectionLost)));
    assert_eq!(disconnected(&mut events).await, "tcp_no_response");
    assert!(!mc.is_connected());
    assert_eq!(device.commands().len(), 5);
}

#[tokio::test]
async fn tcp_factory_reset_that_is_not_sent_is_not_done() {
    let device = TcpDevice::start(|command| match command.first() {
        Some(1) => answer(command),
        _ => Reply::Write(vec![]),
    })
    .await;
    let mc = device.connect(quick_options()).await;
    for _ in 0..4 {
        assert!(matches!(mc.get_bat().await, Err(Error::Timeout)));
    }
    // a missing reply counts as done, but this command is never sent
    let token = mc.request_factory_reset();
    assert!(matches!(
        mc.confirm_factory_reset(&token).await,
        Err(Error::ConnectionLost)
    ));
    assert_eq!(device.commands().len(), 5);
}

#[tokio::test]
async fn tcp_pushes_count_as_answers() {
    // the device never answers commands but keeps sending pushes
    let device = TcpDevice::start(|command| match command.first() {
        Some(1) => answer(command),
        _ => Reply::Write(vec![framed(&[0x83])]),
    })
    .await;
    let mc = device.connect(quick_options()).await;
    for _ in 0..6 {
        assert!(matches!(mc.get_bat().await, Err(Error::Timeout)));
    }
    assert!(mc.is_connected());
}

#[tokio::test]
async fn tcp_frames_too_long_to_send_do_not_count() {
    let device = TcpDevice::start(|_| Reply::Write(vec![])).await;
    let transport = TcpTransport::new("127.0.0.1", device.port);
    let mut link = transport.open().await.unwrap();
    let too_long = vec![0; usize::from(u16::MAX) + 1];
    for _ in 0..5 {
        assert!(matches!(
            link.writer.write_frame(&too_long).await,
            Err(Error::InvalidArgument(_))
        ));
    }
    // nothing went out, so the link is not taken for dead
    link.writer.write_frame(&[20]).await.unwrap();
}

#[tokio::test]
async fn tcp_refused_connection_fails_at_once() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let result = timeout(
        PATIENCE,
        MeshCore::create_tcp("127.0.0.1", port, Options::default()),
    )
    .await
    .expect("a refused connection must not wait");
    let Err(err) = result else {
        panic!("nothing listens there");
    };
    assert!(
        matches!(&err, Error::Io(io) if io.kind() == std::io::ErrorKind::ConnectionRefused),
        "{err:?}"
    );
}

// ---------------------------------------------------------------- serial

#[cfg(unix)]
mod serial {
    use super::*;
    use meshcore::{SelfInfo, SerialConfig, SerialTransport};
    use tokio_serial::{SerialPort, SerialStream};

    /// A device on the master side of a pseudo terminal. The client opens the
    /// slave by its path, as it would a USB serial port.
    struct PtyDevice {
        path: String,
        commands: Arc<Mutex<Vec<Vec<u8>>>>,
        // Holds the slave open, so the master never sees a closed terminal
        // before the client opens it.
        _slave: SerialStream,
    }

    impl PtyDevice {
        /// Starts the device; `boot` is written to the terminal first, as
        /// stale output a board leaves before the client connects.
        async fn start(
            boot: &[u8],
            respond: impl Fn(&[u8]) -> Reply + Send + Sync + 'static,
        ) -> Self {
            let (mut master, slave) = SerialStream::pair().expect("no pseudo terminal");
            let path = slave.name().expect("pseudo terminal without a path");
            master.write_all(boot).await.unwrap();
            let commands = Arc::<Mutex<Vec<Vec<u8>>>>::default();
            tokio::spawn(run_device(master, Arc::clone(&commands), Arc::new(respond)));
            Self {
                path,
                commands,
                _slave: slave,
            }
        }

        fn config(&self) -> SerialConfig {
            SerialConfig::new(self.path.clone())
                .settle_delay(Duration::ZERO)
                .drain_duration(Duration::ZERO)
        }

        async fn connect(&self, config: SerialConfig) -> MeshCore {
            timeout(
                PATIENCE,
                MeshCore::create_serial(config, Options::default()),
            )
            .await
            .expect("connect took too long")
            .expect("connect failed")
        }
    }

    #[tokio::test]
    async fn serial_session_end_to_end() {
        let device = PtyDevice::start(&[], answer).await;
        let mc = device.connect(device.config()).await;
        assert_eq!(mc.self_info().unwrap().name, "Node");
        assert_eq!(mc.connection_info().unwrap(), device.path);
        assert_eq!(mc.get_bat().await.unwrap().level, 3540);
        mc.disconnect().await.unwrap();
        assert!(!mc.is_connected());
        let commands = device.commands.lock().unwrap().clone();
        assert_eq!(commands, [APP_START.to_vec(), vec![20]]);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn serial_port_is_closed_when_disconnect_returns() {
        let device = PtyDevice::start(&[], answer).await;
        let mc = device.connect(device.config()).await;
        // the device's end of the terminal stays open
        assert_eq!(open_files(&device.path), 2);
        mc.disconnect().await.unwrap();
        assert_eq!(open_files(&device.path), 1);
        // also when it comes in while a connect opens the port
        let (mc, connecting) = opening_the_port(&device).await;
        assert_eq!(open_files(&device.path), 2);
        mc.disconnect().await.unwrap();
        assert_eq!(open_files(&device.path), 1);
        assert!(matches!(
            connecting.await.unwrap(),
            Err(Error::NotConnected)
        ));
    }

    /// A client whose `connect` has the port open and waits for it to settle.
    async fn opening_the_port(
        device: &PtyDevice,
    ) -> (
        MeshCore,
        tokio::task::JoinHandle<meshcore::Result<SelfInfo>>,
    ) {
        let config = device.config().settle_delay(Duration::from_secs(1));
        let mc = MeshCore::new(SerialTransport::new(config), Options::default());
        let connecting = tokio::spawn({
            let mc = mc.clone();
            async move { mc.connect().await }
        });
        #[cfg(target_os = "linux")]
        timeout(PATIENCE, async {
            while open_files(&device.path) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the port was not opened");
        #[cfg(not(target_os = "linux"))]
        tokio::time::sleep(Duration::from_millis(200)).await;
        (mc, connecting)
    }

    #[tokio::test]
    async fn serial_connect_takes_over_from_one_that_is_opening_the_port() {
        let device = PtyDevice::start(&[], answer).await;
        let (mc, first) = opening_the_port(&device).await;
        let info = timeout(PATIENCE, mc.connect())
            .await
            .expect("connect took too long")
            .unwrap();
        assert_eq!(info.name, "Node");
        assert!(matches!(first.await.unwrap(), Err(Error::NotConnected)));
        assert!(mc.is_connected());
    }

    /// How often this process has `path` open.
    #[cfg(target_os = "linux")]
    fn open_files(path: &str) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
            .filter(|target| target.as_path() == std::path::Path::new(path))
            .count()
    }

    #[tokio::test]
    async fn serial_connect_replaces_a_link_that_is_up() {
        // the port takes one program at a time, so it has to be free again
        let device = PtyDevice::start(&[], answer).await;
        let mc = device.connect(device.config()).await;
        let info = timeout(PATIENCE, mc.connect())
            .await
            .expect("connect took too long")
            .unwrap();
        assert_eq!(info.name, "Node");
        assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    }

    #[tokio::test]
    async fn serial_client_made_first_and_connected_later() {
        let device = PtyDevice::start(&[], answer).await;
        // the default settings: 115200 baud, settle and drain delays
        let mc = MeshCore::serial(device.path.clone());
        let info = timeout(PATIENCE, mc.connect())
            .await
            .expect("connect took too long")
            .unwrap();
        assert_eq!(info.name, "Node");
        assert_eq!(mc.connection_info().unwrap(), device.path);
    }

    #[tokio::test]
    async fn serial_drops_what_the_board_sent_before_connecting() {
        // A stale answer from an earlier session must not pass for this one.
        let mut boot = b"ESP-ROM:esp32s3-20210327\r\n".to_vec();
        boot.extend(framed(&self_info_frame("Stale")));
        let device = PtyDevice::start(&boot, answer).await;
        let config = device.config().drain_duration(Duration::from_millis(200));
        let mc = device.connect(config).await;
        assert_eq!(mc.self_info().unwrap().name, "Node");
    }

    #[tokio::test]
    async fn serial_finds_frames_behind_console_text() {
        let device = PtyDevice::start(&[], |command| match command.first() {
            Some(20) => {
                let mut bytes = b"[radio] tx done\r\n".to_vec();
                bytes.extend(framed(&[12, 0xd4, 0x0d]));
                Reply::Write(bytes.chunks(3).map(<[u8]>::to_vec).collect())
            }
            _ => answer(command),
        })
        .await;
        let mc = device.connect(device.config()).await;
        assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    }

    #[tokio::test]
    async fn serial_link_loss_is_reported() {
        let device = PtyDevice::start(&[], |command| match command.first() {
            Some(5) => Reply::Close,
            _ => answer(command),
        })
        .await;
        let mc = device.connect(device.config()).await;
        let mut events = mc.subscribe();
        assert!(matches!(mc.get_time().await, Err(Error::ConnectionLost)));
        assert_eq!(disconnected(&mut events).await, "serial_disconnect");
        assert!(!mc.is_connected());
    }

    #[tokio::test]
    async fn serial_missing_port_fails_at_once_without_retries() {
        let config = SerialConfig::new("/dev/meshcore-test-no-such-port");
        // The DTR retries wait a second each; an open error must skip them.
        let result = timeout(
            Duration::from_millis(900),
            MeshCore::create_serial(config, Options::default()),
        )
        .await
        .expect("an open error must not be retried");
        let Err(err) = result else {
            panic!("the port does not exist");
        };
        assert!(matches!(err, Error::Serial(_)), "{err:?}");
    }
}
