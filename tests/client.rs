//! End-to-end tests of the client against a simulated device.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{
    KEY, ack_frame, binary_response, client, contact_frame, err, ok, sent_frame, status_fields,
};
use meshcore::{
    BinaryReqType, Contact, ContactType, Error, ErrorCode, Event, EventKind, FloodScope,
    MAX_TEXT_LENGTH, NeighboursQuery, Options, PathLen, PublicKey, ReceivedMessage, RequestOptions,
    RetryOptions, TraceRequest,
};

fn key() -> PublicKey {
    PublicKey::new(KEY)
}

#[tokio::test(start_paused = true)]
async fn connect_sends_app_start_and_tracks_self_info() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    let mut events = mc.subscribe();
    let info = mc.connect().await.unwrap();
    assert_eq!(info.name, "Node");
    assert_eq!(info.radio_freq, 869.525);
    assert_eq!(info.telemetry_mode_base, 2);
    assert_eq!(device.commands()[0], b"\x01\x03      mccli".to_vec());
    assert!(mc.is_connected());
    assert_eq!(mc.connection_info().as_deref(), Some("mock"));
    assert_eq!(mc.self_info().unwrap().name, "Node");
    assert!(matches!(
        events.recv().await,
        Some(Event::Connected {
            reconnected: false,
            ..
        })
    ));

    mc.disconnect().await.unwrap();
    assert!(!mc.is_connected());
    let mut kinds = Vec::new();
    while let Some(event) = events.try_recv() {
        kinds.push(event.kind());
    }
    assert!(kinds.contains(&EventKind::Disconnected));
}

#[tokio::test(start_paused = true)]
async fn a_disconnect_cut_short_still_leaves_the_client_disconnected() {
    // Over BLE closing waits for BlueZ (2 s); a caller may stop waiting.
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.hang_on_close();
    let cut_short = tokio::time::timeout(Duration::from_millis(500), mc.disconnect()).await;
    assert!(cut_short.is_err());
    assert!(!mc.is_connected());
    assert!(matches!(
        events.try_recv(),
        Some(Event::Disconnected { reason }) if reason == "manual_disconnect"
    ));
}

#[tokio::test(start_paused = true)]
async fn connect_fails_without_an_answer() {
    let mc = silent_client();
    assert!(matches!(mc.connect().await, Err(Error::Timeout)));
    assert!(!mc.is_connected());
}

/// A device that takes every frame and never answers.
struct Silent;

/// Swallows what the client writes; holds the link open until closed.
struct Sink(Option<tokio::sync::mpsc::Sender<meshcore::LinkEvent>>);

impl meshcore::Transport for Silent {
    fn open(&self) -> meshcore::transport::BoxFuture<'_, meshcore::Result<meshcore::Link>> {
        Box::pin(async {
            let (sender, events) = tokio::sync::mpsc::channel(4);
            Ok(meshcore::Link {
                info: "silent".into(),
                events,
                writer: Box::new(Sink(Some(sender))),
            })
        })
    }

    fn describe(&self) -> String {
        "silent".into()
    }
}

impl meshcore::FrameWriter for Sink {
    fn write_frame<'a>(
        &'a mut self,
        _frame: &'a [u8],
    ) -> meshcore::transport::BoxFuture<'a, meshcore::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn close(&mut self) -> meshcore::transport::BoxFuture<'_, ()> {
        self.0 = None;
        Box::pin(async {})
    }
}

/// A client whose device never answers.
fn silent_client() -> meshcore::MeshCore {
    meshcore::MeshCore::new(Silent, Options::default())
}

#[tokio::test(start_paused = true)]
async fn device_errors_carry_the_firmware_code() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        6 => vec![err(6)],
        20 => vec![vec![12, 0xd4, 0x0d, 0, 4, 0, 0, 0, 16, 0, 0]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let battery = mc.get_bat().await.unwrap();
    assert_eq!(battery.level, 3540);
    assert_eq!(battery.used_kb, Some(1024));
    let error = mc.set_time(1).await.unwrap_err();
    assert!(matches!(error, Error::Device(ErrorCode::IllegalArg)));
    assert_eq!(error.to_string(), "device error 6 (ERR_CODE_ILLEGAL_ARG)");
}

#[tokio::test(start_paused = true)]
async fn reboot_does_not_wait_for_an_answer() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    mc.reboot().await.unwrap();
    device.settle().await;
    assert_eq!(device.commands_with(19), vec![b"\x13reboot".to_vec()]);
}

#[tokio::test(start_paused = true)]
async fn private_keys_are_64_bytes_both_ways() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        23 => {
            let mut f = vec![14];
            f.extend([0xab; 64]);
            vec![f]
        }
        24 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let key = mc.export_private_key().await.unwrap();
    mc.import_private_key(key.as_bytes()).await.unwrap();
    let import = &device.commands_with(24)[0];
    assert_eq!(import.len(), 65);
    assert_eq!(import[1..], [0xab; 64]);
}

#[tokio::test(start_paused = true)]
async fn disabled_key_export_and_import() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        23 | 24 => vec![vec![15]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(matches!(
        mc.export_private_key().await,
        Err(Error::Disabled)
    ));
    assert!(matches!(
        mc.import_private_key(&[0xab; 64]).await,
        Err(Error::Disabled)
    ));
}

#[tokio::test(start_paused = true)]
async fn contact_list_fills_and_replaces_the_cache() {
    let first = AtomicU32::new(0);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        4 => {
            let round = first.fetch_add(1, Ordering::SeqCst);
            let mut frames = vec![vec![2, 2, 0, 0, 0]];
            if round == 0 {
                frames.push(contact_frame(3, KEY, "Alice", 0xff, &[]));
                frames.push(contact_frame(3, [0x22; 32], "Rep", 0x41, &[0xaa, 0xbb]));
            } else {
                frames.push(contact_frame(3, [0x22; 32], "Rep", 0xff, &[]));
            }
            let mut end = vec![4];
            end.extend(5000u32.to_le_bytes());
            frames.push(end);
            frames
        }
        15 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let list = mc.get_contacts(0).await.unwrap();
    assert_eq!(list.lastmod, 5000);
    assert_eq!(list.contacts.len(), 2);
    let rep = &list.contacts[1];
    assert_eq!(rep.out_path_len.hops(), Some(1));
    assert_eq!(rep.out_path, vec![0xaa, 0xbb]);
    assert_eq!(mc.contacts().len(), 2);
    assert_eq!(mc.get_contact_by_name("alice").unwrap().public_key, key());
    assert!(mc.get_contact_by_key_prefix("2222").is_some());
    assert!(!mc.contacts_dirty());

    // a full list replaces the cache: Alice is gone
    mc.get_contacts(0).await.unwrap();
    assert_eq!(mc.contacts().len(), 1);
    assert!(mc.contact(&key()).is_none());

    mc.remove_contact(&PublicKey::new([0x22; 32]))
        .await
        .unwrap();
    assert_eq!(mc.contacts(), []);
    assert_eq!(device.commands_with(4)[0], vec![4]);
}

#[tokio::test(start_paused = true)]
async fn incremental_contacts_send_since_and_merge() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => {
            let mut end = vec![4];
            end.extend(6000u32.to_le_bytes());
            vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, [0x33; 32], "New", 0xff, &[]),
                end,
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.cache_contact(Contact::new(key(), ContactType::Chat, "Alice"));
    mc.get_contacts(5000).await.unwrap();
    assert_eq!(mc.contacts().len(), 2);
    assert_eq!(device.commands_with(4)[0], vec![4, 0x88, 0x13, 0, 0]);
    assert_eq!(mc.contacts_lastmod(), 6000);
}

#[tokio::test(start_paused = true)]
async fn contact_changes_are_written_and_cached() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        9 | 13 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let contact = Contact::new(key(), ContactType::Repeater, "Rep");
    mc.add_contact(&contact).await.unwrap();
    assert!(mc.contact(&key()).is_some());

    // no hash mode given: the device's (1 from DEVICE_QUERY, 2-byte hashes) is used
    let changed = mc
        .change_contact_path(&contact, &[1, 2, 3, 4, 5, 6], None)
        .await
        .unwrap();
    assert_eq!(changed.out_path_len, PathLen::new(3, 1).unwrap());
    assert_eq!(device.commands_with(22).len(), 1);
    let frame = device.commands_with(9).last().unwrap().clone();
    assert_eq!(frame[35], 0x43);
    assert_eq!(frame[36..42], [1, 2, 3, 4, 5, 6]);
    assert_eq!(mc.contact(&key()).unwrap().out_path_len.hops(), Some(3));
    // an explicit mode is used as given
    let explicit = mc
        .change_contact_path(&contact, &[1, 2, 3, 4, 5, 6], Some(2))
        .await
        .unwrap();
    assert_eq!(explicit.out_path_len, PathLen::new(2, 2).unwrap());
    assert_eq!(device.commands_with(22).len(), 1);

    let flagged = mc.change_contact_flags(&changed, 1).await.unwrap();
    assert_eq!(flagged.flags, 1);

    mc.reset_path(&key()).await.unwrap();
    assert!(mc.contact(&key()).unwrap().is_flood());

    // a name set past the field is cached as the device stores it
    let mut renamed = mc.contact(&key()).unwrap();
    renamed.name = "r".repeat(40);
    mc.update_contact(&renamed, None).await.unwrap();
    let frame = device.commands_with(9).last().unwrap().clone();
    assert_eq!(&frame[100..131], "r".repeat(31).as_bytes());
    assert_eq!(frame[131], 0);
    assert_eq!(mc.contact(&key()).unwrap().name, "r".repeat(31));
}

#[tokio::test(start_paused = true)]
async fn paths_the_length_byte_cannot_describe_are_refused() {
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();
    let contact = Contact::new(key(), ContactType::Repeater, "Rep");
    let refused = |result: meshcore::Result<()>| matches!(result, Err(Error::InvalidArgument(_)));

    // 3 bytes are no whole number of 2-byte hops; meshcore_py announces one
    let (path, mode) = meshcore::protocol::parse_path_hex("5f107c:1").unwrap();
    assert!(refused(
        mc.change_contact_path(&contact, &path, mode)
            .await
            .map(drop)
    ));
    // 64 hops do not fit the 6 bits of the hop count
    assert!(refused(
        mc.change_contact_path(&contact, &[7; 64], Some(0))
            .await
            .map(drop)
    ));
    // 33 hops of 2 bytes exceed the 64 bytes the firmware stores
    assert!(refused(
        mc.change_contact_path(&contact, &[7; 66], Some(1))
            .await
            .map(drop)
    ));
    // hash mode 3 (4-byte hashes) is reserved
    assert!(refused(
        mc.change_contact_path(&contact, &[1, 2, 3, 4], Some(3))
            .await
            .map(drop)
    ));
    // a length byte announcing 2 hops for a path of 1 byte
    let mut mismatched = contact.clone();
    mismatched.out_path_len = PathLen::new(2, 0).unwrap();
    mismatched.out_path = vec![1];
    assert!(refused(mc.update_contact(&mismatched, None).await));
    assert_eq!(device.commands_with(9).len(), 0);

    assert!(refused(
        mc.send_channel_data(1, 1, &[1], Some(&[1, 2, 3]), 1).await
    ));
    assert!(refused(mc.send_raw_data(&[1, 2, 3, 4], &[7; 64], 0).await));
    assert_eq!(device.commands_with(62).len(), 0);
    assert_eq!(device.commands_with(25).len(), 0);
}

#[tokio::test(start_paused = true)]
async fn new_adverts_are_pending_not_cached() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device
        .push(contact_frame(0x8a, KEY, "Stranger", 0xff, &[]))
        .await;
    let contact = loop {
        if let Event::NewContact(contact) = events.recv().await.unwrap() {
            break contact;
        }
    };
    assert_eq!(contact.name, "Stranger");
    assert_eq!(mc.contacts(), []);
    assert_eq!(mc.pending_contacts().len(), 1);
    assert!(mc.pop_pending_contact(&key()).is_some());
    assert_eq!(mc.pending_contacts(), []);

    device
        .push(contact_frame(0x8a, KEY, "Stranger", 0xff, &[]))
        .await;
    device.settle().await;
    assert_eq!(mc.pending_contacts().len(), 1);
    mc.flush_pending_contacts();
    assert_eq!(mc.pending_contacts(), []);
}

#[tokio::test(start_paused = true)]
async fn messages_and_retries() {
    let attempts = AtomicU32::new(0);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        2 => {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            let tag = 0x100 + attempt;
            let mut frames = vec![sent_frame(false, tag, 1000)];
            // only the second attempt is acknowledged
            if attempt == 1 {
                frames.push(ack_frame(tag));
            }
            frames
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let sent = mc
        .send_msg_with_retry(&key(), "hi", RetryOptions::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sent.expected_ack, 0x101);
    let frames = device.commands_with(2);
    assert_eq!(frames.len(), 2);
    // type, attempt, timestamp, prefix, text; the timestamp stays the same
    assert_eq!(frames[0][1], 0);
    assert_eq!(frames[0][2], 0);
    assert_eq!(frames[1][2], 1);
    assert_eq!(frames[0][3..7], frames[1][3..7]);
    assert_eq!(frames[0][7..13], KEY[..6]);
    assert_eq!(&frames[0][13..], b"hi");
}

#[tokio::test(start_paused = true)]
async fn retries_fall_back_to_flood() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x200, 500)],
        13 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let result = mc
        .send_msg_with_retry(&key(), "hi", RetryOptions::default())
        .await
        .unwrap();
    assert_eq!(result, None);
    // two direct attempts, then the path is reset, then the third attempt
    assert_eq!(device.commands_with(13).len(), 1);
    assert_eq!(device.commands_with(2).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn retries_to_an_unknown_prefix_go_on_without_the_reset() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x300, 500)],
        4 => vec![vec![2, 0, 0, 0, 0], vec![4, 0, 0, 0, 0]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let prefix: [u8; 6] = KEY[..6].try_into().unwrap();
    let result = mc
        .send_msg_with_retry(prefix, "hi", RetryOptions::default())
        .await
        .unwrap();
    assert_eq!(result, None);
    // the contacts are read for the full key, which they do not hold
    assert_eq!(device.commands_with(4).len(), 1);
    assert_eq!(device.commands_with(13).len(), 0);
    assert_eq!(device.commands_with(2).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn binary_answers_right_behind_msg_sent_are_matched() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        // MSG_SENT and the answer in one burst: the tag must already be bound
        50 if cmd[33] == BinaryReqType::Status.code() => {
            vec![
                sent_frame(false, 0xbeef, 2000),
                binary_response(0xbeef, &status_fields(4100)),
            ]
        }
        50 if cmd[33] == BinaryReqType::Mma.code() => {
            let mut answer = 1_700_000_000u32.to_le_bytes().to_vec();
            answer.extend([1, 103, 0, 200, 0, 255, 0, 221]);
            vec![
                sent_frame(false, 0xcafe, 2000),
                binary_response(0xcafe, &answer),
            ]
        }
        50 if cmd[33] == BinaryReqType::Acl.code() => {
            vec![
                sent_frame(false, 0xacc0, 2000),
                binary_response(0xacc0, &[1, 2, 3, 4, 5, 6, 3, 0, 0, 0, 0, 0, 0, 0]),
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let status = mc
        .req_status_sync(&key(), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(status.bat, 4100);
    assert_eq!(status.recv_errors, Some(7));
    assert_eq!(status.pubkey_prefix, KEY[..6]);
    assert_eq!(status.tag, Some(0xbeef));

    let mma = mc
        .req_mma_sync(&key(), 3600, 0, RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(mma.len(), 1);
    let request = &device.commands_with(50)[1];
    assert_eq!(request[34..], [0x10, 0x0e, 0, 0, 0, 0, 0, 0, 0, 0]);

    let acl = mc
        .req_acl_sync(&key(), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(acl.len(), 1);
    assert_eq!(device.commands_with(50)[2][34..], [0, 0]);
}

#[tokio::test(start_paused = true)]
async fn neighbours_are_paged() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        50 => {
            let offset = u16::from_le_bytes([cmd[36], cmd[37]]);
            let tag = 0x500 + u32::from(offset);
            let mut answer = 3i16.to_le_bytes().to_vec();
            let count: i16 = if offset == 0 { 2 } else { 1 };
            answer.extend(count.to_le_bytes());
            for i in 0..count {
                answer.extend([0xa0 + u8::try_from(i).unwrap(); 4]);
                answer.extend(60i32.to_le_bytes());
                answer.push(20);
            }
            vec![sent_frame(false, tag, 1000), binary_response(tag, &answer)]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let page = mc
        .req_neighbours_sync(
            &key(),
            NeighboursQuery::default(),
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(page.results_count, 2);
    assert_eq!(page.neighbours[0].snr, 5.0);
    let all = mc
        .fetch_all_neighbours(&key(), 0, 4, RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(all.neighbours.len(), 3);
    assert_eq!(all.tag, None);
    assert!(device.commands_with(50).len() >= 3);

    // a request of its own: the prefix length comes from its data
    let mut events = mc.subscribe();
    let data = [0, 255, 0, 0, 0, 4, 1, 2, 3, 4];
    mc.req_binary_sync(
        &key(),
        BinaryReqType::Neighbours,
        &data,
        RequestOptions::default(),
    )
    .await
    .unwrap();
    let typed = loop {
        if let Event::NeighboursResponse(page) = events.recv().await.unwrap() {
            break page;
        }
    };
    assert_eq!(typed.neighbours[1].pubkey, vec![0xa1; 4]);
    assert_eq!(typed.neighbours[1].secs_ago, 60);
}

#[tokio::test(start_paused = true)]
async fn anonymous_request_to_an_unknown_node_asks_for_a_zero_hop_reply() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        57 => {
            let mut answer = 7u32.to_le_bytes().to_vec();
            answer.extend(b"#hamburg,#de\0\0");
            vec![
                sent_frame(false, 0x77, 1000),
                binary_response(0x77, &answer),
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let regions = mc
        .req_regions_sync(&key(), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(regions, "#hamburg,#de");
    let frame = &device.commands_with(57)[0];
    assert_eq!(frame[1..33], KEY);
    assert_eq!(frame[33..], [1, 0]);
}

#[tokio::test(start_paused = true)]
async fn anonymous_requests_of_every_kind() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        57 => {
            let tag = 0x900 + u32::from(cmd[33]);
            let answer = [&tag.to_le_bytes()[..], b"answer"].concat();
            vec![sent_frame(false, tag, 1000), binary_response(tag, &answer)]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let options = RequestOptions::default();
    let regions = mc.req_regions_async(&key(), options).await.unwrap();
    let owner = mc.req_owner_async(&key(), options).await.unwrap();
    let basic = mc.req_basic_async(&key(), options).await.unwrap();
    assert_eq!(
        [regions.expected_ack, owner.expected_ack, basic.expected_ack],
        [0x901, 0x902, 0x903]
    );
    let answer = mc.req_basic_sync(&key(), options).await.unwrap();
    assert_eq!(answer.tag, 0x903);
    assert_eq!(&answer.data[4..], b"answer");
    // the request type, then a zero-hop reply path to a node not in the contacts
    let frames = device.commands_with(57);
    let tails: Vec<&[u8]> = frames.iter().map(|frame| &frame[33..]).collect();
    assert_eq!(tails, [&[1, 0][..], &[2, 0], &[3, 0], &[3, 0]]);
}

#[tokio::test(start_paused = true)]
async fn binary_requests_without_waiting() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        50 => {
            let tag = 0xa00 + u32::from(cmd[33]);
            let answer = if cmd[33] == BinaryReqType::Neighbours.code() {
                // one neighbour, with the 6-byte prefix the request asks for
                let mut answer = 1i16.to_le_bytes().to_vec();
                answer.extend(1i16.to_le_bytes());
                answer.extend([0xa0; 6]);
                answer.extend(90i32.to_le_bytes());
                answer.push(12);
                answer
            } else {
                vec![]
            };
            vec![sent_frame(false, tag, 1000), binary_response(tag, &answer)]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let options = RequestOptions::default();
    let mut events = mc.subscribe();
    let query = NeighboursQuery {
        pubkey_prefix_length: 6,
        ..NeighboursQuery::default()
    };
    let sent = mc
        .req_neighbours_async(&key(), query, options)
        .await
        .unwrap();
    assert_eq!(sent.expected_ack, 0xa06);
    let page = loop {
        if let Event::NeighboursResponse(page) = events.recv().await.unwrap() {
            break page;
        }
    };
    assert_eq!(page.tag, Some(0xa06));
    assert_eq!(page.neighbours[0].pubkey, vec![0xa0; 6]);
    assert_eq!(page.neighbours[0].secs_ago, 90);

    let keep_alive = mc
        .send_binary_req(&key(), BinaryReqType::KeepAlive, &[7], options)
        .await
        .unwrap();
    assert_eq!(keep_alive.expected_ack, 0xa02);
    mc.req_unsubscribe_sync(&key(), options).await.unwrap();
    let frames = device.commands_with(50);
    assert_eq!(frames[1][33..], [0x02, 7]);
    assert_eq!(frames[2][33..], [0x09]);
}

#[tokio::test(start_paused = true)]
async fn anonymous_request_to_a_flood_contact_goes_zero_hop_and_back() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![contact_frame(3, KEY, "Rep", 0xff, &[])],
        9 | 13 => vec![ok()],
        57 => vec![sent_frame(false, 0x78, 1000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.cache_contact(Contact::new(key(), ContactType::Repeater, "Rep"));
    let sent = mc
        .send_anon_req(
            &key(),
            meshcore::AnonReqType::Owner,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(sent.suggested_timeout, 1000);
    let codes: Vec<u8> = device
        .commands()
        .iter()
        .map(|c| c[0])
        .filter(|&c| c != 1)
        .collect();
    // read the device's record, set zero-hop (mode from DEVICE_QUERY), send, reset
    assert_eq!(codes, vec![30, 22, 9, 57, 13]);
    assert!(mc.contact(&key()).unwrap().is_flood());
}

#[tokio::test(start_paused = true)]
async fn login_success_and_failure() {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let fail_flag = Arc::clone(&fail);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        26 => {
            let mut answer = if fail_flag.load(Ordering::SeqCst) {
                vec![0x86, 0]
            } else {
                vec![0x85, 1]
            };
            answer.extend(&KEY[..6]);
            if !fail_flag.load(Ordering::SeqCst) {
                answer.extend(99u32.to_le_bytes());
                answer.extend([3, 2]);
            }
            vec![sent_frame(false, 0x1111_1111, 1000), answer]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let login = mc
        .send_login_sync(&key(), "secret", RequestOptions::default())
        .await
        .unwrap();
    assert!(login.is_admin());
    assert_eq!(login.server_timestamp, Some(99));
    fail.store(true, Ordering::SeqCst);
    assert!(matches!(
        mc.send_login_sync(&key(), "wrong", RequestOptions::default())
            .await,
        Err(Error::LoginFailed)
    ));
}

#[tokio::test(start_paused = true)]
async fn get_msg_returns_all_message_kinds() {
    let queue = Arc::new(std::sync::Mutex::new(vec![
        vec![10],
        vec![27, 8, 0, 0, 1, 0xff, 0x34, 0x12, 1, 0x42],
        {
            let mut f = vec![17, 4, 0, 0, 0, 2, 0];
            f.extend(5u32.to_le_bytes());
            f.extend(b"bob: hey");
            f
        },
    ]));
    let queue_ref = Arc::clone(&queue);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        10 => vec![queue_ref.lock().unwrap().pop().unwrap_or_else(|| vec![10])],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let messages = mc.fetch_messages().await.unwrap();
    assert_eq!(messages.len(), 2);
    let ReceivedMessage::Channel(channel) = &messages[0] else {
        panic!()
    };
    assert_eq!(channel.text, "bob: hey");
    assert_eq!(channel.snr, Some(1.0));
    assert_eq!(channel.path_len.hops(), Some(2));
    let ReceivedMessage::ChannelData(data) = &messages[1] else {
        panic!()
    };
    assert_eq!(data.data_type, 0x1234);
    assert_eq!(data.payload, vec![0x42]);
    assert!(data.path_len.is_none());
}

#[tokio::test(start_paused = true)]
async fn settings_keep_the_others_without_app_start() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        38 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.set_multi_acks(2).await.unwrap();
    mc.set_telemetry_mode_env(1).await.unwrap();
    // only the APP_START of connecting
    assert_eq!(device.commands_with(1).len(), 1);
    let last = device.commands_with(38).last().unwrap().clone();
    // manual add 0, telemetry base 2 / loc 1 / env 1, loc policy 1, multi acks 2
    assert_eq!(last, vec![38, 0, 0b01_01_10, 1, 2]);
    assert_eq!(mc.self_info().unwrap().multi_acks, 2);

    mc.set_manual_add_contacts(true).await.unwrap();
    mc.set_telemetry_mode_base(1).await.unwrap();
    mc.set_telemetry_mode_loc(2).await.unwrap();
    mc.set_advert_loc_policy(0).await.unwrap();
    let last = device.commands_with(38).last().unwrap().clone();
    // manual add 1, telemetry base 1 / loc 2 / env 1, loc policy 0, multi acks 2
    assert_eq!(last, vec![38, 1, 0b01_10_01, 0, 2]);
    assert_eq!(device.commands_with(1).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn command_frames_match_the_protocol() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        36 => vec![sent_frame(false, 0x42, 100)],
        _ => vec![ok()],
    });
    mc.connect().await.unwrap();

    mc.send_channel_data(1, 0xffff, &[0xa1, 0xb2, 0xc3], None, 0)
        .await
        .unwrap();
    assert_eq!(
        device.commands_with(62)[0],
        meshcore::hex::decode("3E01FFFFFFA1B2C3").unwrap()
    );
    mc.send_channel_data(2, 0x0102, &[7], Some(&[0xaa, 0xbb]), 0)
        .await
        .unwrap();
    assert_eq!(
        device.commands_with(62)[1],
        meshcore::hex::decode("3E0202AABB020107").unwrap()
    );
    assert!(mc.send_channel_data(1, 0, &[1], None, 0).await.is_err());
    assert!(
        mc.send_channel_data(1, 1, &[0; 164], None, 0)
            .await
            .is_err()
    );

    let no_path = mc.send_trace(&TraceRequest::default()).await;
    assert!(matches!(no_path, Err(Error::InvalidArgument(_))));
    mc.send_trace(&TraceRequest {
        tag: Some(1),
        path: vec![0xaa],
        ..TraceRequest::default()
    })
    .await
    .unwrap();
    assert_eq!(
        device.commands_with(36)[0],
        [36, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0xaa]
    );

    mc.set_flood_scope(&FloodScope::Unscoped).await.unwrap();
    mc.set_flood_scope_str("hamburg").await.unwrap();
    let scopes = device.commands_with(54);
    assert_eq!(scopes[0], vec![54, 1]);
    assert_eq!(scopes[1][2..], FloodScope::key_for_name("#hamburg"));

    mc.set_default_flood_scope(Some("hamburg")).await.unwrap();
    let default_scope = &device.commands_with(63)[0];
    assert_eq!(default_scope.len(), 1 + 31 + 16);
    assert_eq!(&default_scope[1..9], b"#hamburg");

    mc.set_channel(1, "#test", None).await.unwrap();
    let channel = &device.commands_with(32)[0];
    assert_eq!(channel[34..], meshcore::channel_secret_from_name("#test"));

    mc.set_radio(433.875, 62.5, 9, 8, Some(true)).await.unwrap();
    let radio = &device.commands_with(11)[0];
    assert_eq!(radio[1..5], 433_875u32.to_le_bytes());
    assert_eq!(radio[5..9], 62_500u32.to_le_bytes());
    assert_eq!(radio[9..], [9, 8, 1]);
    let info = mc.self_info().unwrap();
    assert_eq!(
        (info.radio_freq, info.radio_bw, info.radio_sf, info.radio_cr),
        (433.875, 62.5, 9, 8)
    );

    mc.set_tx_power(-9).await.unwrap();
    assert_eq!(
        device.commands_with(12)[0],
        vec![12, 0xf7, 0xff, 0xff, 0xff]
    );
    assert_eq!(mc.self_info().unwrap().tx_power, -9);

    let tag = mc
        .send_node_discover_req(0x04, true, Some(9), None)
        .await
        .unwrap();
    assert_eq!(tag, 9);
    assert_eq!(
        device.commands_with(55)[0],
        vec![55, 0x81, 0x04, 9, 0, 0, 0]
    );

    mc.set_path_hash_mode(1).await.unwrap();
    assert_eq!(device.commands_with(61)[0], vec![61, 0, 1]);
}

#[tokio::test(start_paused = true)]
async fn read_commands_return_their_replies() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd {
        [56, 0] => {
            let mut f = vec![24, 0];
            f.extend(4100u16.to_le_bytes());
            f.extend(3600u32.to_le_bytes());
            f.extend(0u16.to_le_bytes());
            f.push(2);
            vec![f]
        }
        [56, 1] => {
            let mut f = vec![24, 1];
            f.extend((-110i16).to_le_bytes());
            f.extend([(-80i8).cast_unsigned(), 26]);
            f.extend(10u32.to_le_bytes());
            f.extend(20u32.to_le_bytes());
            vec![f]
        }
        [56, 2] => {
            let mut f = vec![24, 2];
            for value in 1u32..=7 {
                f.extend(value.to_le_bytes());
            }
            vec![f]
        }
        [43] => vec![[&[23u8][..], &500u32.to_le_bytes(), &1200u32.to_le_bytes()].concat()],
        [40] => vec![[&[21u8][..], b"gps:1,url:x"].concat()],
        [64] => {
            let mut name = [0u8; 31];
            name[..8].copy_from_slice(b"#hamburg");
            vec![[&[28u8][..], &name, &[5; 16]].concat()]
        }
        [59] => vec![vec![25, 0x0e, 3]],
        [60] => vec![
            [
                &[26u8][..],
                &433_000u32.to_le_bytes(),
                &434_000u32.to_le_bytes(),
            ]
            .concat(),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let core = mc.get_stats_core().await.unwrap();
    assert_eq!(
        (core.battery_mv, core.uptime_secs, core.queue_len),
        (4100, 3600, 2)
    );
    let radio = mc.get_stats_radio().await.unwrap();
    assert_eq!((radio.noise_floor, radio.last_rssi), (-110, -80));
    assert_eq!(radio.last_snr, 6.5);
    let packets = mc.get_stats_packets().await.unwrap();
    assert_eq!(
        (packets.recv, packets.direct_rx, packets.recv_errors),
        (1, 6, Some(7))
    );
    let tuning = mc.get_tuning().await.unwrap();
    assert_eq!((tuning.rx_delay, tuning.airtime_factor), (500, 1200));
    assert_eq!(mc.get_custom_vars().await.unwrap().get("url"), Some("x"));
    let scope = mc.get_default_flood_scope().await.unwrap().unwrap();
    assert_eq!((scope.name.as_str(), scope.key), ("#hamburg", [5; 16]));
    let autoadd = mc.get_autoadd_config().await.unwrap();
    assert_eq!((autoadd.config, autoadd.max_hops), (0x0e, Some(3)));
    assert_eq!(
        mc.get_allowed_repeat_freq().await.unwrap(),
        [meshcore::FreqRange {
            min: 433_000,
            max: 434_000
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn contacts_one_by_one_and_without_waiting() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![contact_frame(3, KEY, "Alice", 0xff, &[])],
        4 => {
            let mut end = vec![4];
            end.extend(7000u32.to_le_bytes());
            vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, [0x33; 32], "Bob", 0xff, &[]),
                end,
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // one contact read alone goes into the cache
    assert_eq!(mc.get_contact_by_key(&key()).await.unwrap().name, "Alice");
    assert_eq!(mc.contacts().len(), 1);
    assert_eq!(
        device.commands_with(30)[0],
        [&[30u8][..], &KEY[..]].concat()
    );
    // a list asked for without waiting arrives as events, and replaces the cache
    let mut events = mc.subscribe();
    mc.get_contacts_async(0).await.unwrap();
    let list = loop {
        if let Event::Contacts(list) = events.recv().await.unwrap() {
            break list;
        }
    };
    assert_eq!(list.contacts[0].name, "Bob");
    assert_eq!(mc.contacts_lastmod(), 7000);
    assert!(mc.contact(&key()).is_none());
    assert!(mc.uncache_contact(&PublicKey::new([0x33; 32])).is_some());
    assert_eq!(mc.contacts(), []);
}

#[tokio::test(start_paused = true)]
async fn contacts_are_read_when_needed() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => {
            let mut end = vec![4];
            end.extend(5000u32.to_le_bytes());
            vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, KEY, "Alice", 0xff, &[]),
                end,
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // nothing cached: read; cached and unchanged: not again
    assert!(mc.ensure_contacts(false).await.unwrap());
    assert!(!mc.ensure_contacts(true).await.unwrap());
    // an advert marks them changed: read since the last lastmod when following
    let mut advert = vec![0x80];
    advert.extend(KEY);
    device.push(advert).await;
    device.settle().await;
    assert!(mc.contacts_dirty());
    assert!(!mc.ensure_contacts(false).await.unwrap());
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(
        device.commands_with(4),
        [vec![4], [&[4u8][..], &5000u32.to_le_bytes()[..]].concat()]
    );
}

#[tokio::test(start_paused = true)]
async fn flood_scopes_go_back_to_the_default_or_none() {
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();
    mc.reset_flood_scope().await.unwrap();
    mc.force_unscoped().await.unwrap();
    let scopes = device.commands_with(54);
    assert_eq!(scopes[0], [&[54u8, 0][..], &[0; 16]].concat());
    assert_eq!(scopes[1], [54, 1]);
    mc.reset_default_flood_scope().await.unwrap();
    assert_eq!(device.commands_with(63), [vec![63]]);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_events() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        10 => vec![vec![10]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let push_soon = |frame: Vec<u8>| {
        let device = device.clone();
        tokio::spawn(async move {
            device.settle().await;
            device.push(frame).await;
        })
    };
    push_soon(vec![0x83]);
    let waiting = mc.wait_for_event(EventKind::MessagesWaiting, None).await;
    assert!(matches!(waiting, Some(Event::MessagesWaiting)));
    let mut advert = vec![0x80];
    advert.extend(KEY);
    push_soon(advert);
    let heard = mc
        .wait_for(None, |event| event.pubkey_prefix() == Some([0x11; 6]))
        .await;
    assert!(matches!(heard, Some(Event::Advertisement(k)) if k == key()));
    // nothing comes: None once the timeout is over
    let one_second = Some(Duration::from_secs(1));
    assert!(
        mc.wait_for_event(EventKind::Ack, one_second)
            .await
            .is_none()
    );
    let none = mc.get_msg_timeout(Duration::from_secs(1)).await.unwrap();
    assert!(none.is_none());
}

#[tokio::test(start_paused = true)]
async fn signing_sends_the_data_in_chunks() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        33 => {
            let mut start = vec![19, 0];
            start.extend(8192u32.to_le_bytes());
            vec![start]
        }
        34 => vec![ok()],
        35 => {
            let mut signature = vec![20];
            signature.extend([0x5a; 64]);
            vec![signature]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // a chunk has to fit a frame: refused before a session starts
    for chunk_size in [0, 176] {
        let refused = mc.sign(&[1; 10], chunk_size, None).await;
        assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    }
    assert!(matches!(
        mc.sign_data(&[]).await,
        Err(Error::InvalidArgument(_))
    ));
    assert_eq!(device.commands_with(33).len(), 0);

    let data: Vec<u8> = (0..=249).collect();
    let signature = mc.sign(&data, 120, None).await.unwrap();
    assert_eq!(signature, vec![0x5a; 64]);
    let chunks = device.commands_with(34);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0][1..], data[..120]);
    assert_eq!(chunks[2][1..], data[240..]);
    assert_eq!(device.commands_with(35).len(), 1);

    // more than the device takes (8192 bytes): refused once sign_start told
    // the limit, before any chunk
    let too_long = mc.sign(&[1; 8193], 120, None).await;
    assert!(matches!(too_long, Err(Error::InvalidArgument(_))));
    assert_eq!(device.commands_with(33).len(), 2);
    assert_eq!(device.commands_with(34).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn remaining_command_frames_match_the_firmware() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        17 => vec![vec![11, 0xaa, 0xbb]],
        27 | 39 | 52 => vec![sent_frame(false, 0x42, 1000)],
        42 => {
            let mut path = vec![22];
            path.extend(1_700_000_000u32.to_le_bytes());
            path.extend([0x41, 0xaa, 0xbb]);
            vec![path]
        }
        66 => vec![[&[29u8][..], b"> v1.18.0"].concat()],
        _ => vec![ok()],
    });
    mc.connect().await.unwrap();
    let key_frame = |code: u8| [&[code][..], &KEY[..]].concat();

    mc.set_coords(53.5511, -9.9937).await.unwrap();
    let mut coords = vec![14];
    coords.extend(53_551_100i32.to_le_bytes());
    coords.extend((-9_993_700i32).to_le_bytes());
    coords.extend([0; 4]);
    assert_eq!(device.commands_with(14)[0], coords);
    assert_eq!(mc.self_info().unwrap().longitude_e6, -9_993_700);

    mc.set_tuning(500, 1200).await.unwrap();
    let mut tuning = vec![21];
    tuning.extend(500u32.to_le_bytes());
    tuning.extend(1200u32.to_le_bytes());
    tuning.extend([0, 0]);
    assert_eq!(device.commands_with(21)[0], tuning);

    let params = meshcore::OtherParams {
        manual_add_contacts: true,
        telemetry_mode_base: 2,
        telemetry_mode_loc: 1,
        // two bits on the wire: 4 goes out as 0
        telemetry_mode_env: 4,
        adv_loc_policy: 1,
        multi_acks: 3,
    };
    mc.set_other_params(&params).await.unwrap();
    assert_eq!(device.commands_with(38)[0], [38, 1, 0b00_01_10, 1, 3]);
    let info = mc.self_info().unwrap();
    assert_eq!((info.telemetry_mode_base, info.telemetry_mode_loc), (2, 1));
    assert_eq!(info.telemetry_mode_env, 0);

    mc.set_autoadd_config(0x0e, Some(3)).await.unwrap();
    mc.set_autoadd_config(0x02, None).await.unwrap();
    assert_eq!(
        device.commands_with(58),
        [vec![58, 0x0e, 3], vec![58, 0x02]]
    );

    let path = mc.get_advert_path(&key()).await.unwrap();
    assert_eq!(path.path, vec![0xaa, 0xbb]);
    assert_eq!(
        device.commands_with(42)[0],
        [&[42u8, 0][..], &KEY[..]].concat()
    );

    mc.send_raw_data(&[1, 2, 3, 4], &[0xaa], 0).await.unwrap();
    assert_eq!(device.commands_with(25)[0], [25, 1, 0xaa, 1, 2, 3, 4]);
    mc.send_raw_packet_with_priority(&[0x11, 0, 9, 9], 2)
        .await
        .unwrap();
    assert_eq!(device.commands_with(65)[0], [65, 2, 0x11, 0, 9, 9]);

    assert_eq!(mc.run_cli_command("ver").await.unwrap(), "> v1.18.0");
    assert_eq!(device.commands_with(66)[0], b"\x42ver");

    assert_eq!(mc.export_contact(None).await.unwrap(), "meshcore://aabb");
    mc.export_contact(Some(&key())).await.unwrap();
    assert_eq!(device.commands_with(17), [vec![17], key_frame(17)]);
    mc.share_contact(&key()).await.unwrap();
    assert_eq!(device.commands_with(16)[0], key_frame(16));
    mc.send_logout(&key()).await.unwrap();
    assert_eq!(device.commands_with(29)[0], key_frame(29));

    mc.set_custom_var("gps", "1").await.unwrap();
    assert_eq!(device.commands_with(41)[0], b"\x29gps:1");
    mc.set_devicepin(123_456).await.unwrap();
    assert_eq!(
        device.commands_with(37)[0],
        [&[37u8][..], &123_456u32.to_le_bytes()[..]].concat()
    );
    mc.send_advert(false).await.unwrap();
    mc.send_advert(true).await.unwrap();
    assert_eq!(device.commands_with(7), [vec![7], vec![7, 1]]);

    mc.send_statusreq(&key()).await.unwrap();
    assert_eq!(device.commands_with(27)[0], key_frame(27));
    mc.send_telemetry_req(&key()).await.unwrap();
    assert_eq!(
        device.commands_with(39)[0],
        [&[39u8, 0, 0, 0][..], &KEY[..]].concat()
    );
    mc.send_path_discovery(&key()).await.unwrap();
    assert_eq!(
        device.commands_with(52)[0],
        [&[52u8, 0][..], &KEY[..]].concat()
    );
}

#[tokio::test(start_paused = true)]
async fn coordinates_and_frequencies_are_rounded_and_checked() {
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();

    // rounded with halves to even as meshcore_py rounds, and checked after
    // rounding as the firmware checks
    mc.set_coords(90.000_000_4, -0.000_002_5).await.unwrap();
    let mut coords = vec![14];
    coords.extend(90_000_000i32.to_le_bytes());
    coords.extend((-2i32).to_le_bytes());
    coords.extend([0; 4]);
    assert_eq!(device.commands_with(14), vec![coords]);
    let info = mc.self_info().unwrap();
    assert_eq!((info.latitude_e6, info.longitude_e6), (90_000_000, -2));
    for (lat, lon) in [(90.000_000_6, 0.0), (0.0, -180.5), (f64::NAN, 0.0)] {
        assert!(matches!(
            mc.set_coords(lat, lon).await,
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(device.commands_with(14).len(), 1);

    // rounded to whole kHz: meshcore_py cuts 512.002 MHz (512001.99999999994
    // kHz as a float) to 512001 kHz and 869.4625 MHz to 869462 kHz
    mc.set_radio(512.002, 62.5, 9, 8, None).await.unwrap();
    mc.set_radio(869.4625, 62.5, 9, 8, None).await.unwrap();
    let radio = device.commands_with(11);
    assert_eq!(radio[0][1..5], 512_002u32.to_le_bytes());
    assert_eq!(radio[1][1..5], 869_463u32.to_le_bytes());
    for (freq, bw) in [(f64::NAN, 62.5), (-1.0, 62.5), (869.525, f64::INFINITY)] {
        assert!(matches!(
            mc.set_radio(freq, bw, 9, 8, None).await,
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(device.commands_with(11).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn remote_answers_are_read_like_meshcore_py() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        50 => {
            let tag = 0x600 + u32::from(cmd[33]);
            let answer = match cmd[33] {
                0x03 => vec![1, 116, 0x01, 0x9c],
                0x07 => b"v1.18.0\nRep 1\nOwner\nsecond line\0\0".to_vec(),
                0x08 => [&3600u16.to_le_bytes()[..], &[0, 0], b"#hamburg\0"].concat(),
                _ => vec![],
            };
            vec![sent_frame(false, tag, 1000), binary_response(tag, &answer)]
        }
        57 => {
            let answer = [&7u32.to_le_bytes()[..], b"Rep 1\nOwner\0"].concat();
            vec![
                sent_frame(false, 0x700, 1000),
                binary_response(0x700, &answer),
            ]
        }
        52 => {
            let mut found = vec![0x8d, 0];
            found.extend(&KEY[..6]);
            found.extend([0x41, 0xaa, 0xbb, 0xff]);
            vec![sent_frame(false, 0x800, 1000), found]
        }
        36 => {
            let mut trace = vec![0x89, 0, 1, 0];
            trace.extend(5u32.to_le_bytes());
            trace.extend(0u32.to_le_bytes());
            trace.extend([0xaa, 8, 12]);
            vec![sent_frame(false, 5, 1000), trace]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let options = RequestOptions::default();

    let telemetry = mc.req_telemetry_sync(&key(), options).await.unwrap();
    assert_eq!(telemetry.voltage(), Some(4.12));

    let owner = mc.req_owner_info_sync(&key(), options).await.unwrap();
    assert_eq!(owner.firmware_version, "v1.18.0");
    assert_eq!(owner.name, "Rep 1");
    assert_eq!(owner.owner, "Owner\nsecond line");

    let subscription = mc
        .req_subscribe_sync(&key(), [1, 2, 3, 4], 3600, &[1, 103, 10], options)
        .await
        .unwrap();
    assert_eq!(subscription.expiry_secs, 3600);
    assert_eq!(subscription.scope, "#hamburg");
    let request = device.commands_with(50).last().unwrap().clone();
    assert_eq!(request[34..], [1, 2, 3, 4, 0x10, 0x0e, 0, 3, 1, 103, 10]);

    let anon = mc.req_owner_sync(&key(), options).await.unwrap();
    assert_eq!(
        (anon.name.as_str(), anon.owner.as_str()),
        ("Rep 1", "Owner")
    );

    let paths = mc.send_path_discovery_sync(&key(), options).await.unwrap();
    assert_eq!(paths.out_path, vec![0xaa, 0xbb]);
    assert!(paths.in_path_len.is_none());

    let trace = mc
        .trace_sync(
            &TraceRequest {
                tag: Some(5),
                path: vec![0xaa],
                ..TraceRequest::default()
            },
            options,
        )
        .await
        .unwrap();
    assert_eq!(trace.hops.len(), 1);
    assert_eq!(trace.hops[0].snr, 2.0);
    assert_eq!(trace.final_snr, Some(3.0));
}

#[tokio::test(start_paused = true)]
async fn endless_timeouts_do_not_panic() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x31, 1000), ack_frame(0x31)],
        20 => vec![vec![12, 0xd4, 0x0d]],
        50 => vec![
            sent_frame(false, 0x32, 1000),
            binary_response(0x32, &status_fields(4100)),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // kept exactly, not cut to whole milliseconds
    mc.set_default_timeout(Duration::from_micros(500));
    assert_eq!(mc.default_timeout(), Duration::from_micros(500));
    // Duration::MAX as "no limit" must not overflow a deadline
    mc.set_default_timeout(Duration::MAX);
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    let endless = RequestOptions::timeout(Duration::MAX);
    assert_eq!(mc.req_status_sync(&key(), endless).await.unwrap().bat, 4100);
    let retry = RetryOptions {
        timeout: Some(Duration::MAX),
        ..RetryOptions::default()
    };
    assert!(
        mc.send_msg_with_retry(&key(), "hi", retry)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test(start_paused = true)]
async fn factory_reset_needs_the_token_and_sends_reset() {
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();
    assert!(matches!(
        mc.confirm_factory_reset("nope").await,
        Err(Error::InvalidArgument(_))
    ));
    let token = mc.request_factory_reset();
    mc.confirm_factory_reset(&token).await.unwrap();
    assert_eq!(device.commands_with(51)[0], b"\x33reset".to_vec());
    assert!(mc.confirm_factory_reset(&token).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn self_telemetry_is_told_apart_from_another_nodes() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd {
        [39, 0, 0, 0] => {
            // an answer to an earlier telemetry request of another node, then
            // the device's own (self info: key 0x22...)
            let mut other = vec![0x8b, 0];
            other.extend([0x11; 6]);
            other.extend([1, 116, 0x01, 0x90]);
            let mut own = vec![0x8b, 0];
            own.extend([0x22; 6]);
            own.extend([1, 116, 0x01, 0x4a]);
            vec![other, own]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let telemetry = mc.get_self_telemetry().await.unwrap();
    assert_eq!(telemetry.pubkey_prefix, [0x22; 6]);
    assert_eq!(telemetry.telemetry.voltage(), Some(3.3));
}

#[tokio::test(start_paused = true)]
async fn channel_names_are_cut_where_the_firmware_cuts_them() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        32 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let name = format!("#{}", "a".repeat(40));
    mc.set_channel(1, &name, None).await.unwrap();
    let frame = &device.commands_with(32)[0];
    assert_eq!(frame.len(), 2 + 32 + 16);
    assert_eq!(&frame[2..33], &name.as_bytes()[..31]);
    assert_eq!(frame[33], 0);
    // the key comes from the whole name, as meshcore_py derives it
    assert_eq!(&frame[34..], &meshcore::channel_secret_from_name(&name));
}

#[tokio::test(start_paused = true)]
async fn commands_longer_than_the_firmware_takes_are_refused() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        18 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // the command byte and 175 bytes of card fill the 176-byte frame
    mc.import_contact(&[0xaa; 175]).await.unwrap();
    let too_long = mc.import_contact(&[0xaa; 176]).await;
    assert!(matches!(too_long, Err(Error::InvalidArgument(_))));
    device.settle().await;
    assert_eq!(device.commands_with(18).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn texts_longer_than_the_firmware_takes_are_refused() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 1, 1000)],
        3 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let text = |len| "x".repeat(len);
    mc.send_msg(key(), &text(MAX_TEXT_LENGTH), None, 0)
        .await
        .unwrap();
    let refused = |result: meshcore::Result<_>| matches!(result, Err(Error::InvalidArgument(_)));
    assert!(refused(
        mc.send_msg(key(), &text(MAX_TEXT_LENGTH + 1), None, 0)
            .await
            .map(drop)
    ));
    assert!(refused(
        mc.send_msg(key(), &text(MAX_TEXT_LENGTH - 1), None, 4)
            .await
            .map(drop)
    ));
    assert!(refused(
        mc.send_cmd(key(), &text(MAX_TEXT_LENGTH + 1), None, None)
            .await
            .map(drop)
    ));
    // the device sends "Node: " before the text
    mc.send_chan_msg(0, &text(MAX_TEXT_LENGTH - 6), None)
        .await
        .unwrap();
    assert!(refused(
        mc.send_chan_msg(0, &text(MAX_TEXT_LENGTH - 5), None).await
    ));
    // empty texts are refused, the firmware would take them for unknown commands
    assert!(refused(mc.send_msg(key(), "", None, 0).await.map(drop)));
    assert!(refused(mc.send_cmd(key(), "", None, None).await.map(drop)));
    assert!(refused(mc.set_name("").await));
    assert!(refused(mc.run_cli_command("v").await.map(drop)));
    assert!(refused(mc.set_custom_var("", "1").await));
    assert!(refused(mc.set_custom_var("a", "").await));
    assert!(refused(mc.set_custom_var("a:b", "c").await));
    // the firmware would end these texts at the NUL
    assert!(refused(mc.send_msg(key(), "a\0b", None, 0).await.map(drop)));
    assert!(refused(mc.send_chan_msg(0, "a\0b", None).await));
    assert!(refused(mc.set_name("a\0b").await));
    assert!(refused(mc.run_cli_command("ve\0r").await.map(drop)));
    assert!(refused(mc.set_channel(1, "#a\0b", None).await));
    assert!(refused(mc.set_custom_var("gps", "1\0").await));
    assert!(refused(mc.set_default_flood_scope(Some("a\0b")).await));
    // the firmware sends at most 15 bytes of a password
    assert!(refused(
        mc.send_login(&key(), &"p".repeat(16)).await.map(drop)
    ));
    assert!(refused(mc.send_control_data(0x10, &[1]).await));
    assert!(refused(mc.send_raw_packet(&[0x11]).await));
    assert!(refused(mc.import_contact(&[0; 97]).await));
    // an empty channel message the firmware sends as "Node: "
    mc.send_chan_msg(0, "", None).await.unwrap();
    // retrying does not make a text fit
    let retried = mc
        .send_msg_with_retry(key(), &text(MAX_TEXT_LENGTH + 1), RetryOptions::default())
        .await;
    assert!(matches!(retried, Err(Error::InvalidArgument(_))));
    // five attempts reach the shorter limit: refused before the first
    let five = RetryOptions {
        max_attempts: 5,
        ..RetryOptions::default()
    };
    let retried = mc
        .send_msg_with_retry(key(), &text(MAX_TEXT_LENGTH), five)
        .await;
    assert!(matches!(retried, Err(Error::InvalidArgument(_))));
    device.settle().await;
    assert_eq!(device.commands_with(2).len(), 1);
    assert_eq!(device.commands_with(3).len(), 2);
    assert_eq!(device.commands_with(8).len(), 0);
    assert_eq!(device.commands_with(66).len(), 0);
    assert_eq!(device.commands_with(41).len(), 0);
    for code in [55, 65, 18, 26, 32, 63] {
        assert_eq!(device.commands_with(code).len(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn node_names_are_cut_where_the_firmware_cuts_them() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        8 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // 30 ASCII bytes and a two-byte character would end at byte 32
    let name = format!("{}é", "n".repeat(30));
    mc.set_name(&name).await.unwrap();
    let frame = &device.commands_with(8)[0];
    assert_eq!(&frame[1..], "n".repeat(30).as_bytes());
    assert_eq!(mc.self_info().unwrap().name, "n".repeat(30));
}

#[tokio::test(start_paused = true)]
async fn has_connection_maps_not_found_to_false() {
    let answer = Arc::new(AtomicU32::new(0));
    let answer_ref = Arc::clone(&answer);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        28 if answer_ref.fetch_add(1, Ordering::SeqCst) == 0 => vec![ok()],
        28 => vec![err(2)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(mc.has_connection(&key()).await.unwrap());
    assert!(!mc.has_connection(&key()).await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn parallel_commands_get_their_own_replies() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        5 => vec![vec![9, 1, 0, 0, 0]],
        20 => vec![vec![12, 0xd4, 0x0d]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let (time, battery) = tokio::join!(mc.get_time(), mc.get_bat());
    assert_eq!(time.unwrap(), 1);
    assert_eq!(battery.unwrap().level, 3540);
    assert_eq!(mc.time(), 1);
}

#[tokio::test(start_paused = true)]
async fn reconnects_and_restarts_the_session() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.drop_link().await;
    let event = loop {
        if let event @ Event::Connected { .. } = events.recv().await.unwrap() {
            break event;
        }
    };
    assert!(matches!(
        event,
        Event::Connected {
            reconnected: true,
            ..
        }
    ));
    assert_eq!(device.opens(), 2);
    assert_eq!(device.commands_with(1).len(), 2);
    assert!(mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn lost_link_without_reconnect_reports_it() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.drop_link().await;
    let reason = loop {
        if let Event::Disconnected { reason } = events.recv().await.unwrap() {
            break reason;
        }
    };
    assert_eq!(reason, "test_disconnect");
    assert!(!mc.is_connected());
    assert!(matches!(mc.get_time().await, Err(Error::NotConnected)));
}

#[tokio::test(start_paused = true)]
async fn automatic_contact_updates_survive_disconnect_and_connect() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => {
            let mut end = vec![4];
            end.extend(5000u32.to_le_bytes());
            vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, KEY, "Alice", 0xff, &[]),
                end,
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.set_auto_update_contacts(true);
    mc.disconnect().await.unwrap();
    mc.connect().await.unwrap();
    assert!(mc.auto_update_contacts());
    // an advert after the new connect still makes the client read the contacts
    let mut advert = vec![0x80];
    advert.extend(KEY);
    device.push(advert).await;
    device.settle().await;
    assert_eq!(device.commands_with(4).len(), 1);
    assert_eq!(mc.contacts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn connect_while_reconnecting_leaves_one_link() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    device.drop_link().await;
    device.settle().await;
    // reconnecting waits a second before its first try; connect first
    mc.connect().await.unwrap();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(
        device.opens(),
        2,
        "the pending reconnect opened another link"
    );
    assert!(mc.is_connected());
    assert_eq!(mc.get_bat().await.err().map(|e| e.is_timeout()), Some(true));
}

#[tokio::test(start_paused = true)]
async fn disconnect_while_reconnecting_stays_disconnected() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    // The disconnect comes while the reconnect opens the link, so stopping
    // the reconnect takes effect only after the link is open (as on another
    // thread of a multi-thread runtime).
    device.during_next_open({
        let mc = mc.clone();
        async move { mc.disconnect().await.unwrap() }
    });
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(device.opens(), 2);
    assert!(!mc.is_connected(), "the reconnect kept its link");
    assert!(matches!(mc.get_time().await, Err(Error::NotConnected)));
    while let Some(event) = events.try_recv() {
        assert!(
            !matches!(event, Event::Connected { .. }),
            "reported {event:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn command_in_flight_fails_at_once_when_the_link_drops() {
    // Even while reconnecting: the reply went out on the old link and is lost.
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    let started = tokio::time::Instant::now();
    let pending = tokio::spawn({
        let mc = mc.clone();
        async move { mc.get_time().await }
    });
    device.settle().await;
    assert_eq!(device.commands_with(5).len(), 1);
    device.drop_link().await;
    assert!(matches!(pending.await.unwrap(), Err(Error::ConnectionLost)));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "waited {:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn remote_answers_still_arrive_after_a_reconnect() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |cmd| match cmd[0] {
        50 => vec![sent_frame(false, 0xbeef, 5000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    let pending = tokio::spawn({
        let mc = mc.clone();
        async move { mc.req_status_sync(&key(), RequestOptions::default()).await }
    });
    device.settle().await;
    device.drop_link().await;
    loop {
        if let Event::Connected {
            reconnected: true, ..
        } = events.recv().await.unwrap()
        {
            break;
        }
    }
    // the node's answer reaches the device after the app reconnected
    device
        .push(binary_response(0xbeef, &status_fields(4100)))
        .await;
    assert_eq!(pending.await.unwrap().unwrap().bat, 4100);
}

#[tokio::test(start_paused = true)]
async fn waiting_for_a_remote_answer_passes_over_errors_of_other_commands() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        50 => vec![sent_frame(false, 0xbeef, 5000)],
        5 => vec![err(2)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let pending = tokio::spawn({
        let mc = mc.clone();
        async move { mc.req_status_sync(&key(), RequestOptions::default()).await }
    });
    device.settle().await;
    // another command fails while the status request waits for the node
    assert!(matches!(
        mc.get_time().await,
        Err(Error::Device(ErrorCode::NotFound))
    ));
    device
        .push(binary_response(0xbeef, &status_fields(4100)))
        .await;
    assert_eq!(pending.await.unwrap().unwrap().bat, 4100);
}

/// A private message from `KEY` as the device hands it out.
fn private_message(text: &[u8]) -> Vec<u8> {
    let mut frame = vec![7];
    frame.extend(&KEY[..6]);
    frame.extend([0xff, 0]);
    frame.extend(9u32.to_le_bytes());
    frame.extend(text);
    frame
}

/// The text of the next private message among the events; fails when none
/// comes (on paused time the wait ends as soon as nothing else happens).
async fn next_message(events: &mut meshcore::Subscription) -> String {
    let message = async {
        loop {
            if let Event::ContactMessage(message) = events.recv().await.unwrap() {
                return message.text;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), message)
        .await
        .expect("no message was fetched")
}

/// A device whose message queue the test fills, answering
/// `CMD_SYNC_NEXT_MESSAGE` from it.
fn device_with_queue(
    options: Options,
) -> (
    meshcore::MeshCore,
    common::Device,
    Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
) {
    let queue = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let queue_ref = Arc::clone(&queue);
    let (mc, device) = client(options, move |cmd| match cmd[0] {
        10 => vec![queue_ref.lock().unwrap().pop().unwrap_or_else(|| vec![10])],
        _ => vec![],
    });
    (mc, device, queue)
}

#[tokio::test(start_paused = true)]
async fn auto_fetch_picks_up_waiting_messages() {
    let (mc, device, queue) = device_with_queue(Options::default());
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    mc.start_auto_message_fetching();
    queue.lock().unwrap().push(private_message(b"ping"));
    device.push(vec![0x83]).await;
    assert_eq!(next_message(&mut events).await, "ping");
    mc.stop_auto_message_fetching();
}

#[tokio::test(start_paused = true)]
async fn auto_fetch_goes_on_after_connecting_again() {
    let (mc, device, queue) = device_with_queue(Options::default());
    mc.connect().await.unwrap();
    mc.start_auto_message_fetching();
    device.settle().await;
    let mut events = mc.subscribe();
    // a message the device did not report, as one that came while no app was
    // connected: a new connect (here on a link that is up, as meshcore_py's
    // connect() allows) reads it
    queue.lock().unwrap().push(private_message(b"waiting"));
    mc.connect().await.unwrap();
    assert_eq!(next_message(&mut events).await, "waiting");
    // and the next MESSAGES_WAITING is still followed
    queue.lock().unwrap().push(private_message(b"after"));
    device.push(vec![0x83]).await;
    assert_eq!(next_message(&mut events).await, "after");
}

#[tokio::test(start_paused = true)]
async fn auto_fetch_reads_what_came_during_a_reconnect() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device, queue) = device_with_queue(options);
    mc.connect().await.unwrap();
    mc.start_auto_message_fetching();
    device.settle().await;
    let mut events = mc.subscribe();
    device.drop_link().await;
    // the message comes while the link is down: the device never reports it
    queue.lock().unwrap().push(private_message(b"missed"));
    let reconnected = async {
        loop {
            if let Event::Connected {
                reconnected: true, ..
            } = events.recv().await.unwrap()
            {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(60), reconnected)
        .await
        .expect("no reconnect");
    assert_eq!(next_message(&mut events).await, "missed");
    // the session was started anew before the queue was read
    let codes: Vec<u8> = device.commands().iter().map(|c| c[0]).collect();
    assert_eq!(codes[..4], [1, 10, 1, 10]);
}

#[tokio::test(start_paused = true)]
async fn cut_and_garbled_frames_never_stop_the_reader() {
    // One frame of every type (the parity fixtures), cut at every length and
    // with each byte replaced, plus random frames: none may panic the reader
    // or stop it.
    let frames: Vec<Vec<u8>> = include_str!("fixtures/frames.hex")
        .lines()
        .map(|line| meshcore::hex::decode(line).unwrap())
        .collect();
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        20 => vec![vec![12, 0xd4, 0x0d]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    for frame in &frames {
        for len in 1..frame.len() {
            device.push(frame[..len].to_vec()).await;
        }
        for at in 1..frame.len() {
            for value in [0x00, 0x7f, 0x80, 0xff] {
                let mut garbled = frame.clone();
                garbled[at] = value;
                device.push(garbled).await;
            }
        }
    }
    let mut seed = 0x2545_f491_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..5000 {
        let len = 1 + (next() % 200) as usize;
        let mut frame: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        // mostly known codes, so the parsers get the random payloads
        frame[0] = frames[(next() as usize) % frames.len()][0];
        device.push(frame).await;
    }
    // let the reader work through the queue before the next command
    device.settle().await;
    assert!(mc.is_connected());
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
}

#[tokio::test(start_paused = true)]
async fn unknown_frames_and_unreadable_pushes() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.push(vec![0x87, 0, 1]).await; // a status push cut short: dropped
    device.push(vec![0x7e, 1, 2]).await; // unknown code
    let event = events.recv().await.unwrap();
    assert!(matches!(event, Event::Unknown { code: 0x7e, .. }));
}

/// A flood group text packet as a companion sends it.
fn group_packet(secret: [u8; 16], timestamp: u32, text: &str, path: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    let mut plain = timestamp.to_le_bytes().to_vec();
    plain.push(0);
    plain.extend_from_slice(text.as_bytes());
    while !plain.len().is_multiple_of(16) {
        plain.push(0);
    }
    let cipher = aes::Aes128::new_from_slice(&secret).unwrap();
    let mut crypted = Vec::new();
    for chunk in plain.as_chunks::<16>().0 {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        crypted.extend_from_slice(&block);
    }
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&secret).unwrap();
    mac.update(&crypted);
    let tag = mac.finalize().into_bytes();
    let mut packet = vec![(5 << 2) | 1, u8::try_from(path.len()).unwrap()];
    packet.extend_from_slice(path);
    packet.push(Sha256::digest(secret)[0]);
    packet.extend_from_slice(&tag[..2]);
    packet.extend(crypted);
    packet
}

#[tokio::test(start_paused = true)]
async fn rx_log_channel_messages_get_path_and_signal() {
    let secret = meshcore::channel_secret_from_name("#public");
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        31 => {
            let mut f = vec![18, 0];
            let mut name = [0u8; 32];
            name[..7].copy_from_slice(b"#public");
            f.extend(name);
            f.extend(secret);
            vec![f]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.set_decrypt_channel_logs(true);
    assert_eq!(mc.get_channel(0).await.unwrap().channel_name, "#public");

    let mut events = mc.subscribe();
    // the RX log sees the packet: SNR 6.5, RSSI -80, two hops
    let mut log = vec![0x88, 26, (-80i8).cast_unsigned()];
    log.extend(group_packet(
        secret,
        1_700_000_000,
        "alice: hi",
        &[0xaa, 0xbb],
    ));
    device.push(log).await;
    let rx = loop {
        if let Event::RxLog(rx) = events.recv().await.unwrap() {
            break rx;
        }
    };
    let packet = rx.packet.as_ref().unwrap();
    assert_eq!(packet.payload_type, meshcore::PayloadType::GrpTxt);
    let meshcore::PacketContent::GroupText(group) = &packet.content else {
        panic!()
    };
    assert_eq!(group.chan_name.as_deref(), Some("#public"));
    assert_eq!(group.decrypted.as_ref().unwrap().message, "alice: hi");

    // then the device delivers the message: it gets the logged path and signal
    let mut msg = vec![17, 24, 0, 0, 0, 2, 0];
    msg.extend(1_700_000_000u32.to_le_bytes());
    msg.extend(b"alice: hi");
    device.push(msg).await;
    let message = loop {
        if let Event::ChannelMessage(m) = events.recv().await.unwrap() {
            break m;
        }
    };
    let rx_info = message.rx_info.as_ref().unwrap();
    assert_eq!(rx_info.path, vec![0xaa, 0xbb]);
    assert_eq!(rx_info.rssi, -80);
    assert_eq!(rx_info.snr, 6.5);
}

#[tokio::test(start_paused = true)]
async fn set_channel_keeps_the_secret_for_the_rx_log() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        32 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.set_channel(2, "#hamburg", None).await.unwrap();
    let mut events = mc.subscribe();
    let secret = meshcore::channel_secret_from_name("#hamburg");
    let mut log = vec![0x88, 20, (-70i8).cast_unsigned()];
    log.extend(group_packet(secret, 1_700_000_000, "bob: moin", &[]));
    device.push(log).await;
    let rx = loop {
        if let Event::RxLog(rx) = events.recv().await.unwrap() {
            break rx;
        }
    };
    let meshcore::PacketContent::GroupText(group) = &rx.packet.as_ref().unwrap().content else {
        panic!()
    };
    assert_eq!(group.chan_name.as_deref(), Some("#hamburg"));
    assert!(mc.keep_channel_secrets());
    mc.set_keep_channel_secrets(false);
    assert!(!mc.keep_channel_secrets());
}

#[test]
fn trace_path_strings() {
    let (path, flags) = meshcore::parse_trace_path("23,5f").unwrap();
    assert_eq!((path, flags), (vec![0x23, 0x5f], 0));
}
