//! Parity with meshcore_py: the fixture frames (`tests/fixtures/frames.hex`,
//! frames of every packet type) must give the events meshcore_py gives for them
//! (`tests/fixtures/meshcore_py.jsonl`, recorded with `meshcore_py_reader.py`),
//! serialized with the `serde` feature: same event types, keys, key order and
//! values.

#![cfg(feature = "serde")]

mod common;

use meshcore::{BinaryReqType, Event, Options, PublicKey, RequestOptions};

/// Where this library deliberately differs from meshcore_py, as
/// (meshcore_py's text, this library's text).
const DEVIATIONS: &[(&str, &str)] = &[
    // The firmware sends the TX power as a signed byte (it takes -9 dBm and up);
    // meshcore_py reads it unsigned.
    (r#""tx_power":247"#, r#""tx_power":-9"#),
    // A contact's name ends at its first NUL; meshcore_py removes the NULs and
    // keeps the old name's tail the firmware leaves after a shorter new one.
    (r#""adv_name":"Alicedra""#, r#""adv_name":"Alice""#),
    // A trace of no whole hops (here two of 2 bytes and a byte more): the
    // firmware sends all hash bytes before the SNRs; meshcore_py reads the
    // SNRs from where the hash cut short begins, here its one byte.
    (
        r#"{"hash":"aaab","snr":-13.0},{"hash":"bbbc","snr":2.0},{"snr":-1.0}"#,
        r#"{"hash":"aaab","snr":2.0},{"hash":"bbbc","snr":-1.0},{"snr":3.0}"#,
    ),
];

#[test]
fn return_values_serialize_like_meshcore_py() {
    // the dicts of req_owner_info_sync, req_owner_sync and req_subscribe_sync
    let owner = meshcore::OwnerInfo {
        firmware_version: "v1.18.0".into(),
        name: "Rep 1".into(),
        owner: "Owner".into(),
    };
    assert_eq!(
        serde_json::to_string(&owner).unwrap(),
        r#"{"firmware_version":"v1.18.0","name":"Rep 1","owner":"Owner"}"#
    );
    let anon = meshcore::AnonOwner {
        name: "Rep 1".into(),
        owner: "Owner".into(),
    };
    assert_eq!(
        serde_json::to_string(&anon).unwrap(),
        r#"{"name":"Rep 1","owner":"Owner"}"#
    );
    let subscription = meshcore::SensorSubscription {
        expiry_secs: 3600,
        scope: "#hamburg".into(),
    };
    assert_eq!(
        serde_json::to_string(&subscription).unwrap(),
        r##"{"expiry_secs":3600,"scope":"#hamburg"}"##
    );
    // a fetched message as the event of its kind (frames.hex: the datagram)
    let data = meshcore::ReceivedMessage::ChannelData(meshcore::ChannelData {
        snr: 2.0,
        channel_idx: 3,
        path_len: meshcore::PathLen::from_byte(0x41),
        data_type: 0x1234,
        payload: vec![0xaa, 0xbb],
    });
    assert_eq!(
        serde_json::to_string(&data).unwrap(),
        r#"{"type":"DATA","SNR":2.0,"channel_idx":3,"path_hash_mode":1,"path_len":1,"data_type":4660,"data_len":2,"payload":"aabb"}"#
    );
}

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

#[test]
fn unreadable_replies_serialize_like_meshcore_py() {
    let json = |packet_type, reason: &str| {
        serde_json::to_string(&Event::Error(meshcore::ErrorInfo {
            code: None,
            reason: Some(reason.into()),
            packet_type: Some(packet_type),
        }))
        .unwrap()
    };
    // reader.py reports MSG_SENT, TUNING_PARAMS and STATS replies it cannot
    // read without the packet type, the others with it
    assert_eq!(
        json(6, "invalid_frame_length"),
        r#"{"reason":"invalid_frame_length"}"#
    );
    assert_eq!(
        json(23, "invalid_frame_length"),
        r#"{"reason":"invalid_frame_length"}"#
    );
    assert_eq!(
        json(24, "unknown_stats_type: 9"),
        r#"{"reason":"unknown_stats_type: 9"}"#
    );
    assert_eq!(
        json(27, "invalid_frame_length"),
        r#"{"reason":"invalid_frame_length","packet_type":27}"#
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
    let fixture = include_str!("fixtures/meshcore_py.jsonl");
    // each deviation listed is one meshcore_py shows
    for (py, _) in DEVIATIONS {
        assert!(fixture.contains(py), "meshcore_py does not show {py}");
    }
    let expected: Vec<String> = fixture
        .lines()
        .map(|line| {
            DEVIATIONS
                .iter()
                .fold(line.to_string(), |acc, (py, rs)| acc.replace(py, rs))
        })
        .collect();

    // binary requests get the tag 0xa1000000 + their type
    let (mc, device) = common::client(Options::default(), |cmd| match cmd[0] {
        50 => vec![common::sent_frame(
            false,
            0xa100_0000 + u32::from(cmd[33]),
            1_000_000,
        )],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // as in meshcore_py_reader.py
    mc.set_decrypt_channel_logs(true);
    let key = PublicKey::new([0x11; 32]);
    let neighbours = [0, 255, 0, 0, 0, 4, 1, 2, 3, 4];
    for (kind, data) in [
        (BinaryReqType::Status, &[][..]),
        (BinaryReqType::Telemetry, &[]),
        (BinaryReqType::Mma, &[0; 10]),
        (BinaryReqType::Acl, &[0, 0]),
        (BinaryReqType::Neighbours, &neighbours),
    ] {
        let options = RequestOptions::default();
        mc.send_binary_req(&key, kind, data, options).await.unwrap();
    }
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
