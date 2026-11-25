//! Where meshcore_py logs the same case at info level or above, the library
//! logs meshcore_py's line, so that programs on both show the same.

mod common;

use std::future::Future;
use std::sync::Mutex;

use common::{Device, KEY, binary_response, client, err, ok, sent_frame};
use meshcore::{MeshCore, Options, PublicKey, RequestOptions, RetryOptions};

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
            "ERROR:Unknown stats type: 9, data: 1809",
            "ERROR:Invalid private key response length: 3",
            "ERROR:handle_rx parse error: packet type 0x05, 2 bytes: protocol error: \
             invalid_frame_length"
        ]
    );
}

// One test: the logger is global to this test binary.
#[tokio::test(start_paused = true)]
async fn lines_read_as_meshcore_py() {
    log::set_logger(&CAPTURE).unwrap();
    log::set_max_level(log::LevelFilter::Info);
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
    let once = RetryOptions {
        max_attempts: 1,
        ..RetryOptions::default()
    };

    let lines = lines_of(async {
        let sent = mc
            .send_msg_with_retry(&key, "hi", RetryOptions::default())
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
            mc.send_msg_with_retry(&key, "full", once).await.unwrap(),
            None
        );
        assert_eq!(
            mc.send_msg_with_retry(&key, "silent", once).await.unwrap(),
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

    unknown_mma_type(&mc, &key).await;
    hints_at_sync_variants(&mc, &key).await;
    unreadable_replies(&device).await;

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
