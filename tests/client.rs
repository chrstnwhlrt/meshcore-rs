//! End-to-end tests of the client against a simulated device.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use common::{
    KEY, Random, ack_frame, binary_response, client, client_answering_app_start, client_on_device,
    contact_frame, err, ok, poll_once, self_info_frame, sent_frame, status_fields,
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
async fn a_disconnect_cut_short_still_closes_the_link() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    device.slow_close(Duration::from_secs(2));
    let cut_short = tokio::time::timeout(Duration::from_millis(500), mc.disconnect()).await;
    assert!(cut_short.is_err());
    assert_eq!(device.closes(), 0);
    // a connect waits until it is closed
    mc.connect().await.unwrap();
    assert_eq!((device.closes(), device.opens()), (1, 2));
}

#[tokio::test(start_paused = true)]
async fn a_lost_link_is_reported_before_it_is_closed() {
    // closing it may take a while, as over BLE
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.hang_on_close();
    device.drop_link().await;
    device.settle().await;
    assert!(!mc.is_connected());
    assert!(matches!(
        events.try_recv(),
        Some(Event::Disconnected { reason }) if reason == "test_disconnect"
    ));
}

#[tokio::test(start_paused = true)]
async fn a_new_link_stays_up_when_the_lost_one_closes_late() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    // the lost link takes 7 s to close, a connect waits 5 s for it
    device.slow_close(Duration::from_secs(7));
    device.drop_link().await;
    device.settle().await;
    let started = tokio::time::Instant::now();
    mc.connect().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(6));
    let mut events = mc.subscribe();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(mc.is_connected());
    assert!(
        events.try_recv().is_none(),
        "the new link was reported gone"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnects_although_the_lost_link_does_not_close() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    device.hang_on_close();
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert!(mc.is_connected());
    assert_eq!(device.opens(), 2);
}

#[tokio::test(start_paused = true)]
async fn disconnect_waits_until_a_lost_link_is_closed() {
    // so the device is free for other apps afterwards
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    device.slow_close(Duration::from_secs(2));
    device.drop_link().await;
    device.settle().await;
    let started = tokio::time::Instant::now();
    mc.disconnect().await.unwrap();
    assert!(started.elapsed() > Duration::from_secs(1));
}

#[tokio::test(start_paused = true)]
async fn a_write_that_hangs_holds_up_neither_disconnect_nor_the_report_of_a_lost_link() {
    for lost in [false, true] {
        let (mc, device) = client(Options::default(), |_| vec![]);
        mc.connect().await.unwrap();
        let mut events = mc.subscribe();
        device.hang_on_write();
        let pending = tokio::spawn({
            let mc = mc.clone();
            async move { mc.get_time().await }
        });
        device.settle().await;
        if lost {
            device.drop_link().await;
            device.settle().await;
        } else {
            tokio::time::timeout(Duration::from_secs(1), mc.disconnect())
                .await
                .expect("disconnect waited for the write")
                .unwrap();
        }
        assert!(!mc.is_connected());
        assert!(matches!(
            events.try_recv(),
            Some(Event::Disconnected { .. })
        ));
        // and the command that was being written ends
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), pending).await,
            Ok(Ok(Err(Error::ConnectionLost)))
        ));
    }
}

#[tokio::test(start_paused = true)]
async fn disconnect_while_connecting_stays_disconnected() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    let mut events = mc.subscribe();
    device.during_next_open({
        let mc = mc.clone();
        async move { mc.disconnect().await.unwrap() }
    });
    assert!(matches!(mc.connect().await, Err(Error::NotConnected)));
    assert!(!mc.is_connected());
    assert!(events.try_recv().is_none(), "reported a link it gave up");
}

#[tokio::test(start_paused = true)]
async fn a_link_opened_too_late_is_closed_again() {
    // the disconnect comes in right before the link is up
    let (mc, device) = client(Options::default(), |_| vec![]);
    let mut events = mc.subscribe();
    device.during_next_open({
        let mc = mc.clone();
        async move {
            let mut disconnect = Box::pin(async move { mc.disconnect().await });
            let _ = poll_once(&mut disconnect).await;
        }
    });
    assert!(matches!(mc.connect().await, Err(Error::NotConnected)));
    device.settle().await;
    assert_eq!((device.opens(), device.closes()), (1, 1));
    assert!(!mc.is_connected());
    assert!(events.try_recv().is_none(), "reported a link it gave up");
}

#[tokio::test(start_paused = true)]
async fn of_two_connects_at_once_the_later_one_wins() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        20 => vec![vec![12, 0xd4, 0x0d]],
        _ => vec![],
    });
    // a serial port is open from the start of an open on
    device.one_link_at_a_time();
    let mut events = mc.subscribe();
    device.during_next_open(tokio::time::sleep(Duration::from_secs(60)));
    let first = tokio::spawn({
        let mc = mc.clone();
        async move { mc.connect().await }
    });
    device.settle().await;
    let started = tokio::time::Instant::now();
    mc.connect().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(first.await.unwrap(), Err(Error::NotConnected)));
    assert!(mc.is_connected());
    device.settle().await;
    // one link, reported once and still up
    let reported: Vec<EventKind> = std::iter::from_fn(|| events.try_recv())
        .map(|event| event.kind())
        .filter(|kind| matches!(kind, EventKind::Connected | EventKind::Disconnected))
        .collect();
    assert_eq!(reported, [EventKind::Connected]);
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
}

#[tokio::test(start_paused = true)]
async fn a_connect_takes_over_from_a_reconnect() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    device.one_link_at_a_time();
    mc.connect().await.unwrap();
    device.during_next_open(tokio::time::sleep(Duration::from_secs(60)));
    device.drop_link().await;
    // the reconnect is opening the link
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(device.in_use());
    mc.connect().await.unwrap();
    assert!(mc.is_connected());
    assert_eq!(device.opens(), 2);
}

#[tokio::test(start_paused = true)]
async fn disconnect_lets_go_of_a_link_that_is_being_opened() {
    for reconnect in [false, true] {
        let options = Options {
            auto_reconnect: true,
            ..Options::default()
        };
        let (mc, device) = client(options, |_| vec![]);
        device.one_link_at_a_time();
        if reconnect {
            mc.connect().await.unwrap();
        }
        device.during_next_open(tokio::time::sleep(Duration::from_secs(60)));
        if reconnect {
            device.drop_link().await;
        } else {
            let mc = mc.clone();
            tokio::spawn(async move { mc.connect().await });
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(device.in_use());
        mc.disconnect().await.unwrap();
        assert!(!device.in_use());
    }
}

#[tokio::test(start_paused = true)]
async fn a_connect_that_is_not_polled_holds_nothing_up() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    device.one_link_at_a_time();
    device.during_next_open(tokio::time::sleep(Duration::from_secs(60)));
    let mut first = Box::pin(async { mc.connect().await });
    assert!(!poll_once(&mut first).await);
    device.settle().await;
    let started = tokio::time::Instant::now();
    mc.connect().await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(matches!(first.await, Err(Error::NotConnected)));
    assert!(mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn a_connect_given_up_lets_go_of_the_device() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    device.one_link_at_a_time();
    device.during_next_open(tokio::time::sleep(Duration::from_secs(60)));
    let given_up = tokio::time::timeout(Duration::from_secs(1), mc.connect()).await;
    assert!(given_up.is_err());
    device.settle().await;
    assert!(!device.in_use());
    assert!(!mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn connect_waits_5_seconds_for_the_link_it_replaces() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    device.hang_on_close();
    let started = tokio::time::Instant::now();
    mc.connect().await.unwrap();
    assert_eq!(started.elapsed().as_secs(), 5);
    assert!(mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn a_command_that_is_not_polled_does_not_hold_up_disconnect() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    device.hang_on_write();
    let mut command = Box::pin(async { mc.get_time().await });
    assert!(!poll_once(&mut command).await);
    device.settle().await;
    tokio::time::timeout(Duration::from_secs(1), mc.disconnect())
        .await
        .expect("disconnect waited for the command")
        .unwrap();
    assert!(matches!(command.await, Err(Error::ConnectionLost)));
}

#[tokio::test(start_paused = true)]
async fn a_frame_is_written_to_its_end_when_its_command_is_given_up() {
    // BlueZ takes one write at a time, and half a frame over serial or TCP
    // would be filled with the next one's bytes
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        20 => vec![vec![12, 0xd4, 0x0d]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    device.slow_write(Duration::from_millis(100));
    let given_up = tokio::time::timeout(Duration::from_millis(50), mc.get_time()).await;
    assert!(given_up.is_err());
    // one that is given up before its turn is not sent
    let given_up = tokio::time::timeout(Duration::from_millis(20), mc.get_tuning()).await;
    assert!(given_up.is_err());
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
    let sent: Vec<u8> = device.commands().iter().map(|c| c[0]).collect();
    assert_eq!(sent, [1, 5, 20]);
}

#[tokio::test(start_paused = true)]
async fn a_command_that_is_not_sent_changes_nothing() {
    // given up while its frame waits behind one that is being written, or its
    // link drops meanwhile
    for lost in [false, true] {
        let (mc, device) = client(Options::default(), one_contact);
        let alice = key();
        mc.connect().await.unwrap();
        mc.get_contacts(0).await.unwrap();
        let token = mc.request_factory_reset();
        device.slow_write(Duration::from_secs(1));
        let time = tokio::time::timeout(Duration::from_millis(50), mc.get_time());
        assert!(time.await.is_err());
        let path = tokio::time::timeout(Duration::from_millis(50), mc.reset_path(&alice));
        assert!(path.await.is_err());
        let reset = tokio::time::timeout(Duration::from_millis(50), async {
            if lost {
                device.drop_link().await;
            }
            mc.confirm_factory_reset(&token).await
        });
        assert!(!matches!(reset.await, Ok(Ok(()))));
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(mc.self_info().is_some() && mc.contact(&alice).is_some());
        assert!(!mc.contacts_dirty());
        assert_eq!(device.commands_with(13), Vec::<Vec<u8>>::new());
        assert_eq!(device.commands_with(51), Vec::<Vec<u8>>::new());
    }
}

#[tokio::test(start_paused = true)]
async fn a_connect_without_an_answer_leaves_a_later_one_alone() {
    // APP_START is answered only on the second link
    let answered = Arc::new(AtomicU32::new(0));
    let (mc, device) = {
        let answered = Arc::clone(&answered);
        client_answering_app_start(move || answered.fetch_add(1, Ordering::SeqCst) > 0)
    };
    let first = tokio::spawn({
        let mc = mc.clone();
        async move { mc.connect().await }
    });
    device.settle().await;
    mc.connect().await.unwrap();
    assert!(matches!(first.await.unwrap(), Err(Error::ConnectionLost)));
    assert!(mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn disconnect_while_reconnecting_reports_it() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.drop_link().await;
    device.settle().await;
    assert!(events.try_recv().is_none(), "reported while reconnecting");
    mc.disconnect().await.unwrap();
    assert!(matches!(
        events.try_recv(),
        Some(Event::Disconnected { reason }) if reason == "manual_disconnect"
    ));
}

#[tokio::test(start_paused = true)]
async fn subscribers_see_the_end_of_a_client_dropped_while_its_link_closes() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    device.hang_on_close();
    device.drop_link().await;
    device.settle().await;
    drop(mc);
    assert!(matches!(
        events.recv().await,
        Some(Event::Disconnected { .. })
    ));
    let end = tokio::time::timeout(Duration::from_secs(1), events.recv()).await;
    assert!(matches!(end, Ok(None)), "{end:?}");
}

#[tokio::test(start_paused = true)]
async fn dropping_the_client_lets_go_of_the_link_while_its_tasks_wait() {
    // the device answers neither the message fetching nor the contact update
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    mc.start_auto_message_fetching();
    mc.set_auto_update_contacts(true);
    let mut advert = vec![0x80];
    advert.extend(KEY);
    device.settle().await;
    device.push(advert).await;
    device.settle().await;
    assert_eq!(device.commands_with(10).len(), 1);
    let clone = mc.clone();
    drop(mc);
    device.settle().await;
    assert!(device.has_client(), "a clone was left");
    drop(clone);
    device.settle().await;
    assert!(!device.has_client());
}

#[tokio::test(start_paused = true)]
async fn dropping_the_client_stops_a_reconnect() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    mc.connect().await.unwrap();
    device.during_next_open(tokio::time::sleep(Duration::from_secs(5)));
    device.drop_link().await;
    // the reconnect is opening the link
    tokio::time::sleep(Duration::from_secs(3)).await;
    drop(mc);
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(device.opens(), 1, "the reconnect went on");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_frame_of_a_link_counts_once_it_is_taken_down() {
    let options = Options {
        event_capacity: 1 << 18,
        ..Options::default()
    };
    let (mc, device) = client(options, |_| vec![]);
    // adverts come in all the time, on other threads
    let flood = tokio::spawn({
        let device = device.clone();
        async move {
            let advert = [&[0x80u8][..], &KEY].concat();
            loop {
                for _ in 0..200 {
                    device.try_push(advert.clone());
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    });
    for _ in 0..500 {
        mc.connect().await.unwrap();
        tokio::time::sleep(Duration::from_micros(100)).await;
        let mut events = mc.subscribe();
        mc.disconnect().await.unwrap();
        tokio::time::sleep(Duration::from_micros(100)).await;
        let mut gone = false;
        while let Some(event) = events.try_recv() {
            assert!(!gone, "{event:?} after the link was reported gone");
            gone = matches!(event, Event::Disconnected { .. });
        }
        assert!(gone);
    }
    flood.abort();
}

/// One thing of many that come in at any time.
async fn something_random(mc: &meshcore::MeshCore, device: &common::Device, random: &mut Random) {
    match random.below(10) {
        0 => {
            // the device is free once its turn comes
            let result = mc.connect().await;
            assert!(!matches!(result, Err(Error::Io(_))), "{result:?}");
        }
        1 => mc.disconnect().await.unwrap(),
        2 => {
            let _ = tokio::time::timeout(random.millis(20), mc.connect()).await;
        }
        3 => {
            let _ = tokio::time::timeout(random.millis(20), mc.disconnect()).await;
        }
        4 => device.try_drop_link(),
        5 => device.slow_close(random.millis(400)),
        6 => device.during_next_open(tokio::time::sleep(random.millis(300))),
        7 => device.slow_write(random.millis(10)),
        8 => {
            let _ = tokio::time::timeout(random.millis(30), mc.get_bat()).await;
        }
        _ => {
            let _ = mc.get_bat().await;
        }
    }
    tokio::time::sleep(random.millis(30)).await;
}

#[tokio::test(start_paused = true)]
async fn the_connection_takes_whatever_comes_in() {
    for seed in 1..=50 {
        let options = Options {
            auto_reconnect: true,
            default_timeout: Duration::from_millis(300),
            ..Options::default()
        };
        let (mc, device) = client(options, |cmd| match cmd[0] {
            20 => vec![vec![12, 0xd4, 0x0d]],
            _ => vec![],
        });
        device.one_link_at_a_time();
        let mut events = mc.subscribe();
        let tasks: Vec<_> = (0..5)
            .map(|task| {
                let (mc, device) = (mc.clone(), device.clone());
                let mut random = Random::new(seed * 100 + task);
                tokio::spawn(async move {
                    for _ in 0..40 {
                        something_random(&mc, &device, &mut random).await;
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        // at the end it is disconnected, and was reported so each time
        mc.disconnect().await.unwrap();
        assert!(!mc.is_connected() && !device.in_use(), "seed {seed}");
        let mut up = false;
        while let Some(event) = events.try_recv() {
            match event {
                // a reconnect reports a link whose loss it did not report
                Event::Connected { reconnected, .. } => {
                    assert!(reconnected == up, "seed {seed}");
                    up = true;
                }
                Event::Disconnected { .. } => assert!(std::mem::take(&mut up), "seed {seed}"),
                _ => {}
            }
        }
        assert!(!up, "seed {seed}");
        // and it still works
        device.slow_write(Duration::ZERO);
        mc.connect().await.unwrap();
        assert_eq!(mc.get_bat().await.unwrap().level, 3540);
        drop(mc);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(!device.in_use(), "seed {seed}");
    }
}

#[tokio::test(start_paused = true)]
async fn connect_fails_without_an_answer() {
    let mc = silent_client();
    assert!(matches!(mc.connect().await, Err(Error::Timeout)));
    assert!(!mc.is_connected());
    // after 2 s, as in meshcore_py, and takes the link down again
    // (meshcore_py's connect leaves it up)
    let (mc, device) = client_answering_app_start(|| false);
    let started = tokio::time::Instant::now();
    assert!(matches!(mc.connect().await, Err(Error::Timeout)));
    assert_eq!(started.elapsed().as_secs(), 2);
    assert_eq!((device.opens(), device.closes()), (1, 1));
    assert!(!device.in_use());
}

#[tokio::test(start_paused = true)]
async fn commands_wait_15_seconds_for_their_reply() {
    // as in meshcore_py
    let (mc, _device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let started = tokio::time::Instant::now();
    assert!(matches!(mc.get_time().await, Err(Error::Timeout)));
    assert_eq!(started.elapsed().as_secs(), 15);
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

/// A device that is gone once its first link drops: it cannot be opened
/// again.
struct Vanishing {
    opens: Arc<AtomicU32>,
    link: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::Sender<meshcore::LinkEvent>>>>,
}

/// Answers `APP_START`, swallows the rest.
struct AppStartAnswer(tokio::sync::mpsc::Sender<meshcore::LinkEvent>);

impl meshcore::Transport for Vanishing {
    fn open(&self) -> meshcore::transport::BoxFuture<'_, meshcore::Result<meshcore::Link>> {
        Box::pin(async {
            if self.opens.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound).into());
            }
            let (sender, events) = tokio::sync::mpsc::channel(4);
            *self.link.lock().unwrap() = Some(sender.clone());
            Ok(meshcore::Link {
                info: "vanishing".into(),
                events,
                writer: Box::new(AppStartAnswer(sender)),
            })
        })
    }

    fn describe(&self) -> String {
        "vanishing".into()
    }
}

impl meshcore::FrameWriter for AppStartAnswer {
    fn write_frame<'a>(
        &'a mut self,
        frame: &'a [u8],
    ) -> meshcore::transport::BoxFuture<'a, meshcore::Result<()>> {
        Box::pin(async move {
            if frame.first() == Some(&1) {
                let info = meshcore::LinkEvent::Frame(self_info_frame("Node"));
                let _ = self.0.send(info).await;
            }
            Ok(())
        })
    }

    fn close(&mut self) -> meshcore::transport::BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[tokio::test(start_paused = true)]
async fn reconnecting_gives_up_after_its_attempts() {
    // three, a second apart, as in meshcore_py
    let opens = Arc::new(AtomicU32::new(0));
    let link = Arc::default();
    let transport = Vanishing {
        opens: Arc::clone(&opens),
        link: Arc::clone(&link),
    };
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let mc = meshcore::MeshCore::new(transport, options);
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    let started = tokio::time::Instant::now();
    let device = link.lock().unwrap().take().unwrap();
    device
        .send(meshcore::LinkEvent::Closed("gone".into()))
        .await
        .unwrap();
    let reason = loop {
        if let Event::Disconnected { reason } = events.recv().await.unwrap() {
            break reason;
        }
    };
    assert_eq!(reason, "reconnect_failed");
    assert_eq!(started.elapsed().as_secs(), 3);
    assert_eq!(opens.load(Ordering::SeqCst), 4);
    assert!(!mc.is_connected());
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
async fn a_command_the_link_drops_with_counts_as_sent() {
    // over BLE the device restarts before it confirms the write
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    device.drop_link_on_write();
    mc.reboot().await.unwrap();
    device.settle().await;
    assert!(!mc.is_connected());

    mc.connect().await.unwrap();
    let token = mc.request_factory_reset();
    device.drop_link_on_write();
    mc.confirm_factory_reset(&token).await.unwrap();
    assert_eq!(mc.self_info(), None);

    // one that waits for a reply gets none
    mc.connect().await.unwrap();
    device.drop_link_on_write();
    assert!(matches!(mc.get_bat().await, Err(Error::ConnectionLost)));
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
            // the device holds two contacts, later only Rep
            let count: u32 = if round == 0 { 2 } else { 1 };
            let mut frames = vec![[&[2u8][..], &count.to_le_bytes()].concat()];
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
    // case is ignored, anything else is matched as given (as meshcore_py)
    let ab = PublicKey::new([0xab; 32]);
    mc.cache_contact(Contact::new(ab, ContactType::Chat, "Ab"));
    let found = |prefix: &str| mc.get_contact_by_key_prefix(prefix).map(|c| c.public_key);
    assert_eq!(found("ABab"), Some(ab));
    assert_eq!(found(" abab"), None);
    assert_eq!(found(""), None);
    mc.uncache_contact(&ab);
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
async fn a_small_event_capacity_leaves_the_commands_alone() {
    // a long list at once, as over TCP
    let options = Options {
        event_capacity: 1,
        ..Options::default()
    };
    let (mc, _device) = client(options, |cmd| match cmd[0] {
        4 => {
            let mut frames = vec![vec![2, 94, 1, 0, 0]];
            frames.extend((0..350u16).map(|n| {
                let mut key = KEY;
                key[..2].copy_from_slice(&n.to_le_bytes());
                contact_frame(3, key, "Node", 0xff, &[])
            }));
            frames.push(vec![4, 0x88, 0x13, 0, 0]);
            frames
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert_eq!(mc.get_contacts(0).await.unwrap().contacts.len(), 350);
}

#[tokio::test(start_paused = true)]
async fn incremental_contacts_send_since_and_merge() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => {
            let mut end = vec![4];
            end.extend(6000u32.to_le_bytes());
            vec![
                vec![2, 2, 0, 0, 0],
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
    // and returned so by the changes
    let flagged = mc.change_contact_flags(&renamed, 2).await.unwrap();
    assert_eq!(flagged.name, "r".repeat(31));

    // a new contact without a type is a transient entry of the device
    let untyped = PublicKey::new([0x44; 32]);
    mc.add_contact(&Contact::new(untyped, ContactType::None, "x"))
        .await
        .unwrap();
    assert!(mc.contact(&untyped).is_none());
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
    // hash mode 3 (4-byte hashes) is reserved, and that is the reason also
    // for a path that would be no whole number of such hops
    assert!(refused(
        mc.change_contact_path(&contact, &[1, 2, 3, 4], Some(3))
            .await
            .map(drop)
    ));
    let reason = mc.change_contact_path(&contact, &[1, 2], Some(3)).await;
    assert!(
        matches!(&reason, Err(Error::InvalidArgument(reason)) if reason == "hash mode 3, at most 2"),
        "{reason:?}"
    );
    // a length byte announcing 2 hops for a path of 1 byte
    let mut mismatched = contact.clone();
    mismatched.out_path_len = PathLen::new(2, 0).unwrap();
    mismatched.out_path = vec![1];
    assert!(refused(mc.update_contact(&mismatched, None).await));
    // a path for a contact that floods, which the device would not hand back
    let mut flooded = contact.clone();
    flooded.out_path = vec![1];
    assert!(refused(mc.update_contact(&flooded, None).await));
    // a path that matches its length byte (33 hops of 2 bytes) but is longer
    // than the 64 bytes the device stores
    let mut long = contact.clone();
    long.out_path_len = PathLen::from_byte(0x40 | 0x21);
    long.out_path = vec![1; 66];
    assert!(refused(mc.update_contact(&long, None).await));
    assert_eq!(device.commands_with(9).len(), 0);

    assert!(refused(
        mc.send_channel_data(1, 1, &[1], Some(&[1, 2, 3]), 1).await
    ));
    assert!(refused(mc.send_raw_data(&[1, 2, 3, 4], &[7; 64], 0).await));
    let reserved = |result: meshcore::Result<()>| matches!(&result, Err(Error::InvalidArgument(reason)) if reason == "hash mode 3, at most 2");
    for path in [&[1, 2, 3, 4][..], &[1, 2]] {
        assert!(reserved(
            mc.send_channel_data(1, 1, &[1], Some(path), 3).await
        ));
        assert!(reserved(mc.send_raw_data(&[1, 2, 3, 4], path, 3).await));
    }
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
async fn an_ack_that_comes_while_the_last_attempt_fails_counts() {
    let attempts = AtomicU32::new(0);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        2 if attempts.fetch_add(1, Ordering::SeqCst) == 0 => vec![sent_frame(false, 7, 500)],
        2 => vec![ack_frame(7), err(3)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let twice = RetryOptions {
        max_attempts: 2,
        ..RetryOptions::default()
    };
    let sent = mc.send_msg_with_retry(&key(), "hi", twice).await.unwrap();
    assert_eq!(sent.map(|sent| sent.expected_ack), Some(7));
    assert_eq!(device.commands_with(2).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_ack_that_comes_between_attempts_ends_them() {
    // while an attempt fails, or while the path is reset
    for failing in [true, false] {
        let attempts = AtomicU32::new(0);
        let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
            2 if failing && attempts.fetch_add(1, Ordering::SeqCst) == 1 => {
                vec![ack_frame(7), err(3)]
            }
            2 => vec![sent_frame(false, 7, 500)],
            13 => vec![ack_frame(7), ok()],
            _ => vec![],
        });
        mc.connect().await.unwrap();
        let sent = mc
            .send_msg_with_retry(&key(), "hi", RetryOptions::default())
            .await
            .unwrap();
        assert_eq!(sent.map(|sent| sent.expected_ack), Some(7));
        assert_eq!(device.commands_with(2).len(), 2);
        assert_eq!(device.commands_with(13).len(), usize::from(!failing));
    }
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
async fn retries_to_a_cached_prefix_take_its_key_from_the_cache() {
    // as in meshcore_py, no contact list is read for it
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x300, 500)],
        13 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.cache_contact(Contact::new(key(), ContactType::Chat, "Alice"));
    let prefix: [u8; 6] = KEY[..6].try_into().unwrap();
    let started = tokio::time::Instant::now();
    let result = mc
        .send_msg_with_retry(prefix, "hi", RetryOptions::default())
        .await
        .unwrap();
    assert_eq!(result, None);
    assert_eq!(device.commands_with(4).len(), 0);
    assert_eq!(device.commands_with(13), [[&[13u8][..], &KEY].concat()]);
    assert_eq!(device.commands_with(2).len(), 3);
    // each attempt waits the device's suggestion (500 ms) and a fifth, as in
    // meshcore_py
    assert_eq!(started.elapsed(), Duration::from_millis(1800));
}

#[tokio::test(start_paused = true)]
async fn cli_commands_take_the_text_type_of_their_destination() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        2 => vec![sent_frame(false, 0x100, 500)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let companion = PublicKey::new([0x44; 32]);
    mc.cache_contact(Contact::new(companion, ContactType::Chat, "Bob"));
    let room = Contact::new(PublicKey::new([0x55; 32]), ContactType::Room, "Room");
    // the type given, the destination contact's, the cached contact's (by its
    // key or a prefix) and a repeater's for a key the cache lacks; meshcore_py
    // takes a repeater for any bare key
    mc.send_cmd(&room, "ver", Some(7), Some(ContactType::Chat))
        .await
        .unwrap();
    mc.send_cmd(&room, "ver", Some(7), None).await.unwrap();
    mc.send_cmd(companion, "ver", Some(7), None).await.unwrap();
    mc.send_cmd([0x44u8; 6], "ver", Some(7), None)
        .await
        .unwrap();
    mc.send_cmd(key(), "ver", Some(7), None).await.unwrap();
    let frames = device.commands_with(2);
    let types: Vec<u8> = frames.iter().map(|frame| frame[1]).collect();
    // TXT_TYPE_CLI_COMMAND for companions, TXT_TYPE_CLI_DATA for the others
    assert_eq!(types, [3, 1, 3, 3, 1]);
    // type, attempt, timestamp, key prefix, command
    assert_eq!(
        frames[4],
        [&[2u8, 1, 0][..], &7u32.to_le_bytes(), &KEY[..6], b"ver"].concat()
    );
}

#[tokio::test(start_paused = true)]
async fn a_path_without_hash_mode_fails_when_the_device_query_fails() {
    // meshcore_py takes mode 0 then
    let (mc, device) = client_on_device(Options::default(), |cmd| match cmd[0] {
        1 => vec![self_info_frame("Node")],
        22 => vec![err(1)],
        _ => vec![ok()],
    });
    mc.connect().await.unwrap();
    assert!(matches!(
        mc.get_path_hash_mode().await,
        Err(Error::Device(ErrorCode::UnsupportedCmd))
    ));
    let contact = Contact::new(key(), ContactType::Repeater, "Rep");
    assert!(matches!(
        mc.change_contact_path(&contact, &[1, 2], None).await,
        Err(Error::Device(ErrorCode::UnsupportedCmd))
    ));
    assert_eq!(device.commands_with(9).len(), 0);
    // with the mode given, no query is needed
    mc.change_contact_path(&contact, &[1, 2], Some(1))
        .await
        .unwrap();
    assert_eq!(device.commands_with(22).len(), 2);
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
async fn neighbour_pages_end_at_an_empty_page_or_a_second_failure() {
    // The repeater counts 6 neighbours: two at offset 0, one at offset 2 on
    // the second try, none at offset 3 (meshcore_py asks again forever) or
    // no answer there.
    for empty in [true, false] {
        let offsets = Arc::new(std::sync::Mutex::new(Vec::new()));
        let asked = Arc::clone(&offsets);
        let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
            50 => {
                let offset = u16::from_le_bytes([cmd[36], cmd[37]]);
                let mut asked = asked.lock().unwrap();
                asked.push(offset);
                let tag = 0x500 + u32::try_from(asked.len()).unwrap();
                let tries = asked.iter().filter(|&&at| at == offset).count();
                let count: i16 = match (offset, tries) {
                    (0, _) => 2,
                    (2, 1) => return vec![sent_frame(false, tag, 1000)],
                    (2, _) => 1,
                    _ if empty => 0,
                    _ => return vec![sent_frame(false, tag, 1000)],
                };
                let mut answer = 6i16.to_le_bytes().to_vec();
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
        let all = mc
            .fetch_all_neighbours(&key(), 0, 4, RequestOptions::default())
            .await
            .unwrap();
        assert_eq!((all.neighbours_count, all.results_count), (6, 3));
        assert_eq!(all.neighbours.len(), 3);
        // a failed page is asked for once more, an empty one is not
        let expected: &[u16] = if empty {
            &[0, 2, 2, 3]
        } else {
            &[0, 2, 2, 3, 3]
        };
        assert_eq!(*offsets.lock().unwrap(), expected);
    }
}

#[tokio::test(start_paused = true)]
async fn anonymous_request_to_an_unknown_node_asks_for_a_zero_hop_reply() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![err(2)],
        57 => {
            let mut answer = 7u32.to_le_bytes().to_vec();
            answer.extend(b"hamburg,de\0\0");
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
    assert_eq!(regions, "hamburg,de");
    let frame = &device.commands_with(57)[0];
    assert_eq!(frame[1..33], KEY);
    assert_eq!(frame[33..], [1, 0]);
}

#[tokio::test(start_paused = true)]
async fn anonymous_request_takes_the_reply_path_from_the_devices_record() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![contact_frame(
            3,
            KEY,
            "Rep",
            0x42,
            &[0xa1, 0xa2, 0xb1, 0xb2],
        )],
        57 => vec![sent_frame(false, 0x79, 1000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // the cache lacks the contact, then holds an older path
    for _ in 0..2 {
        let sent = mc
            .send_anon_req(
                &key(),
                meshcore::AnonReqType::Regions,
                RequestOptions::default(),
            )
            .await
            .unwrap();
        // the answer comes back over both hops
        assert_eq!(sent.suggested_timeout, 3000);
        let frame = device.commands_with(57).last().unwrap().clone();
        assert_eq!(frame[33..], [1, 0x42, 0xb1, 0xb2, 0xa1, 0xa2]);
        let mut older = mc.contact(&key()).unwrap();
        older.out_path_len = PathLen::new(1, 0).unwrap();
        older.out_path = vec![0xcc];
        mc.cache_contact(older);
    }
    // the record was read for each request
    assert_eq!(device.commands_with(30).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn anonymous_requests_of_every_kind() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![err(2)],
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
async fn an_anonymous_request_given_up_halfway_still_resets_the_path() {
    // the device never confirms the request, so a timeout around it drops it
    // while the contact is zero-hop
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![contact_frame(3, KEY, "Rep", 0xff, &[])],
        9 | 13 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.cache_contact(Contact::new(key(), ContactType::Repeater, "Rep"));
    let key = key();
    let request = mc.send_anon_req(
        &key,
        meshcore::AnonReqType::Owner,
        RequestOptions::default(),
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .is_err()
    );
    device.settle().await;
    let codes: Vec<u8> = device
        .commands()
        .iter()
        .map(|c| c[0])
        .filter(|&c| c != 1)
        .collect();
    assert_eq!(codes, vec![30, 22, 9, 57, 13]);
    assert!(mc.contact(&key).unwrap().is_flood());
}

#[tokio::test(start_paused = true)]
async fn a_zero_hop_change_without_an_answer_is_set_back_to_flood() {
    // the device may have taken the change whose answer never came, or could
    // not be read
    for unreadable in [false, true] {
        let options = Options {
            default_timeout: Duration::from_secs(1),
            ..Options::default()
        };
        let (mc, device) = client(options, move |cmd| match cmd[0] {
            30 => vec![contact_frame(3, KEY, "Rep", 0xff, &[])],
            9 if unreadable => vec![vec![24]],
            13 => vec![ok()],
            _ => vec![],
        });
        mc.connect().await.unwrap();
        mc.cache_contact(Contact::new(key(), ContactType::Repeater, "Rep"));
        let result = mc
            .send_anon_req(
                &key(),
                meshcore::AnonReqType::Owner,
                RequestOptions::default(),
            )
            .await;
        if unreadable {
            assert!(matches!(result, Err(Error::Protocol(_))));
        } else {
            assert!(matches!(result, Err(Error::Timeout)));
        }
        let codes: Vec<u8> = device
            .commands()
            .iter()
            .map(|c| c[0])
            .filter(|&c| c != 1)
            .collect();
        // read the device's record, query the hash mode, set zero-hop, reset
        assert_eq!(codes, vec![30, 22, 9, 13]);
        assert!(mc.contact(&key()).unwrap().is_flood());
    }
}

#[tokio::test(start_paused = true)]
async fn a_zero_hop_path_takes_mode_0_when_the_device_does_not_tell_its_own() {
    // zero hops have no size a mode could give, so the request goes on, as in
    // meshcore_py; the hash mode query gets no answer, or is refused
    for refused in [false, true] {
        let options = Options {
            default_timeout: Duration::from_secs(1),
            ..Options::default()
        };
        let (mc, device) = client_on_device(options, move |cmd| match cmd[0] {
            1 => vec![self_info_frame("Node")],
            22 if refused => vec![err(1)],
            30 => vec![contact_frame(3, KEY, "Rep", 0xff, &[])],
            9 | 13 => vec![ok()],
            57 => vec![sent_frame(false, 0x78, 1000)],
            _ => vec![],
        });
        mc.connect().await.unwrap();
        mc.cache_contact(Contact::new(key(), ContactType::Repeater, "Rep"));
        mc.send_anon_req(
            &key(),
            meshcore::AnonReqType::Owner,
            RequestOptions::default(),
        )
        .await
        .unwrap();
        let codes: Vec<u8> = device
            .commands()
            .iter()
            .map(|c| c[0])
            .filter(|&c| c != 1)
            .collect();
        assert_eq!(codes, vec![30, 22, 9, 57, 13]);
        // zero hops of 1-byte hashes
        assert_eq!(device.commands_with(9)[0][35], 0);
        assert!(mc.contact(&key()).unwrap().is_flood());
    }
}

#[tokio::test(start_paused = true)]
async fn login_success_and_failure() {
    let fail = Arc::new(AtomicBool::new(false));
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
async fn fetched_messages_survive_a_later_failure() {
    // the device hands out one message, then stops answering
    let requests = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        10 if requests.fetch_add(1, Ordering::SeqCst) == 0 => vec![private_message(b"hi")],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let messages = mc.fetch_messages().await.unwrap();
    assert!(matches!(&messages[..], [ReceivedMessage::Contact(m)] if m.text == "hi"));
    // nothing fetched: the failure itself
    assert!(matches!(mc.fetch_messages().await, Err(Error::Timeout)));
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
async fn settings_changed_at_once_keep_both_changes() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        38 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // each setter reads the other settings and writes them back: the second
    // must see the first's change
    let (acks, env) = tokio::join!(mc.set_multi_acks(2), mc.set_telemetry_mode_env(1));
    acks.unwrap();
    env.unwrap();
    let last = device.commands_with(38).last().unwrap().clone();
    // manual add 0, telemetry base 2 / loc 1 / env 1, loc policy 1, multi acks 2
    assert_eq!(last, vec![38, 0, 0b01_01_10, 1, 2]);
}

#[tokio::test(start_paused = true)]
async fn signing_sessions_at_once_run_one_after_the_other() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        33 => vec![[&[19u8, 0][..], &8192u32.to_le_bytes()].concat()],
        34 => vec![ok()],
        35 => vec![[&[20u8][..], &[0x5a; 64]].concat()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let (first, second) =
        tokio::join!(mc.sign(&[1; 200], 120, None), mc.sign(&[2; 200], 120, None));
    first.unwrap();
    second.unwrap();
    // the device has one session: start, chunks and finish of one, then the other
    let steps: Vec<u8> = device
        .commands()
        .iter()
        .map(|c| c[0])
        .filter(|code| (33..=35).contains(code))
        .collect();
    assert_eq!(steps, [33, 34, 34, 35, 33, 34, 34, 35]);
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
    // the firmware answers ERR_CODE_ILLEGAL_ARG to a path that is no whole
    // number of hops (here of 2 bytes) or has more than 64
    for (flags, path) in [(1, vec![0xaa; 3]), (0, vec![0xaa; 65])] {
        let request = TraceRequest {
            flags: Some(flags),
            path,
            ..TraceRequest::default()
        };
        assert!(matches!(
            mc.send_trace(&request).await,
            Err(Error::InvalidArgument(_))
        ));
    }
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
    // 64 hops, and two of 8 bytes
    for (flags, path) in [(0, vec![0xaa; 64]), (3, vec![0xaa; 16])] {
        let request = TraceRequest {
            flags: Some(flags),
            path,
            ..TraceRequest::default()
        };
        mc.send_trace(&request).await.unwrap();
    }
    assert_eq!(device.commands_with(36).len(), 3);

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
async fn settings_the_firmware_refuses_are_not_sent() {
    // it answers them with ERR_CODE_ILLEGAL_ARG
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();
    let radio = [
        (149.999, 62.5, 9, 8),
        (2500.001, 62.5, 9, 8),
        (869.525, 6.999, 9, 8),
        (869.525, 500.001, 9, 8),
        (869.525, 62.5, 4, 8),
        (869.525, 62.5, 13, 8),
        (869.525, 62.5, 9, 4),
        (869.525, 62.5, 9, 9),
    ];
    for (freq, bw, sf, cr) in radio {
        let refused = mc.set_radio(freq, bw, sf, cr, None).await;
        let what = format!("{freq} {bw} {sf} {cr}");
        assert!(matches!(refused, Err(Error::InvalidArgument(_))), "{what}");
    }
    mc.set_radio(150.0, 7.0, 5, 5, None).await.unwrap();
    mc.set_radio(2500.0, 500.0, 12, 8, None).await.unwrap();
    assert_eq!(device.commands_with(11).len(), 2);

    // from -9 dBm up to the device's maximum (22 dBm)
    for dbm in [-9, 22] {
        mc.set_tx_power(dbm).await.unwrap();
    }
    for dbm in [-10, 23] {
        let refused = mc.set_tx_power(dbm).await;
        assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    }
    assert_eq!(device.commands_with(12).len(), 2);

    mc.set_path_hash_mode(2).await.unwrap();
    let refused = mc.set_path_hash_mode(3).await;
    assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    assert_eq!(device.commands_with(61).len(), 1);

    // a default flood scope name of 1 to 30 bytes, its '#' included
    mc.set_default_flood_scope(Some(&"s".repeat(29)))
        .await
        .unwrap();
    let refused = mc.set_default_flood_scope(Some(&"s".repeat(30))).await;
    assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    assert_eq!(device.commands_with(63).len(), 1);
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
    let (mc, device) = client(Options::default(), one_contact);
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
    assert_eq!(device.commands_with(4), [vec![4], contacts_since()]);
}

/// Answers a contact request with a list of one contact.
fn one_contact(cmd: &[u8]) -> Vec<Vec<u8>> {
    match cmd[0] {
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
    }
}

/// The command for the contacts changed since [`one_contact`]'s list.
fn contacts_since() -> Vec<u8> {
    [&[4u8][..], &5000u32.to_le_bytes()[..]].concat()
}

#[tokio::test(start_paused = true)]
async fn single_cached_contacts_do_not_stand_for_the_list() {
    let bob = PublicKey::new([0x33; 32]);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        30 => vec![contact_frame(3, *bob.as_bytes(), "Bob", 0xff, &[])],
        9 => vec![ok()],
        _ => one_contact(cmd),
    });
    mc.connect().await.unwrap();
    // one read by its key (as for an anonymous request), one added
    mc.get_contact_by_key(&bob).await.unwrap();
    let carol = Contact::new(PublicKey::new([0x44; 32]), ContactType::Chat, "Carol");
    mc.add_contact(&carol).await.unwrap();
    assert_eq!(mc.contacts().len(), 2);
    assert!(mc.ensure_contacts(false).await.unwrap());
    assert!(mc.get_contact_by_name("Alice").is_some());
    assert!(!mc.ensure_contacts(false).await.unwrap());
    assert_eq!(device.commands_with(4), [[4]]);
}

#[tokio::test(start_paused = true)]
async fn contacts_since_a_time_do_not_stand_for_the_list() {
    // the first list is one since a time: the device holds two contacts
    let (mc, device) = client(Options::default(), |cmd| match cmd {
        [4] => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
            contact_frame(3, [0x33; 32], "Bob", 0xff, &[]),
            [&[4][..], &5000u32.to_le_bytes()].concat(),
        ],
        [4, ..] => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, [0x33; 32], "Bob", 0xff, &[]),
            [&[4][..], &5000u32.to_le_bytes()].concat(),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.get_contacts(3000).await.unwrap();
    assert!(mc.ensure_contacts(false).await.unwrap());
    assert_eq!(mc.contacts().len(), 2);
    assert!(!mc.ensure_contacts(false).await.unwrap());
    assert_eq!(device.commands_with(4).last(), Some(&vec![4]));
}

#[tokio::test(start_paused = true)]
async fn contacts_count_as_changed_after_a_reconnect() {
    // the device reports adverts and path updates only to a connected app
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, one_contact);
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert!(!mc.contacts_dirty());
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(mc.is_connected() && mc.contacts_dirty());
    assert!(!mc.ensure_contacts(false).await.unwrap());
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(device.commands_with(4), [vec![4], contacts_since()]);
}

#[tokio::test(start_paused = true)]
async fn a_list_that_brings_nothing_new_keeps_the_time_to_go_on_from() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    // all contacts, or none since a time: such a list ends with 0
    let (mc, device) = client(options, |cmd| match cmd {
        [4] => one_contact(cmd),
        [4, ..] => vec![vec![2, 1, 0, 0, 0], vec![4, 0, 0, 0, 0]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(true).await.unwrap());
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(mc.contacts_lastmod(), 5000);
    // a path update of a node that is no contact (as after an anonymous
    // request) is in no list either
    device.push([&[0x81][..], &[0x55; 32]].concat()).await;
    device.settle().await;
    assert!(mc.ensure_contacts(true).await.unwrap());
    let since = contacts_since();
    assert_eq!(
        device.commands_with(4),
        [vec![4], since.clone(), since.clone()]
    );
    // an advert the list does not bring: the device's clock was set back, so
    // the contacts are read in full
    device.push([&[0x80][..], &KEY].concat()).await;
    device.settle().await;
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(device.commands_with(4)[3..], [since, vec![4]]);
    assert_eq!(mc.contacts_lastmod(), 5000);
    assert!(!mc.contacts_dirty());
}

#[tokio::test(start_paused = true)]
async fn a_list_that_lacks_a_reported_change_is_read_again_in_full() {
    // Bob's change is in the list since a time, Alice's is not: it came in the
    // second that time stands for, or the device's clock was set back
    const BOB: [u8; 32] = [0x33; 32];
    let count = AtomicU32::new(2);
    let (mc, device) = client(Options::default(), move |cmd| match cmd {
        [4] => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
            contact_frame(3, BOB, "Bob", 0xff, &[]),
            [&[4][..], &5000u32.to_le_bytes()].concat(),
        ],
        [4, ..] => vec![
            [&[2][..], &count.load(Ordering::SeqCst).to_le_bytes()].concat(),
            contact_frame(3, BOB, "Bob", 0xff, &[]),
            [&[4][..], &6000u32.to_le_bytes()].concat(),
        ],
        [15, ..] => {
            count.store(1, Ordering::SeqCst);
            vec![ok()]
        }
        _ => vec![],
    });
    let advert = |key: [u8; 32]| [&[0x80][..], &key].concat();
    mc.connect().await.unwrap();
    mc.get_contacts(0).await.unwrap();
    for key in [KEY, BOB] {
        device.push(advert(key)).await;
    }
    device.settle().await;
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(
        device.commands_with(4),
        [vec![4], contacts_since(), vec![4]]
    );
    assert!(!mc.contacts_dirty());
    // read by hand, such a list leaves the contacts marked as changed
    for key in [KEY, BOB] {
        device.push(advert(key)).await;
    }
    device.settle().await;
    mc.get_contacts(mc.contacts_lastmod()).await.unwrap();
    assert!(mc.contacts_dirty());
    assert_eq!(mc.contacts_lastmod(), 0);
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert!(!mc.contacts_dirty());
    assert_eq!(mc.contacts_lastmod(), 5000);
    // a contact removed meanwhile is in no list any more
    device.push(advert(KEY)).await;
    device.settle().await;
    mc.remove_contact(&PublicKey::new(KEY)).await.unwrap();
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(device.commands_with(4)[5..], [contacts_since()]);
    assert_eq!(mc.contacts_lastmod(), 6000);
}

#[tokio::test(start_paused = true)]
async fn contacts_removed_while_no_link_was_up_are_noticed() {
    // the device drops Alice to make room for Bob while it has no link to
    // report it on; it holds one contact before and after
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let replaced = Arc::new(AtomicBool::new(false));
    let (mc, device) = client(options, {
        let replaced = Arc::clone(&replaced);
        move |cmd| match cmd {
            [4, ..] if replaced.load(Ordering::SeqCst) => vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, [0x33; 32], "Bob", 0xff, &[]),
                [&[4][..], &6000u32.to_le_bytes()].concat(),
            ],
            _ => one_contact(cmd),
        }
    });
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(true).await.unwrap());
    replaced.store(true, Ordering::SeqCst);
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(mc.ensure_contacts(true).await.unwrap());
    let names: Vec<String> = mc.contacts().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["Bob"]);
    assert_eq!(
        device.commands_with(4),
        [vec![4], contacts_since(), vec![4]]
    );
    assert!(!mc.contacts_dirty());
}

#[tokio::test(start_paused = true)]
async fn an_emptied_cache_is_read_in_full() {
    // since the last list nothing has changed on the device
    let (mc, device) = client(Options::default(), |cmd| match cmd {
        [4] => one_contact(cmd),
        [4, ..] => vec![vec![2, 1, 0, 0, 0], vec![4, 0, 0, 0, 0]],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.get_contacts(0).await.unwrap();
    mc.uncache_contact(&PublicKey::new(KEY));
    assert!(mc.ensure_contacts(false).await.unwrap());
    assert_eq!(mc.contacts().len(), 1);
    assert_eq!(device.commands_with(4), [[4], [4]]);
}

#[tokio::test(start_paused = true)]
async fn a_list_that_goes_on_on_the_next_link_leaves_the_contacts_dirty() {
    // the device has stopped answering, it goes on with the list later
    let answers = Arc::new(AtomicBool::new(true));
    let (mc, device) = client_answering_app_start({
        let answers = Arc::clone(&answers);
        move || answers.load(Ordering::SeqCst)
    });
    mc.connect().await.unwrap();
    let (asked, ()) = tokio::join!(mc.get_contacts_async(0), async {
        device.settle().await;
        device.push(vec![2, 1, 0, 0, 0]).await;
    });
    asked.unwrap();
    device.push(contact_frame(3, KEY, "Alice", 0xff, &[])).await;
    device.settle().await;
    answers.store(false, Ordering::SeqCst);
    let (connected, ()) = tokio::join!(mc.connect(), async {
        device.settle().await;
        device.push(vec![4, 9, 0, 0, 0]).await;
    });
    assert!(matches!(connected, Err(Error::Timeout)));
    assert_eq!(mc.contacts().len(), 1);
    assert!(mc.contacts_dirty());
}

#[tokio::test(start_paused = true)]
async fn automatic_contact_updates_catch_up_after_a_reconnect() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, one_contact);
    mc.set_auto_update_contacts(true);
    mc.connect().await.unwrap();
    device.settle().await;
    // not read yet: that is up to the program, or the first advert
    assert_eq!(device.commands_with(4).len(), 0);
    mc.get_contacts(0).await.unwrap();
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let app_start = b"\x01\x03      mccli".to_vec();
    assert_eq!(
        device.commands(),
        [app_start.clone(), vec![4], app_start, contacts_since()]
    );
    assert!(!mc.contacts_dirty());
    // a session started anew on the same link has missed nothing
    mc.send_appstart().await.unwrap();
    device.settle().await;
    assert_eq!(device.commands_with(4).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn an_advert_during_a_contact_list_has_the_contacts_read_again() {
    // the advert comes after the list brought its contact, so the list may
    // hold the contact from before the change
    let mut advert = vec![0x80];
    advert.extend(KEY);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        4 => {
            let mut end = vec![4];
            end.extend(5000u32.to_le_bytes());
            vec![
                vec![2, 1, 0, 0, 0],
                contact_frame(3, KEY, "Alice", 0xff, &[]),
                advert.clone(),
                end,
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert!(mc.contacts_dirty());
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(device.commands_with(4).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn the_end_of_a_list_given_up_on_is_not_the_next_answer() {
    // the first list stalls after one contact, so its request gives up; its
    // rest comes behind the next request, then the device's new list
    let requests = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        4 if requests.fetch_add(1, Ordering::SeqCst) == 0 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
        ],
        4 => {
            let end = |lastmod: u32| [&[4u8][..], &lastmod.to_le_bytes()].concat();
            vec![
                contact_frame(3, [0x22; 32], "Rep", 0xff, &[]),
                end(1000),
                vec![2, 1, 0, 0, 0],
                contact_frame(3, [0x33; 32], "Bob", 0xff, &[]),
                end(2000),
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(matches!(mc.get_contacts(0).await, Err(Error::Timeout)));
    let list = mc.get_contacts(0).await.unwrap();
    assert_eq!(list.lastmod, 2000);
    assert_eq!(list.contacts.len(), 1);
    assert_eq!(list.contacts[0].name, "Bob");
    assert_eq!(mc.contacts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_contact_list_fails_once_it_stalls_for_5_seconds() {
    // the first request gets no answer, the second a start and one contact;
    // meshcore_py looks every 5 s whether a contact came
    let requests = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        4 if requests.fetch_add(1, Ordering::SeqCst) == 0 => vec![],
        4 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    for _ in 0..2 {
        let started = tokio::time::Instant::now();
        assert!(matches!(mc.get_contacts(0).await, Err(Error::Timeout)));
        assert_eq!(started.elapsed().as_secs(), 5);
    }
}

#[tokio::test(start_paused = true)]
async fn a_busy_device_refuses_a_second_contact_list() {
    // the old list is still coming when the next request arrives
    let requests = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        4 if requests.fetch_add(1, Ordering::SeqCst) == 0 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
        ],
        4 => vec![
            contact_frame(3, [0x22; 32], "Rep", 0xff, &[]),
            err(4),
            [&[4u8][..], &1000u32.to_le_bytes()].concat(),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    assert!(matches!(mc.get_contacts(0).await, Err(Error::Timeout)));
    assert!(matches!(
        mc.get_contacts(0).await,
        Err(Error::Device(ErrorCode::BadState))
    ));
}

#[tokio::test(start_paused = true)]
async fn a_refused_contact_request_is_not_the_answer_to_the_next_command() {
    // a list is coming in when it is asked for again without waiting for it
    let requests = AtomicU32::new(0);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        4 if requests.fetch_add(1, Ordering::SeqCst) == 0 => vec![vec![2, 1, 0, 0, 0]],
        4 => vec![err(4)],
        13 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.get_contacts_async(0).await.unwrap();
    let refused = mc.get_contacts_async(0).await;
    mc.reset_path(&key()).await.unwrap();
    assert!(matches!(refused, Err(Error::Device(ErrorCode::BadState))));
}

#[tokio::test(start_paused = true)]
async fn commands_while_a_contact_list_comes_in_keep_their_changes() {
    // the list ends only when the test pushes its end, so commands run while
    // it comes in, after it brought the contacts they change
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
            contact_frame(3, [0x22; 32], "Rep", 0x41, &[0xaa, 0xbb]),
        ],
        13 | 15 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let mut events = mc.subscribe();
    mc.get_contacts_async(0).await.unwrap();
    let mut brought = 0;
    while brought < 2 {
        if let Event::NextContact(_) = events.recv().await.unwrap() {
            brought += 1;
        }
    }
    let rep = PublicKey::new([0x22; 32]);
    mc.remove_contact(&key()).await.unwrap();
    mc.reset_path(&rep).await.unwrap();
    let mut end = vec![4];
    end.extend(5000u32.to_le_bytes());
    device.push(end).await;
    loop {
        if let Event::Contacts(list) = events.recv().await.unwrap() {
            // the list is what the device sent
            assert_eq!(list.contacts.len(), 2);
            break;
        }
    }
    assert!(mc.contact(&key()).is_none());
    assert!(mc.contact(&rep).unwrap().is_flood());
    assert_eq!(mc.contacts().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_contact_that_changes_right_after_a_command_keeps_the_change() {
    // the commands' futures are polled again only after the device has sent
    // the contact anew, changed by an advert
    for code in [9u8, 13, 15] {
        let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
            4 => vec![vec![2, 1, 0, 0, 0]],
            9 | 13 | 15 => vec![ok()],
            _ => vec![],
        });
        mc.connect().await.unwrap();
        mc.get_contacts_async(0).await.unwrap();
        let contact = Contact::new(key(), ContactType::Repeater, "written");
        let mut command = Box::pin(async {
            match code {
                9 => mc.add_contact(&contact).await,
                13 => mc.reset_path(&key()).await,
                _ => mc.remove_contact(&key()).await,
            }
        });
        assert!(!poll_once(&mut command).await);
        device.settle().await;
        assert_eq!(device.commands_with(code).len(), 1);
        for frame in [
            [&[0x80][..], &KEY].concat(),
            contact_frame(3, KEY, "Alice", 0x01, &[0xaa]),
            [&[4][..], &5000u32.to_le_bytes()].concat(),
        ] {
            device.push(frame).await;
        }
        device.settle().await;
        command.await.unwrap();
        let cached = mc.contact(&key()).unwrap();
        assert_eq!((cached.name.as_str(), cached.out_path.len()), ("Alice", 1));
        assert!(mc.contacts_dirty(), "to be read again");
    }
}

#[tokio::test(start_paused = true)]
async fn what_the_device_took_counts_when_the_command_is_given_up() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => vec![
            vec![2, 1, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0x01, &[0xaa]),
            [&[4][..], &2000u32.to_le_bytes()].concat(),
        ],
        8 | 13 => vec![ok()],
        _ => vec![],
    });
    let alice = key();
    mc.connect().await.unwrap();
    mc.get_contacts(0).await.unwrap();
    assert!(!mc.contact(&alice).unwrap().is_flood());
    // their futures are not polled again after the command went out
    let mut command = Box::pin(mc.set_name("New"));
    assert!(!poll_once(&mut command).await);
    device.settle().await;
    drop(command);
    let mut command = Box::pin(mc.reset_path(&alice));
    assert!(!poll_once(&mut command).await);
    device.settle().await;
    drop(command);
    assert_eq!(mc.self_info().unwrap().name, "New");
    assert!(mc.contact(&alice).unwrap().is_flood());
    assert!(!mc.contacts_dirty());
}

#[tokio::test(start_paused = true)]
async fn a_contact_command_without_an_answer_has_the_contacts_read_again() {
    // the device does what it is told, but the answer gets lost; resetting the
    // path leaves the contact's lastmod, so a list since then does not bring it
    let reset = Arc::new(AtomicBool::new(false));
    let (mc, device) = client(Options::default(), {
        let reset = Arc::clone(&reset);
        move |cmd| {
            let path: &[u8] = if reset.load(Ordering::SeqCst) {
                &[]
            } else {
                &[0xaa]
            };
            let len = if path.is_empty() { 0xff } else { 1 };
            match cmd {
                [4] => vec![
                    vec![2, 1, 0, 0, 0],
                    contact_frame(3, KEY, "Alice", len, path),
                    [&[4][..], &2000u32.to_le_bytes()].concat(),
                ],
                [4, ..] => vec![vec![2, 1, 0, 0, 0], vec![4, 0, 0, 0, 0]],
                [13, ..] => {
                    reset.store(true, Ordering::SeqCst);
                    vec![]
                }
                _ => vec![err(2)],
            }
        }
    });
    let alice = key();
    mc.connect().await.unwrap();
    // given up at its timeout, and by its caller
    for wait in [Duration::from_secs(60), Duration::from_secs(1)] {
        reset.store(false, Ordering::SeqCst);
        mc.get_contacts(0).await.unwrap();
        assert!(!mc.contact(&alice).unwrap().is_flood());
        assert!(!mc.contacts_dirty());
        let given_up = tokio::time::timeout(wait, mc.reset_path(&alice)).await;
        assert!(!matches!(given_up, Ok(Ok(()))));
        assert!(mc.contacts_dirty());
        assert!(mc.ensure_contacts(true).await.unwrap());
        assert!(mc.contact(&alice).unwrap().is_flood());
        assert!(!mc.contacts_dirty());
    }
    // one that does not go out changes nothing
    device.drop_link().await;
    device.settle().await;
    let lastmod = mc.contacts_lastmod();
    assert!(matches!(
        mc.reset_path(&alice).await,
        Err(Error::NotConnected)
    ));
    mc.connect().await.unwrap();
    mc.ensure_contacts(true).await.unwrap();
    assert_eq!(mc.contacts_lastmod(), lastmod);
    assert_eq!(device.commands_with(4).last().unwrap().len(), 5);
}

#[tokio::test(start_paused = true)]
async fn a_contact_the_device_no_longer_holds_has_the_contacts_read_again() {
    // someone removed Alice on the device, which does not report that
    let alice = key();
    let bob = PublicKey::new([0x33; 32]);
    for code in [9u8, 13, 15, 30] {
        let removed = Arc::new(AtomicBool::new(false));
        let (mc, _device) = client(Options::default(), {
            let removed = Arc::clone(&removed);
            move |cmd| match cmd[0] {
                4 if removed.load(Ordering::SeqCst) => {
                    vec![vec![2, 0, 0, 0, 0], vec![4, 0, 0, 0, 0]]
                }
                4 => one_contact(cmd),
                9 => vec![err(3)],
                _ => vec![err(2)],
            }
        });
        mc.connect().await.unwrap();
        mc.get_contacts(0).await.unwrap();
        removed.store(true, Ordering::SeqCst);
        let refused = async |key: &PublicKey| {
            let result = match code {
                9 => {
                    let contact = Contact::new(*key, ContactType::Chat, "x");
                    mc.add_contact(&contact).await
                }
                13 => mc.reset_path(key).await,
                15 => mc.remove_contact(key).await,
                _ => mc.get_contact_by_key(key).await.map(drop),
            };
            assert!(matches!(result, Err(Error::Device(_))));
        };
        // one the cache does not hold either tells nothing
        refused(&bob).await;
        assert!(!mc.contacts_dirty());
        refused(&alice).await;
        assert!(mc.contacts_dirty());
        assert!(mc.ensure_contacts(true).await.unwrap());
        assert_eq!(mc.contacts(), Vec::<Contact>::new());
    }
}

#[tokio::test(start_paused = true)]
async fn a_list_during_which_a_contact_is_removed_does_not_count_as_read() {
    // the firmware keeps its contacts in an array: removing Alice moves Rep to
    // her place, which the list has passed, and Bob is added at the end
    let rep = Contact::new(PublicKey::new([0x22; 32]), ContactType::Repeater, "Rep");
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        4 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, KEY, "Alice", 0xff, &[]),
        ],
        15 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    mc.cache_contact(rep.clone());
    mc.get_contacts_async(0).await.unwrap();
    device.settle().await;
    mc.remove_contact(&key()).await.unwrap();
    let bob = contact_frame(3, [0x33; 32], "Bob", 0xff, &[]);
    for frame in [bob, [&[4][..], &5000u32.to_le_bytes()].concat()] {
        device.push(frame).await;
    }
    device.settle().await;
    assert_eq!(mc.contact(&rep.public_key), Some(rep));
    assert!(mc.get_contact_by_name("Bob").is_some());
    assert!(mc.contact(&key()).is_none() && mc.contacts_dirty());
    assert_eq!(mc.contacts_lastmod(), 0);
}

#[tokio::test(start_paused = true)]
async fn flood_scopes_go_back_to_the_default_or_none() {
    let (mc, device) = client(Options::default(), |_| vec![ok()]);
    mc.connect().await.unwrap();
    mc.reset_flood_scope().await.unwrap();
    mc.force_unscoped().await.unwrap();
    let scopes = device.commands_with(54);
    // without a key, as meshcore_py sends it; the firmware resets the scope
    assert_eq!(scopes[0], [54, 0]);
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
    // the firmware takes 0 and 100000 to 999999, and refuses the others
    for pin in [0, 100_000, 999_999] {
        mc.set_devicepin(pin).await.unwrap();
    }
    for pin in [12_345, 1_000_000] {
        assert!(matches!(
            mc.set_devicepin(pin).await,
            Err(Error::InvalidArgument(_))
        ));
    }
    assert_eq!(device.commands_with(37).len(), 4);
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

    // the self info takes the location as the device reports it back: it
    // keeps the degrees as a double, and 249 millionths come back as 248
    mc.set_coords(0.000_249, -0.000_249).await.unwrap();
    let sent = &device.commands_with(14)[1];
    assert_eq!(sent[1..5], 249i32.to_le_bytes());
    assert_eq!(sent[5..9], (-249i32).to_le_bytes());
    let info = mc.self_info().unwrap();
    assert_eq!((info.latitude_e6, info.longitude_e6), (248, -248));

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

    // the self info takes frequency and bandwidth as the device reports them
    // back: it keeps them as floats, and 512001 kHz and 8004 Hz come back 1
    // lower (no LoRa band or bandwidth does)
    mc.set_radio(512.001, 8.004, 9, 8, None).await.unwrap();
    let sent = &device.commands_with(11)[2];
    assert_eq!(sent[1..5], 512_001u32.to_le_bytes());
    assert_eq!(sent[5..9], 8_004u32.to_le_bytes());
    let info = mc.self_info().unwrap();
    assert_eq!((info.radio_freq, info.radio_bw), (512.0, 8.003));
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
        30 => vec![err(2)],
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
async fn a_subscription_answer_cut_short_is_an_error() {
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        50 => vec![sent_frame(false, 0x610, 1000), binary_response(0x610, &[1])],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let answer = mc
        .req_subscribe_sync(
            &key(),
            [1, 2, 3, 4],
            60,
            &[1, 103, 10],
            RequestOptions::default(),
        )
        .await;
    assert!(matches!(answer, Err(Error::Protocol(reason)) if reason == "invalid_frame_length"));
}

#[tokio::test(start_paused = true)]
async fn a_subscription_takes_3_to_14_bytes_of_deltas() {
    // as many as the sensor keeps
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    for deltas in [&[1, 103][..], &[1; 15]] {
        let options = RequestOptions::default();
        let refused = mc
            .req_subscribe_sync(&key(), [1, 2, 3, 4], 60, deltas, options)
            .await;
        assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    }
    assert_eq!(device.commands_with(50).len(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_owner_answer_without_a_line_break_is_the_name_alone() {
    // meshcore_py fails on it with an IndexError, and so on an owner info
    // answer with fewer than two line breaks
    let (mc, _device) = client(Options::default(), |cmd| match cmd[0] {
        30 => vec![err(2)],
        50 => vec![
            sent_frame(false, 0x707, 1000),
            binary_response(0x707, b"v1.18.0\nRep 1\0\0"),
        ],
        57 => {
            let answer = [&7u32.to_le_bytes()[..], b"Rep 1\0\0"].concat();
            vec![
                sent_frame(false, 0x700, 1000),
                binary_response(0x700, &answer),
            ]
        }
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let anon = mc
        .req_owner_sync(&key(), RequestOptions::default())
        .await
        .unwrap();
    assert_eq!((anon.name.as_str(), anon.owner.as_str()), ("Rep 1", ""));
    let info = mc
        .req_owner_info_sync(&key(), RequestOptions::default())
        .await
        .unwrap();
    let fields = (info.firmware_version.as_str(), info.name.as_str());
    assert_eq!((fields, info.owner.as_str()), (("v1.18.0", "Rep 1"), ""));
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
    mc.cache_contact(Contact::new(key(), ContactType::Chat, "a"));
    let token = mc.request_factory_reset();
    mc.confirm_factory_reset(&token).await.unwrap();
    assert_eq!(device.commands_with(51)[0], b"\x33reset".to_vec());
    // the device starts anew
    assert!(mc.contacts().is_empty() && mc.self_info().is_none());
    assert!(mc.confirm_factory_reset(&token).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_factory_reset_that_fails_leaves_the_state_alone() {
    let (mc, _device) = client(Options::default(), |_| vec![err(5)]);
    mc.connect().await.unwrap();
    mc.cache_contact(Contact::new(key(), ContactType::Chat, "a"));
    let token = mc.request_factory_reset();
    assert!(matches!(
        mc.confirm_factory_reset(&token).await,
        Err(Error::Device(ErrorCode::FileIoError))
    ));
    assert!(mc.contacts().len() == 1 && mc.self_info().is_some());
}

#[tokio::test(start_paused = true)]
async fn a_factory_reset_whose_answer_is_not_waited_for_counts_as_done() {
    let (mc, _device) = client(Options::default(), one_contact);
    mc.connect().await.unwrap();
    mc.get_contacts(0).await.unwrap();
    let token = mc.request_factory_reset();
    let reset = mc.confirm_factory_reset(&token);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), reset)
            .await
            .is_err()
    );
    assert_eq!(mc.self_info(), None);
    assert_eq!(mc.contacts(), Vec::<Contact>::new());
}

#[tokio::test(start_paused = true)]
async fn reconnecting_to_another_device_reads_its_contacts() {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let (mc, device) = client(options, one_contact);
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(false).await.unwrap());
    // the same device again: its contacts are known
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!mc.ensure_contacts(false).await.unwrap());
    // another one, as a board plugged into the port meanwhile
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut other = self_info_frame("Other");
    other[4] ^= 1;
    device.push(other).await;
    device.settle().await;
    assert_eq!(mc.contacts(), Vec::<Contact>::new());
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(mc.contacts().len(), 1);
    assert_eq!(device.commands_with(4), [[4], [4]], "read in full");
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
async fn answers_about_another_channel_key_or_node_are_passed_over() {
    // each answer asked for comes second; meshcore_py takes the first of the
    // kind
    let other = [0x33u8; 32];
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        31 => {
            let info = |index: u8, name: &str| {
                let mut field = [0u8; 32];
                field[..name.len()].copy_from_slice(name.as_bytes());
                [&[18, index][..], &field, &[index; 16]].concat()
            };
            vec![info(1, "#other"), info(cmd[1], "#asked")]
        }
        // the contact asked for within a list that is coming in
        30 => vec![
            vec![2, 2, 0, 0, 0],
            contact_frame(3, other, "Bob", 0xff, &[]),
            contact_frame(3, KEY, "Alice", 0xff, &[]),
            vec![4, 0, 0, 0, 0],
        ],
        52 => {
            let found = |key: &[u8], hash: u8| {
                [&[0x8du8, 0][..], &key[..6], &[0x41, hash, hash, 0xff]].concat()
            };
            vec![
                sent_frame(false, 0x800, 1000),
                found(&other, 0xcc),
                found(&KEY, 0xaa),
            ]
        }
        // another node refuses its login
        26 => vec![
            sent_frame(false, 0x900, 1000),
            [&[0x86u8, 0][..], &other[..6]].concat(),
            [&[0x85u8, 1][..], &KEY[..6]].concat(),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let options = RequestOptions::default();
    assert_eq!(mc.get_channel(2).await.unwrap().channel_name, "#asked");
    assert_eq!(mc.get_contact_by_key(&key()).await.unwrap().name, "Alice");
    let paths = mc.send_path_discovery_sync(&key(), options).await.unwrap();
    assert_eq!(paths.out_path, vec![0xaa, 0xaa]);
    let login = mc
        .send_login_sync(&key(), "password", options)
        .await
        .unwrap();
    assert_eq!(login.pubkey_prefix, Some([0x11; 6]));
}

#[tokio::test(start_paused = true)]
async fn a_device_with_an_imported_key_is_still_the_same() {
    let (mc, device) = client(Options::default(), |cmd| match cmd {
        [24, ..] => vec![ok()],
        [39, 0, 0, 0] => vec![[&[0x8b, 0][..], &[0x23; 6], &[1, 116, 0x01, 0x4a]].concat()],
        _ => one_contact(cmd),
    });
    mc.connect().await.unwrap();
    assert!(mc.ensure_contacts(false).await.unwrap());
    mc.import_private_key(&[7; 64]).await.unwrap();
    // it answers with its new key at once
    let telemetry = mc.get_self_telemetry().await.unwrap();
    assert_eq!(telemetry.pubkey_prefix, [0x23; 6]);
    let mut info = self_info_frame("Node");
    info[4..36].fill(0x23);
    device.push(info).await;
    device.settle().await;
    assert_eq!(mc.self_info().unwrap().public_key.prefix(), [0x23; 6]);
    assert_eq!(mc.contacts().len(), 1);
    // it has read its contacts anew, those it had not stored yet are gone
    assert!(mc.ensure_contacts(true).await.unwrap());
    assert_eq!(device.commands_with(4), [[4], [4]]);
    // with yet another key it is another device
    let mut info = self_info_frame("Node");
    info[4..36].fill(0x24);
    device.push(info).await;
    device.settle().await;
    assert_eq!(mc.contacts(), Vec::<Contact>::new());
}

#[tokio::test(start_paused = true)]
async fn a_command_goes_by_what_those_before_it_changed() {
    let (mc, device) = client(Options::default(), |cmd| match cmd {
        [3 | 8 | 24, ..] => vec![ok()],
        [39, 0, 0, 0] => vec![[&[0x8b, 0][..], &[0x23; 6], &[1, 116, 0x01, 0x4a]].concat()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // the device's own telemetry while another key is being imported
    let mut import = Box::pin(mc.import_private_key(&[7; 64]));
    assert!(!poll_once(&mut import).await);
    let mut telemetry = Box::pin(mc.get_self_telemetry());
    assert!(!poll_once(&mut telemetry).await);
    import.await.unwrap();
    assert_eq!(telemetry.await.unwrap().pubkey_prefix, [0x23; 6]);
    // a channel text while a longer name is being set
    let mut name = Box::pin(mc.set_name("Longer name"));
    assert!(!poll_once(&mut name).await);
    let text = "x".repeat(MAX_TEXT_LENGTH - "Node: ".len());
    let mut message = Box::pin(mc.send_chan_msg(0, &text, None));
    assert!(!poll_once(&mut message).await);
    name.await.unwrap();
    assert!(matches!(message.await, Err(Error::InvalidArgument(_))));
    assert_eq!(device.commands_with(3).len(), 0);
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
    // texts too short are refused, the firmware would take their commands for
    // unknown ones
    assert!(refused(mc.send_msg(key(), "", None, 0).await.map(drop)));
    assert!(refused(mc.send_cmd(key(), "", None, None).await.map(drop)));
    assert!(refused(mc.set_name("").await));
    assert!(refused(mc.run_cli_command("v").await.map(drop)));
    assert!(refused(mc.set_custom_var("", "1").await));
    assert!(refused(mc.set_custom_var("a", "").await));
    // the firmware would split the variable at the first colon
    assert!(refused(mc.set_custom_var("a:b", "c").await));
    // the firmware would end these texts at the NUL
    assert!(refused(mc.send_msg(key(), "a\0b", None, 0).await.map(drop)));
    assert!(refused(mc.send_chan_msg(0, "a\0b", None).await));
    assert!(refused(mc.set_name("a\0b").await));
    assert!(refused(mc.run_cli_command("ve\0r").await.map(drop)));
    assert!(refused(mc.set_channel(1, "#a\0b", None).await));
    assert!(refused(mc.set_custom_var("gps", "1\0").await));
    assert!(refused(mc.set_default_flood_scope(Some("a\0b")).await));
    let mut contact = Contact::new(key(), ContactType::Chat, "x");
    contact.name = "a\0b".into();
    assert!(refused(mc.update_contact(&contact, None).await));
    assert!(refused(mc.send_login(&key(), "pa\0ss").await.map(drop)));
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
    for code in [55, 65, 18, 26, 32, 63, 9] {
        assert_eq!(device.commands_with(code).len(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn channel_texts_are_checked_before_connecting() {
    let (mc, _device) = client(Options::default(), |_| vec![]);
    let refused = |result: meshcore::Result<()>| matches!(result, Err(Error::InvalidArgument(_)));
    // no self info yet: a NUL, or a text too long for any node name, is refused
    assert!(refused(mc.send_chan_msg(0, "a\0b", None).await));
    let too_long = "x".repeat(MAX_TEXT_LENGTH - 1);
    assert!(refused(mc.send_chan_msg(0, &too_long, None).await));
    // anything else goes to the link, which is not up
    assert!(matches!(
        mc.send_chan_msg(0, "hi", None).await,
        Err(Error::NotConnected)
    ));
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
async fn a_node_name_cut_inside_a_character_counts_with_all_its_bytes() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        3 | 8 => vec![ok()],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // another app left the cut to the firmware
    let mut info = self_info_frame("Caf");
    info.push(0xc3);
    device.push(info).await;
    device.settle().await;
    assert_eq!(mc.self_info().unwrap().name, "Caf");
    // the device sends four bytes and ": " before the text
    let text = "x".repeat(MAX_TEXT_LENGTH - 5);
    let refused = mc.send_chan_msg(0, &text, None).await;
    assert!(matches!(refused, Err(Error::InvalidArgument(_))));
    mc.send_chan_msg(0, &text[1..], None).await.unwrap();
    // a name set here is whole
    mc.set_name("Caf").await.unwrap();
    mc.send_chan_msg(0, &text, None).await.unwrap();
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
    // also with automatic reconnecting of zero attempts
    for options in [
        Options::default(),
        Options {
            auto_reconnect: true,
            max_reconnect_attempts: 0,
            ..Options::default()
        },
    ] {
        let (mc, device) = client(options, |_| vec![]);
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
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(device.opens(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn automatic_contact_updates_survive_disconnect_and_connect() {
    let (mc, device) = client(Options::default(), one_contact);
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
    // The disconnect comes in right before the reconnect's link is up, too
    // late to stop opening it (as on another thread).
    device.during_next_open({
        let mc = mc.clone();
        async move {
            let mut disconnect = Box::pin(async move { mc.disconnect().await });
            let _ = poll_once(&mut disconnect).await;
        }
    });
    device.drop_link().await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!((device.opens(), device.closes()), (2, 2));
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
async fn a_request_that_waits_its_turn_through_a_new_connect_gets_its_answer() {
    let (mc, device) = client(Options::default(), |cmd| match cmd[0] {
        50 => vec![
            sent_frame(false, 0xbeef, 5000),
            binary_response(0xbeef, &status_fields(4100)),
        ],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    // the status request waits behind a command that is not polled meanwhile
    let mut unanswered = Box::pin(async { mc.get_time().await });
    assert!(!poll_once(&mut unanswered).await);
    let pending = tokio::spawn({
        let mc = mc.clone();
        async move { mc.req_status_sync(&key(), RequestOptions::default()).await }
    });
    device.settle().await;
    let connect = tokio::spawn({
        let mc = mc.clone();
        async move { mc.connect().await }
    });
    device.settle().await;
    assert_eq!(device.opens(), 2);
    // it goes out on the new link, so the end of the old one is none of its
    assert!(matches!(unanswered.await, Err(Error::ConnectionLost)));
    assert_eq!(pending.await.unwrap().unwrap().bat, 4100);
    connect.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_answer_from_before_a_login_is_not_taken_for_its_own() {
    let mut success = vec![0x85, 1];
    success.extend(&KEY[..6]);
    let (mc, _device) = client(Options::default(), move |cmd| match cmd[0] {
        // the node's late answer to an earlier login, then the login goes out
        26 => vec![success.clone(), sent_frame(false, 1, 1000)],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let result = mc
        .send_login_sync(&key(), "password", RequestOptions::default())
        .await;
    assert!(matches!(result, Err(Error::Timeout)), "{result:?}");
}

#[tokio::test(start_paused = true)]
async fn sync_requests_run_one_after_the_other() {
    // the firmware keeps one pending request, which the next would replace;
    // no request here gets an answer
    let sent = AtomicU32::new(0);
    let (mc, device) = client(Options::default(), move |cmd| match cmd[0] {
        50 | 52 => vec![sent_frame(
            false,
            0x500 + sent.fetch_add(1, Ordering::SeqCst),
            1000,
        )],
        _ => vec![],
    });
    mc.connect().await.unwrap();
    let options = RequestOptions::default();
    let status = tokio::spawn({
        let mc = mc.clone();
        async move { mc.req_status_sync(&key(), options).await }
    });
    device.settle().await;
    let paths = tokio::spawn({
        let mc = mc.clone();
        async move { mc.send_path_discovery_sync(&key(), options).await }
    });
    device.settle().await;
    // the path discovery waits for the status request to end; a request that
    // does not wait for its answer is not held back
    mc.send_binary_req(&key(), BinaryReqType::KeepAlive, &[7], options)
        .await
        .unwrap();
    assert_eq!(device.commands_with(52).len(), 0);
    assert!(matches!(status.await.unwrap(), Err(Error::Timeout)));
    assert!(matches!(paths.await.unwrap(), Err(Error::Timeout)));
    let codes: Vec<u8> = device
        .commands()
        .iter()
        .map(|command| command[0])
        .filter(|&code| code == 50 || code == 52)
        .collect();
    assert_eq!(codes, [50, 50, 52]);
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
async fn a_message_on_its_way_is_not_lost_by_a_disconnect() {
    // the device has taken the message out of its queue, the frame is on its
    // way, whether the fetching was stopped before or not
    for stopped in [false, true] {
        let (mc, device) = client(Options::default(), |_| vec![]);
        mc.connect().await.unwrap();
        let mut events = mc.subscribe();
        mc.start_auto_message_fetching();
        device.settle().await;
        if stopped {
            mc.stop_auto_message_fetching();
            device.settle().await;
        }
        let (gone, ()) = tokio::join!(mc.disconnect(), async {
            device.settle().await;
            device.try_push(private_message(b"last"));
        });
        gone.unwrap();
        assert_eq!(next_message(&mut events).await, "last");
        assert!(matches!(
            events.recv().await,
            Some(Event::Disconnected { .. })
        ));
        // and no more were asked for
        assert_eq!(device.commands_with(10).len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_command_that_waits_its_turn_is_not_sent_after_a_disconnect() {
    // the device would hand out a message whose frame the disconnect cuts off
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    let time = tokio::spawn({
        let mc = mc.clone();
        async move { mc.get_time().await }
    });
    device.settle().await;
    let message = tokio::spawn({
        let mc = mc.clone();
        async move { mc.get_msg().await }
    });
    device.settle().await;
    let disconnected = tokio::spawn({
        let mc = mc.clone();
        async move { mc.disconnect().await }
    });
    device.settle().await;
    device.push(vec![9, 0x10, 0, 0, 0]).await;
    assert_eq!(time.await.unwrap().unwrap(), 16);
    assert!(matches!(message.await.unwrap(), Err(Error::NotConnected)));
    disconnected.await.unwrap().unwrap();
    assert_eq!(device.commands_with(10), Vec::<Vec<u8>>::new());
    // the next link takes commands again
    mc.connect().await.unwrap();
    assert!(matches!(mc.get_msg().await, Err(Error::Timeout)));
    assert_eq!(device.commands_with(10).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn auto_fetch_stops_after_the_message_in_flight() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    mc.start_auto_message_fetching();
    device.settle().await;
    mc.stop_auto_message_fetching();
    // one is handed out and more are waiting
    device.push(vec![0x83]).await;
    device.push(private_message(b"last")).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    device.push(vec![0x83]).await;
    device.settle().await;
    assert_eq!(device.commands_with(10).len(), 1);
    // started again, it goes on
    mc.start_auto_message_fetching();
    device.settle().await;
    assert_eq!(device.commands_with(10).len(), 2);
    device.push(vec![10]).await;
    device.push(vec![0x83]).await;
    device.settle().await;
    assert_eq!(device.commands_with(10).len(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_disconnect_that_waits_for_a_reply_gives_way_to_a_connect() {
    let (mc, device) = client(Options::default(), |_| vec![]);
    mc.connect().await.unwrap();
    // the command in flight gets no reply
    let unanswered = || {
        let mc = mc.clone();
        tokio::spawn(async move { mc.get_time().await })
    };
    let command = unanswered();
    device.settle().await;
    let mut disconnect = Box::pin(mc.disconnect());
    assert!(!poll_once(&mut disconnect).await);
    mc.connect().await.unwrap();
    assert!(matches!(command.await.unwrap(), Err(Error::ConnectionLost)));
    disconnect.await.unwrap();
    assert!(mc.is_connected());
    // one that is given up goes on, and waits half a second at most
    unanswered();
    device.settle().await;
    let mut disconnect = Box::pin(mc.disconnect());
    assert!(!poll_once(&mut disconnect).await);
    drop(disconnect);
    device.settle().await;
    assert!(mc.is_connected());
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!mc.is_connected());
}

#[tokio::test(start_paused = true)]
async fn disconnects_at_once_both_wait_until_the_link_is_closed() {
    // also when a command is in flight
    for in_flight in [false, true] {
        let (mc, device) = client(Options::default(), |_| vec![]);
        mc.connect().await.unwrap();
        device.slow_close(Duration::from_secs(2));
        if in_flight {
            let mc = mc.clone();
            tokio::spawn(async move { mc.get_time().await });
            device.settle().await;
        }
        let closes = || {
            let (mc, device) = (mc.clone(), device.clone());
            tokio::spawn(async move {
                mc.disconnect().await.unwrap();
                device.closes()
            })
        };
        let (first, second) = (closes(), closes());
        assert_eq!((first.await.unwrap(), second.await.unwrap()), (1, 1));
    }
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
    // Frames of every type (the parity fixtures), cut at every length and with
    // each byte after the code replaced, plus random frames: none may panic the
    // reader or stop it.
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

    // also with NUL bytes at either end of the text, which the firmware does
    // not send: they go, as in the older frame, and the hash is taken without
    // them (meshcore_py keeps them in a v3 frame and misses the log entry)
    let mut padded = vec![17, 24, 0, 0, 0, 2, 0];
    padded.extend(1_700_000_000u32.to_le_bytes());
    padded.extend(b"\0alice: hi\0\0");
    device.push(padded).await;
    let padded = loop {
        if let Event::ChannelMessage(m) = events.recv().await.unwrap() {
            break m;
        }
    };
    assert_eq!(padded.text, "alice: hi");
    assert_eq!(padded.txt_hash, message.txt_hash);
    assert_eq!(padded.rx_info, message.rx_info);
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
