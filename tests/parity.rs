//! Parity with meshcore_py: the fixture frames (`tests/fixtures/frames.hex`,
//! one of every packet type) must give the events meshcore_py gives for them
//! (`tests/fixtures/meshcore_py.jsonl`, recorded with `meshcore_py_reader.py`),
//! serialized with the `serde` feature: same event types, keys, key order and
//! values.

#![cfg(feature = "serde")]

mod common;

use meshcore::{Event, Options};

/// Where this library deliberately differs from meshcore_py, as
/// (meshcore_py's text, this library's text).
const DEVIATIONS: &[(&str, &str)] = &[
    // The firmware sends the TX power as a signed byte (it takes -9 dBm and up);
    // meshcore_py reads it unsigned.
    (r#""tx_power":247"#, r#""tx_power":-9"#),
];

#[test]
fn connection_events_serialize_like_meshcore_py() {
    let json = |event: Event| serde_json::to_string(&event).unwrap();
    let connected = |reconnected| Event::Connected {
        info: "/dev/ttyUSB0".into(),
        reconnected,
    };
    let disconnected = |reason: &str| Event::Disconnected {
        reason: reason.into(),
    };
    // connection_manager.py: connect(), _attempt_reconnect(), disconnect(),
    // handle_disconnect()
    assert_eq!(
        json(connected(false)),
        r#"{"connection_info":"/dev/ttyUSB0"}"#
    );
    assert_eq!(
        json(connected(true)),
        r#"{"connection_info":"/dev/ttyUSB0","reconnected":true}"#
    );
    assert_eq!(
        json(disconnected("manual_disconnect")),
        r#"{"reason":"manual_disconnect"}"#
    );
    assert_eq!(
        json(disconnected("reconnect_failed")),
        r#"{"reason":"reconnect_failed","max_attempts_exceeded":true}"#
    );
    assert_eq!(
        json(disconnected("tcp_disconnect")),
        r#"{"reason":"tcp_disconnect","reconnect_failed":false}"#
    );
}

/// Removes the receive time, which is the time of the run.
fn without_recv_time(line: &str) -> String {
    let Some(start) = line.find(r#""recv_time":"#) else {
        return line.to_string();
    };
    let end = line[start..]
        .find(',')
        .map_or(line.len(), |i| start + i + 1);
    format!("{}{}", &line[..start], &line[end..])
}

#[tokio::test(start_paused = true)]
async fn events_serialize_like_meshcore_py() {
    let frames = include_str!("fixtures/frames.hex");
    let expected: Vec<String> = include_str!("fixtures/meshcore_py.jsonl")
        .lines()
        .map(|line| {
            DEVIATIONS
                .iter()
                .fold(line.to_string(), |acc, (py, rs)| acc.replace(py, rs))
        })
        .collect();

    let (mc, device) = common::client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    let mut actual = Vec::new();
    for line in frames.lines() {
        device.push(meshcore::hex::decode(line).unwrap()).await;
        device.push(vec![0x7f]).await; // marks the end of the frame's events
        let mut produced = false;
        loop {
            let event = events.recv().await.unwrap();
            match event {
                Event::Unknown { code: 0x7f, .. } => break,
                // meshcore_py dispatches nothing for CONTACT_START
                Event::ContactsStart { .. } => {}
                event => {
                    let kind = serde_json::to_string(event.kind().as_str()).unwrap();
                    let payload = serde_json::to_string(&event).unwrap();
                    actual.push(without_recv_time(&format!(
                        r#"{{"type":{kind},"payload":{payload}}}"#
                    )));
                    produced = true;
                }
            }
        }
        if !produced {
            actual.push(r#"{"type":null}"#.to_string());
        }
    }

    assert_eq!(actual.len(), expected.len(), "number of events");
    let mismatches: Vec<String> = actual
        .iter()
        .zip(&expected)
        .enumerate()
        .filter(|(_, (a, e))| a != e)
        .map(|(i, (a, e))| format!("event {i}\n  meshcore_py: {e}\n  meshcore-rs: {a}"))
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
