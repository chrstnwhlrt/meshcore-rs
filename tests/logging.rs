//! Where meshcore_py logs the same case at info level or above, the library
//! logs meshcore_py's line, so that programs on both show the same. Some
//! cases meshcore_py does not log at that level, such as a lost link, get
//! lines of the library's own, and a command without a link and input refused
//! before sending log nothing.

mod common;

use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{Device, KEY, binary_response, client, contact_frame, err, ok, sent_frame};
use meshcore::protocol::encode_reply_path;
use meshcore::{
    AnonReqType, Error, ErrorCode, Event, MeshCore, NeighboursQuery, Options, PathLen, PublicKey,
    RequestOptions, RetryOptions, TcpTransport, Transport,
};
#[cfg(unix)]
use meshcore::{SerialConfig, SerialTransport};
use tokio::io::AsyncReadExt;

/// The lines logged at info level and above, as `LEVEL:message`.
static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

struct Capture;

impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "meshcore" && metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            LINES
                .lock()
                .unwrap()
                .push(format!("{}:{}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

static CAPTURE: Capture = Capture;

/// The lines logged while `work` runs.
async fn lines_of(work: impl Future<Output = ()>) -> Vec<String> {
    LINES.lock().unwrap().clear();
    work.await;
    std::mem::take(&mut *LINES.lock().unwrap())
}

/// A min/max/average answer: a temperature series, then one of a type
/// meshcore_py does not know (200).
fn mma_answer() -> Vec<u8> {
    let mut answer = 1_700_000_000u32.to_le_bytes().to_vec();
    answer.extend([1, 103, 0, 200, 0, 255, 0, 221, 2, 200, 1, 2]);
    answer
}

/// The line meshcore_py logs for the unknown type; the series before it stays.
async fn unknown_mma_type(mc: &MeshCore, key: &PublicKey) {
    let lines = lines_of(async {
        let series = mc
            .req_mma_sync(key, 3600, 0, RequestOptions::default())
            .await
            .unwrap();
        assert_eq!(series.len(), 1);
    })
    .await;
    assert_eq!(lines, ["ERROR:Unknown LPP type: 200"]);
}

/// The other series meshcore_py cannot read: one without its type byte and
/// one cut short.
async fn unreadable_mma_series() {
    let lines = lines_of(async {
        let temperature = [1, 103, 0, 200, 0, 255, 0, 221];
        let (series, rest) = meshcore::lpp::parse_mma(&[&temperature[..], &[2]].concat());
        assert_eq!((series.len(), rest), (1, vec![2]));
        let (series, rest) = meshcore::lpp::parse_mma(&temperature[..6]);
        assert_eq!((series.len(), rest.len()), (0, 6));
    })
    .await;
    assert_eq!(
        lines,
        [
            "ERROR:Error parsing binary MMA response: index out of range",
            "ERROR:Error parsing binary MMA response: Invalid buffer length!"
        ]
    );
}

/// The plain request variants hint at the synchronous ones, which log nothing.
async fn hints_at_sync_variants(mc: &MeshCore, key: &PublicKey) {
    let lines = lines_of(async {
        mc.send_login_sync(key, "pw", RequestOptions::default())
            .await
            .unwrap();
        mc.send_login(key, "pw").await.unwrap();
        mc.send_statusreq(key).await.unwrap();
        mc.send_telemetry_req(key).await.unwrap();
        mc.send_path_discovery(key).await.unwrap();
    })
    .await;
    assert_eq!(
        lines,
        [
            "WARN:*** please consider using send_login_sync instead of send_login",
            "WARN:*** please consider using req_status_sync instead of send_statusreq",
            "WARN:*** please consider using req_telemetry_sync instead of send_telemetry_req",
            "WARN:*** please consider using send_path_discovery_sync instead of send_path_discovery"
        ]
    );
}

/// Frames the reader cannot read: an empty one, stats and the private key
/// have lines of their own, the others share meshcore_py's line for an
/// exception (here a self info cut short, where meshcore_py runs out of
/// bytes).
async fn unreadable_replies(device: &Device) {
    let lines = lines_of(async {
        for frame in [
            vec![],
            vec![24],
            vec![24, 0, 1],
            vec![24, 1, 0],
            vec![24, 2, 0],
            vec![24, 9],
            vec![14, 1, 2],
            vec![5, 1],
        ] {
            device.push(frame).await;
        }
        device.settle().await;
    })
    .await;
    assert_eq!(
        lines,
        [
            "WARN:Received empty packet: index out of range",
            "ERROR:Stats response too short: 1 bytes, need at least 2 for header",
            "ERROR:Stats core response too short: 3 bytes, expected 11",
            "ERROR:Stats radio response too short: 3 bytes, expected 14",
            "ERROR:Stats packets response too short: 3 bytes, expected 26 or 30",
            "ERROR:Unknown stats type: 9, data: 1809",
            "ERROR:Invalid private key response length: 3",
            "ERROR:handle_rx parse error: packet type 0x05, 2 bytes: protocol error: \
             invalid_frame_length"
        ]
    );
}

/// A command without a link fails at once and logs nothing; meshcore_py logs
/// that the transport is not connected.
async fn command_without_a_link() {
    let (idle, _device) = client(Options::default(), |_| vec![]);
    let lines = lines_of(async {
        assert!(matches!(
            idle.get_time().await,
            Err(meshcore::Error::NotConnected)
        ));
    })
    .await;
    assert_eq!(lines, Vec::<String>::new());
}

/// Input refused before sending is an error and logs nothing. meshcore_py's
/// `send_trace` logs "Invalid path format: ..." for a path it cannot read,
/// and its `send_msg_with_retry` sends a text the firmware does not take and
/// logs "Failed to send message: ..." for each attempt.
async fn refused_input(mc: &MeshCore, key: &PublicKey) {
    let lines = lines_of(async {
        for path in ["abc", "23,zz", "2301,5f"] {
            let parsed = meshcore::parse_trace_path(path);
            assert!(matches!(parsed, Err(Error::InvalidArgument(_))), "{path}");
        }
        let long = "x".repeat(161);
        let refused = mc.send_msg_with_retry(key, &long, RetryOptions::default());
        assert!(matches!(refused.await, Err(Error::InvalidArgument(_))));
    })
    .await;
    assert_eq!(lines, Vec::<String>::new());
}

/// Reply paths a server could not use or hold, as a caller may hand them to
/// `encode_reply_path`.
async fn unusable_reply_paths() {
    let lines = lines_of(async {
        let reserved = encode_reply_path(PathLen::from_byte(0xc1), &[1, 2, 3, 4]);
        assert_eq!(reserved, [0]);
        // 35 hops of 2 bytes
        let long = encode_reply_path(PathLen::from_byte(0x40 | 0x23), &[7; 70]);
        assert_eq!(long.len(), 1 + 64);
    })
    .await;
    assert_eq!(
        lines,
        [
            "WARN:Unsupported out_path_hash_mode 3; requesting a zero-hop reply path instead",
            "WARN:Reply path of 35 hops x 2B exceeds the 64B the server can hold; truncating to \
             32"
        ]
    );
}

/// A lost link gets a line of the library's own; meshcore_py logs it below
/// info level.
async fn lost_link() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let lines = lines_of(async {
        device.drop_link().await;
        device.settle().await;
    })
    .await;
    assert!(!mc.is_connected());
    assert_eq!(lines, ["INFO:connection lost: test_disconnect"]);
}

/// A subscriber that falls more than the event buffer (1024 by default)
/// behind loses the oldest events, with a warning of the library's own.
async fn lagging_subscriber(mc: &MeshCore, device: &Device) {
    let mut events = mc.subscribe();
    let lines = lines_of(async {
        for _ in 0..1100 {
            device.push(vec![0x83]).await;
        }
        device.settle().await;
        assert!(matches!(events.recv().await, Some(Event::MessagesWaiting)));
    })
    .await;
    assert_eq!(lines, ["WARN:event subscriber fell behind, 76 events lost"]);
}

/// Anonymous requests: the contact cannot be read, its path cannot be set to
/// zero-hop, and it goes out zero-hop.
async fn anonymous_requests() {
    let reads = AtomicU32::new(0);
    let writes = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        30 if reads.fetch_add(1, Ordering::SeqCst) == 0 => vec![err(4)],
        30 => vec![contact_frame(3, KEY, "Rep", 0xff, &[])],
        9 if writes.fetch_add(1, Ordering::SeqCst) == 0 => vec![err(3)],
        9 | 13 => vec![ok()],
        57 => vec![sent_frame(false, 0x57, 1000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let key = PublicKey::new(KEY);
    let send = || mc.send_anon_req(&key, AnonReqType::Owner, RequestOptions::default());
    let lines = lines_of(async {
        let unread = send().await;
        assert!(matches!(unread, Err(Error::Device(ErrorCode::BadState))));
        let unset = send().await;
        assert!(matches!(unset, Err(Error::Device(ErrorCode::TableFull))));
        send().await.unwrap();
    })
    .await;
    assert_eq!(
        lines,
        [
            "ERROR:Could not read the contact from the device, aborting anon request",
            "INFO:No path set trying zero hop",
            "ERROR:Could not set zero-hop path, aborting anon request",
            "INFO:No path set trying zero hop"
        ]
    );
}

/// A chunk to sign the device refuses ends the signing.
async fn refused_sign_chunk() {
    let chunks = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        33 => vec![[&[19u8, 0][..], &8192u32.to_le_bytes()].concat()],
        34 if chunks.fetch_add(1, Ordering::SeqCst) == 0 => vec![ok()],
        34 => vec![err(4)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let lines = lines_of(async {
        let signed = mc.sign(&[1; 300], 120, None).await;
        assert!(matches!(signed, Err(Error::Device(ErrorCode::BadState))));
    })
    .await;
    assert_eq!(
        lines,
        ["ERROR:Error sending chunk 2/3: {'error_code': 4, 'code_string': 'ERR_CODE_BAD_STATE'}"]
    );
}

/// Retries that cannot reset the path: to a key prefix no contact has, and
/// with a reset the device refuses.
async fn retries_without_the_reset() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x300, 500)],
        4 => vec![vec![2, 0, 0, 0, 0], vec![4, 0, 0, 0, 0]],
        13 => vec![err(2)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let prefix: [u8; 6] = KEY[..6].try_into().unwrap();
    let lines = lines_of(async {
        let destinations: [meshcore::Destination; 2] = [prefix.into(), PublicKey::new(KEY).into()];
        for dst in destinations {
            let acked = mc.send_msg_with_retry(dst, "hi", RetryOptions::default());
            assert_eq!(acked.await.unwrap(), None);
        }
    })
    .await;
    assert_eq!(
        lines,
        [
            "INFO:Retry sending msg: 2",
            "INFO:Don't have full key, retreiving contacts so we can reset path",
            "ERROR:no full key to reset the path, continuing",
            "INFO:Retry sending msg: 3",
            "INFO:Retry sending msg: 2",
            "INFO:Resetting path",
            "ERROR:Couldn't reset path Event(type=<EventType.ERROR: 'command_error'>, \
             payload={'error_code': 2, 'code_string': 'ERR_CODE_NOT_FOUND'}, attributes={}) \
             continuing ...",
            "INFO:Retry sending msg: 3"
        ]
    );
}

/// A neighbours answer cut short.
async fn unreadable_neighbours() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        50 => {
            // two announced, one there
            let mut answer = 2i16.to_le_bytes().to_vec();
            answer.extend(2i16.to_le_bytes());
            answer.extend([0xa1; 4]);
            answer.extend(30i32.to_le_bytes());
            answer.push(8);
            vec![
                sent_frame(false, 0x600, 1000),
                binary_response(0x600, &answer),
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let key = PublicKey::new(KEY);
    let lines = lines_of(async {
        let page =
            mc.req_neighbours_sync(&key, NeighboursQuery::default(), RequestOptions::default());
        assert!(matches!(page.await, Err(Error::Timeout)));
    })
    .await;
    assert_eq!(
        lines,
        ["ERROR:Error parsing binary NEIGHBOURS response: protocol error: invalid_frame_length"]
    );
}

/// A status answer cut short, which meshcore_py drops without a line, and a
/// subscription answer without its expiry, in which it reads zeros: lines of
/// the form meshcore_py logs for the other answers.
async fn unreadable_status_and_subscription() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        50 => {
            let tag = 0x600 + u32::from(cmd[33]);
            let answer = if cmd[33] == 0x01 {
                vec![1; 40]
            } else {
                vec![1]
            };
            vec![sent_frame(false, tag, 1000), binary_response(tag, &answer)]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let key = PublicKey::new(KEY);
    let lines = lines_of(async {
        let options = RequestOptions::default();
        let status = mc.req_status_sync(&key, options).await;
        assert!(matches!(status, Err(Error::Timeout)));
        let subscription = mc.req_subscribe_sync(&key, [1, 2, 3, 4], 60, &[1, 103, 10], options);
        assert!(matches!(subscription.await, Err(Error::Protocol(_))));
    })
    .await;
    assert_eq!(
        lines,
        [
            "ERROR:Error parsing binary STATUS response: protocol error: invalid_frame_length",
            "ERROR:Error parsing binary SUBSCRIBE response: protocol error: invalid_frame_length"
        ]
    );
}

/// A chunk to sign that gets no answer counts as received after 5 s.
async fn unanswered_sign_chunk(mc: &MeshCore) {
    let lines = lines_of(async {
        let started = tokio::time::Instant::now();
        mc.sign_data(&[1; 10]).await.unwrap();
        assert_eq!(started.elapsed().as_secs(), 5);
    })
    .await;
    assert_eq!(
        lines,
        [
            "WARN:sign_data OK response not received (timeout), but continuing - data may have \
             been processed by device"
        ]
    );
}

/// A node on TCP that takes the connection but does not answer `APP_START`.
/// On real time, for the kernel to move the bytes.
async fn silent_tcp_node() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let node = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 64];
        while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
    });
    let lines = lines_of(async {
        assert!(
            MeshCore::create_tcp("127.0.0.1", port, Options::default())
                .await
                .is_err()
        );
    })
    .await;
    assert_eq!(
        lines,
        [
            "INFO:TCP Connection started",
            "ERROR:No response from meshcore node, disconnecting"
        ]
    );
    node.abort();
}

/// A TCP write after the peer closed the connection. On real time.
async fn failed_tcp_write() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move { drop(listener.accept().await.unwrap()) });
    let mut link = TcpTransport::new("127.0.0.1", port).open().await.unwrap();
    peer.await.unwrap();
    let lines = lines_of(async {
        // the first write may still go out, the peer's reset ends the next
        for _ in 0..20 {
            if link.writer.write_frame(&[22, 3]).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the writes went on");
    })
    .await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with("WARN:TCP write failed: "), "{lines:?}");
}

/// Serial links on a pseudo terminal: a board that does not answer, which
/// `create_serial` tries again with DTR set (a terminal has none), a write
/// nobody reads and one whose other end is gone.
#[cfg(unix)]
async fn serial_links() {
    use tokio_serial::{SerialPort, SerialStream};
    let quick = |path: String| {
        SerialConfig::new(path)
            .settle_delay(Duration::ZERO)
            .drain_duration(Duration::ZERO)
    };

    let (_board, terminal) = SerialStream::pair().unwrap();
    let path = terminal.name().unwrap();
    let lines = lines_of(async {
        let created = MeshCore::create_serial(quick(path.clone()), Options::default()).await;
        assert!(matches!(created, Err(Error::Serial(_))));
    })
    .await;
    assert_eq!(
        lines,
        [
            "INFO:Serial Connection started",
            "INFO:No response from meshcore node, trying to force DTR = True"
        ]
    );

    let mut link = SerialTransport::new(quick(path)).open().await.unwrap();
    let lines = lines_of(async { while link.writer.write_frame(&[0; 170]).await.is_ok() {} }).await;
    assert_eq!(lines, ["WARN:Serial write timed out after 10.0s"]);

    let (board, terminal) = SerialStream::pair().unwrap();
    let mut link = SerialTransport::new(quick(terminal.name().unwrap()))
        .open()
        .await
        .unwrap();
    drop(board);
    let lines = lines_of(async {
        assert!(link.writer.write_frame(&[22, 3]).await.is_err());
    })
    .await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("WARN:Serial write failed: "),
        "{lines:?}"
    );
}

/// Messages retried, sent without an ACK or refused, and a command to a node
/// of unknown type, on a device that sends "hi" but never acknowledges it,
/// refuses "full" and does not answer "silent".
async fn messages_and_commands(mc: &MeshCore, key: &PublicKey) {
    let once = RetryOptions {
        max_attempts: 1,
        ..RetryOptions::default()
    };

    let lines = lines_of(async {
        let sent = mc
            .send_msg_with_retry(key, "hi", RetryOptions::default())
            .await
            .unwrap();
        assert_eq!(sent, None);
    })
    .await;
    assert_eq!(
        lines,
        [
            "INFO:Retry sending msg: 2",
            "INFO:Resetting path",
            "INFO:Retry sending msg: 3"
        ]
    );

    let lines = lines_of(async {
        assert_eq!(
            mc.send_msg_with_retry(key, "full", once).await.unwrap(),
            None
        );
        assert_eq!(
            mc.send_msg_with_retry(key, "silent", once).await.unwrap(),
            None
        );
    })
    .await;
    assert_eq!(
        lines,
        [
            "ERROR:Failed to send message: {'error_code': 3, 'code_string': 'ERR_CODE_TABLE_FULL'}",
            "ERROR:Failed to send message: {'reason': 'no_event_received'}"
        ]
    );

    let lines = lines_of(async {
        assert!(mc.send_cmd(key, "ver", None, None).await.is_err());
    })
    .await;
    assert_eq!(
        lines,
        [
            "WARN:Can't determine destination type, please ensure contacts first or specify \
             dst_type when calling `send_cmd`. Assuming it's a repeater."
        ]
    );
}

// One test: the logger is global to this test binary.
#[tokio::test]
async fn lines_read_as_meshcore_py() {
    log::set_logger(&CAPTURE).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    silent_tcp_node().await;
    failed_tcp_write().await;
    tokio::time::pause();
    #[cfg(unix)]
    serial_links().await;
    command_without_a_link().await;
    lost_link().await;
    anonymous_requests().await;
    refused_sign_chunk().await;
    retries_without_the_reset().await;
    unreadable_neighbours().await;
    unreadable_status_and_subscription().await;
    unusable_reply_paths().await;
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        // "hi" is sent but never acknowledged, "full" fails, "silent" gets
        // no answer at all
        2 if cmd.ends_with(b"hi") => vec![sent_frame(false, 0x200, 500)],
        2 if cmd.ends_with(b"silent") => vec![],
        2 => vec![err(3)],
        13 => vec![ok()],
        50 => vec![
            sent_frame(false, 0x500, 1000),
            binary_response(0x500, &mma_answer()),
        ],
        26 => vec![
            sent_frame(false, 0x26, 1000),
            [&[0x85u8, 1][..], &KEY[..6]].concat(),
        ],
        27 | 39 | 52 => vec![sent_frame(false, 0x27, 1000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let key = PublicKey::new(KEY);
    messages_and_commands(&mc, &key).await;
    refused_input(&mc, &key).await;
    unknown_mma_type(&mc, &key).await;
    unreadable_mma_series().await;
    hints_at_sync_variants(&mc, &key).await;
    unanswered_sign_chunk(&mc).await;
    unreadable_replies(&device).await;
    lagging_subscriber(&mc, &device).await;

    let mut token = String::new();
    let lines = lines_of(async {
        token = mc.request_factory_reset();
        mc.confirm_factory_reset(&token).await.unwrap();
    })
    .await;
    assert_eq!(
        lines,
        [
            format!(
                "WARN:Factory reset requested. Call confirm_factory_reset('{token}') to proceed. \
                 This will ERASE ALL DATA on the device."
            ),
            "WARN:Executing factory reset — all device data will be erased".to_string()
        ]
    );
}
