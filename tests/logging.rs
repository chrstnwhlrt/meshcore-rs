//! Where meshcore_py logs the same case at info level or above, the library
//! logs meshcore_py's line, so that programs on both show the same.

mod common;

use std::future::Future;
use std::sync::Mutex;

use common::{KEY, client, err, ok, sent_frame};
use meshcore::{Options, PublicKey, RetryOptions};

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

// One test: the logger is global to this test binary.
#[tokio::test(start_paused = true)]
async fn lines_read_as_meshcore_py() {
    log::set_logger(&CAPTURE).unwrap();
    log::set_max_level(log::LevelFilter::Info);
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        // "hi" is sent but never acknowledged, "full" fails, "silent" gets
        // no answer at all
        2 if cmd.ends_with(b"hi") => vec![sent_frame(false, 0x200, 500)],
        2 if cmd.ends_with(b"silent") => vec![],
        2 => vec![err(3)],
        13 => vec![ok()],
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
