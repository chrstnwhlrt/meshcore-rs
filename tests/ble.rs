//! The BLE transport against a stand-in for `BlueZ`.
//!
//! Each test runs in a process and on a D-Bus bus of its own, and on real time,
//! as D-Bus goes through a socket.

#![cfg(all(feature = "ble", target_os = "linux"))]

mod bluez;
mod common;

use std::time::Duration;

use bluez::{ADDRESS, Bluez, NAME, Reaction, Then, answer, run};
use common::Random;
use meshcore::transport::ble::scan;
use meshcore::{BleConfig, BleTransport, Error, Event, MeshCore, Options, Subscription};
use tokio::time::{sleep, timeout};

/// How long a test waits for something that should happen right away.
const PATIENCE: Duration = Duration::from_secs(5);

const BATTERY: &[u8] = &[12, 0xd4, 0x0d];
const MESSAGES_WAITING: &[u8] = &[0x83];

const fn millis(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

fn client(config: BleConfig) -> MeshCore {
    MeshCore::new(BleTransport::new(config), Options::default())
}

/// A client for the companion by its address, which is paired already.
fn paired() -> MeshCore {
    client(BleConfig::new(ADDRESS))
}

fn with_pin(pin: u32) -> MeshCore {
    client(BleConfig::new(ADDRESS).pin(Some(pin)))
}

async fn connected() -> MeshCore {
    let mc = paired();
    mc.connect().await.unwrap();
    mc
}

/// The next event.
async fn next(events: &mut Subscription) -> Event {
    timeout(PATIENCE, events.recv())
        .await
        .expect("no event")
        .expect("the client is gone")
}

/// Why the connection is reported gone.
async fn disconnected(events: &mut Subscription) -> String {
    loop {
        if let Event::Disconnected { reason } = next(events).await {
            return reason;
        }
    }
}

/// Waits until `happened`.
async fn wait_until(what: &str, happened: impl Fn() -> bool) {
    let wait = async {
        while !happened() {
            sleep(millis(1)).await;
        }
    };
    let result = timeout(PATIENCE, wait).await;
    assert!(result.is_ok(), "it takes too long: {what}");
}

async fn link_is_down(bluez: &Bluez) {
    wait_until("the link goes down", || !bluez.connected()).await;
}

async fn works(mc: &MeshCore) {
    assert_eq!(mc.get_bat().await.unwrap().level, 3540);
}

fn failed_pairing(result: meshcore::Result<meshcore::SelfInfo>) -> String {
    match result {
        Err(Error::PairingFailed(reason)) => reason,
        other => panic!("{other:?}"),
    }
}

fn in_use(result: meshcore::Result<meshcore::SelfInfo>) -> String {
    match result {
        Err(Error::DeviceInUse(reason)) => reason,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_session_end_to_end() {
    run(a_session_end_to_end, async |bluez| {
        let mc = paired();
        assert_eq!(mc.connect().await.unwrap().name, "Node");
        assert_eq!(mc.connection_info().as_deref(), Some(ADDRESS));
        assert!(mc.is_connected() && bluez.connected());
        works(&mc).await;
        let mut events = mc.subscribe();
        bluez.send(MESSAGES_WAITING);
        assert!(matches!(next(&mut events).await, Event::MessagesWaiting));
        mc.disconnect().await.unwrap();
        assert!(!mc.is_connected() && !bluez.connected());
        let app_start = b"\x01\x03      mccli".to_vec();
        assert_eq!(bluez.written(), [app_start, vec![20]]);
    });
}

#[test]
fn a_scan_lists_the_companions_in_range() {
    run(a_scan_lists_the_companions_in_range, async |bluez| {
        bluez.add_device("AA:BB:CC:DD:EE:01", "MeshCore-Connected", None, true);
        bluez.add_device("AA:BB:CC:DD:EE:02", "MeshCore-Gone", None, false);
        bluez.add_device("AA:BB:CC:DD:EE:03", "OnePlus Buds", Some(-40), false);
        let scanning = tokio::spawn(scan(Duration::from_secs(1)));
        sleep(millis(300)).await;
        bluez.add_device("AA:BB:CC:DD:EE:04", "MeshCore-Later", Some(-90), false);
        let mut found: Vec<_> = scanning
            .await
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|device| (device.address, device.name, device.rssi))
            .collect();
        found.sort();
        let device = |last, name: &str, rssi| (format!("AA:BB:CC:DD:EE:{last}"), name.into(), rssi);
        assert_eq!(
            found,
            [
                device("01", "MeshCore-Connected", None),
                device("04", "MeshCore-Later", Some(-90)),
                device("FF", NAME, Some(-50)),
            ]
        );
    });
}

#[test]
fn a_companion_is_found_by_address_and_name() {
    run(a_companion_is_found_by_address_and_name, async |bluez| {
        bluez.add_device(
            "AA:BB:CC:DD:EE:03",
            "Node of another kind",
            Some(-40),
            false,
        );
        let lower = ADDRESS.to_lowercase();
        for text in [Some(ADDRESS), Some(&lower), Some("Node"), Some(NAME), None] {
            let mc = client(BleConfig {
                address: text.map(str::to_string),
                ..BleConfig::default()
            });
            mc.connect().await.unwrap();
            assert_eq!(mc.connection_info().as_deref(), Some(ADDRESS));
            mc.disconnect().await.unwrap();
        }
        for text in ["Elsewhere", "AA:BB:CC:DD:EE:77"] {
            let absent = client(BleConfig::new(text).scan_timeout(millis(300)));
            assert!(matches!(
                absent.connect().await,
                Err(Error::DeviceNotFound(_))
            ));
        }
        bluez.set_powered(false);
        assert!(matches!(
            paired().connect().await,
            Err(Error::DeviceNotFound(_))
        ));
        assert!(matches!(
            scan(millis(100)).await,
            Err(Error::DeviceNotFound(_))
        ));
    });
}

#[test]
fn a_device_without_the_uart_service_is_refused() {
    run(
        a_device_without_the_uart_service_is_refused,
        async |bluez| {
            bluez.script(|script| script.has_uart = false);
            let result = paired().connect().await;
            assert!(matches!(result, Err(Error::DeviceNotFound(_))));
            link_is_down(&bluez).await;
        },
    );
}

#[test]
fn a_companion_is_paired_with_its_pin() {
    run(a_companion_is_paired_with_its_pin, async |bluez| {
        bluez.set_paired(false);
        let reason = failed_pairing(paired().connect().await);
        assert!(reason.ends_with("is not paired with this computer, give its PIN"));
        // nothing that would leave pairing to the desktop
        assert_eq!(bluez.written(), Vec::<Vec<u8>>::new());
        let reason = failed_pairing(with_pin(111_111).connect().await);
        assert!(reason.contains("did not take the PIN"));
        assert!(!bluez.paired() && !bluez.connected());
        let mc = with_pin(123_456);
        mc.connect().await.unwrap();
        assert!(bluez.paired());
        mc.disconnect().await.unwrap();
        // once paired, it needs none
        connected().await.disconnect().await.unwrap();
        assert_eq!(bluez.removals(), 0);
    });
}

#[test]
fn a_companion_that_forgot_the_pairing_is_paired_again() {
    run(
        a_companion_that_forgot_the_pairing_is_paired_again,
        async |bluez| {
            let texts = ["le-connection-abort-by-local", "br-connection-key-missing"];
            for (removals, text) in texts.into_iter().enumerate() {
                bluez.script(|script| script.forgot_pairing = Some(text));
                let reason = failed_pairing(paired().connect().await);
                assert!(reason.contains("may have forgotten the pairing"));
                assert_eq!(bluez.removals(), removals);
                let mc = with_pin(123_456);
                mc.connect().await.unwrap();
                works(&mc).await;
                mc.disconnect().await.unwrap();
                assert_eq!(bluez.removals(), removals + 1);
                assert!(bluez.paired());
            }
            // with the wrong PIN the pairing is gone, and the right one pairs
            bluez.script(|script| script.forgot_pairing = Some(texts[0]));
            let reason = failed_pairing(with_pin(111_111).connect().await);
            assert!(reason.contains("did not take the PIN"));
            with_pin(123_456).connect().await.unwrap();
        },
    );
}

#[test]
fn a_companion_out_of_range_is_not_paired_again() {
    run(
        a_companion_out_of_range_is_not_paired_again,
        async |bluez| {
            bluez.script(|script| {
                script.forgot_pairing = Some("le-connection-abort-by-local");
                script.heard = false;
            });
            let config = BleConfig::new(ADDRESS).pin(Some(123_456));
            let mc = client(config.scan_timeout(millis(300)));
            assert!(matches!(mc.connect().await, Err(Error::DeviceNotFound(_))));
        },
    );
}

#[test]
fn a_link_that_drops_while_it_is_made_is_made_again() {
    run(
        a_link_that_drops_while_it_is_made_is_made_again,
        async |bluez| {
            let text = "le-connection-abort-by-local";
            bluez.script(|script| script.drops = [text, "br-connection-canceled"].into());
            let mc = with_pin(123_456);
            mc.connect().await.unwrap();
            works(&mc).await;
            mc.disconnect().await.unwrap();
            // BlueZ shows the link for a moment
            bluez.script(|script| {
                script.drops = [text].into();
                script.drop_shows = millis(900);
            });
            mc.connect().await.unwrap();
            works(&mc).await;
            mc.disconnect().await.unwrap();
            assert_eq!(bluez.removals(), 0);
            // three times: nothing to pair anew on a device that is not paired
            bluez.set_paired(false);
            bluez.script(|script| {
                script.drops = [text; 3].into();
                script.drop_shows = Duration::ZERO;
            });
            assert!(matches!(mc.connect().await, Err(Error::Ble(_))));
            // any other error is final
            bluez.script(|script| script.drops = ["le-connection-busy"].into());
            assert!(matches!(mc.connect().await, Err(Error::Ble(_))));
            mc.connect().await.unwrap();
            assert_eq!(bluez.removals(), 0);
        },
    );
}

#[test]
fn connecting_gives_up_after_its_timeout() {
    run(connecting_gives_up_after_its_timeout, async |bluez| {
        bluez.script(|script| script.connect_takes = None);
        let mc = client(BleConfig::new(ADDRESS).connect_timeout(millis(300)));
        assert!(matches!(mc.connect().await, Err(Error::Timeout)));
        bluez.script(|script| script.connect_takes = Some(millis(1)));
        mc.connect().await.unwrap();
    });
}

#[test]
fn a_link_of_another_program_is_left_alone() {
    run(a_link_of_another_program_is_left_alone, async |bluez| {
        let other = connected().await;
        let mine = paired();
        let reason = in_use(mine.connect().await);
        assert!(reason.contains("is connected to"));
        works(&other).await;
        other.disconnect().await.unwrap();
        mine.connect().await.unwrap();
        mine.disconnect().await.unwrap();
        // it has the notifications for itself
        bluez.connect();
        bluez.script(|script| script.notify_acquired = true);
        let reason = in_use(mine.connect().await);
        assert!(reason.contains("is connected to"));
        assert!(bluez.connected());
        bluez.script(|script| script.notify_acquired = false);
        bluez.drop_link();
        // it is connecting
        bluez.script(|script| script.connect_takes = None);
        let connecting = tokio::spawn(connected());
        wait_until("the other one connects", || bluez.connecting()).await;
        let reason = in_use(mine.connect().await);
        assert!(reason.contains("is connecting to"));
        bluez.finish_connect();
        works(&connecting.await.unwrap()).await;
    });
}

#[test]
fn a_link_nobody_uses_is_taken_over() {
    run(a_link_nobody_uses_is_taken_over, async |bluez| {
        bluez.connect();
        let mc = connected().await;
        works(&mc).await;
        mc.disconnect().await.unwrap();
        assert!(!bluez.connected());
    });
}

#[test]
fn a_link_bluez_is_taking_down_is_made_anew() {
    run(a_link_bluez_is_taking_down_is_made_anew, async |bluez| {
        bluez.script(|script| script.disconnect_takes = millis(500));
        let other = connected().await;
        let leaving = tokio::spawn(async move { other.disconnect().await });
        wait_until("the other one leaves", || bluez.leaving()).await;
        let mc = connected().await;
        works(&mc).await;
        leaving.await.unwrap().unwrap();
    });
}

#[test]
fn a_lost_link_is_reported() {
    run(a_lost_link_is_reported, async |bluez| {
        let mc = connected().await;
        let mut events = mc.subscribe();
        bluez.drop_link();
        assert_eq!(disconnected(&mut events).await, "ble_disconnect");
        assert!(!mc.is_connected());
        // a command that waits for its reply, and one that starts right after
        mc.connect().await.unwrap();
        bluez.on_write(|_| Reaction::new(vec![], Then::ConfirmAndDrop));
        assert!(matches!(mc.get_time().await, Err(Error::ConnectionLost)));
        bluez.on_write(answer);
        mc.connect().await.unwrap();
        bluez.drop_link();
        let result = mc.get_bat().await;
        assert!(
            matches!(result, Err(Error::ConnectionLost | Error::NotConnected)),
            "{result:?}"
        );
        // BlueZ removes the device
        mc.connect().await.unwrap();
        let mut events = mc.subscribe();
        bluez.remove();
        assert_eq!(disconnected(&mut events).await, "ble_disconnect");
    });
}

#[test]
fn a_command_the_companion_restarts_with_counts_as_sent() {
    run(
        a_command_the_companion_restarts_with_counts_as_sent,
        async |bluez| {
            // in whatever order BlueZ tells it
            let ends = [
                Then::ErrorAndDrop(Duration::ZERO),
                Then::ErrorAndDrop(millis(20)),
                Then::DropAndError(Duration::ZERO),
                Then::DropAndError(millis(2)),
                Then::DropAndError(millis(200)),
            ];
            for then in ends {
                let mc = connected().await;
                let mut events = mc.subscribe();
                bluez.on_write(move |_| Reaction::new(vec![], then));
                mc.reboot().await.unwrap();
                let reason = disconnected(&mut events).await;
                assert!(reason.starts_with("ble_write_failed: "), "{reason}");
                bluez.on_write(answer);

                mc.connect().await.unwrap();
                let token = mc.request_factory_reset();
                bluez.on_write(move |_| Reaction::new(vec![], then));
                mc.confirm_factory_reset(&token).await.unwrap();
                assert_eq!(mc.self_info(), None);
                bluez.on_write(answer);

                // one with a reply gets none
                mc.connect().await.unwrap();
                bluez.on_write(move |_| Reaction::new(vec![], then));
                assert!(matches!(mc.get_bat().await, Err(Error::ConnectionLost)));
                bluez.on_write(answer);
            }
            // a write that does not end
            let mc = connected().await;
            let mut events = mc.subscribe();
            bluez.on_write(|_| Reaction::new(vec![], Then::Drop));
            assert!(matches!(mc.reboot().await, Err(Error::ConnectionLost)));
            assert_eq!(disconnected(&mut events).await, "ble_disconnect");
        },
    );
}

#[test]
fn frames_from_before_a_link_drops_are_read() {
    run(frames_from_before_a_link_drops_are_read, async |bluez| {
        let ends = [
            Then::ConfirmAndDrop,
            Then::DropAndError(Duration::ZERO),
            Then::DropAndError(millis(20)),
            Then::ErrorAndDrop(Duration::ZERO),
            Then::ErrorAndDrop(millis(20)),
        ];
        for then in ends {
            for pushes in [5, 100] {
                let mc = connected().await;
                let mut events = mc.subscribe();
                bluez.on_write(move |_| {
                    let mut frames = vec![MESSAGES_WAITING.to_vec(); pushes];
                    frames.push(BATTERY.to_vec());
                    Reaction::new(frames, then)
                });
                works(&mc).await;
                let mut read = 0;
                while !matches!(next(&mut events).await, Event::Battery(_)) {
                    read += 1;
                }
                assert_eq!(read, pushes, "{then:?}");
                disconnected(&mut events).await;
                bluez.on_write(answer);
            }
        }
    });
}

#[test]
fn a_write_that_does_not_end_takes_the_link_down() {
    run(
        a_write_that_does_not_end_takes_the_link_down,
        async |bluez| {
            let mc = client(BleConfig::new(ADDRESS).write_timeout(millis(300)));
            mc.connect().await.unwrap();
            let mut events = mc.subscribe();
            bluez.on_write(|_| Reaction::new(vec![], Then::Nothing));
            assert!(matches!(mc.get_time().await, Err(Error::ConnectionLost)));
            assert_eq!(disconnected(&mut events).await, "ble_write_timeout");
            link_is_down(&bluez).await;
        },
    );
}

#[test]
fn a_dropped_client_takes_its_link_down() {
    run(a_dropped_client_takes_its_link_down, async |bluez| {
        drop(connected().await);
        link_is_down(&bluez).await;
    });
}

#[test]
fn lost_links_come_back() {
    run(lost_links_come_back, async |bluez| {
        let mut random = Random::new(7);
        for _ in 0..3 {
            let options = Options {
                auto_reconnect: true,
                default_timeout: millis(300),
                ..Options::default()
            };
            let mc = MeshCore::new(BleTransport::new(BleConfig::new(ADDRESS)), options);
            mc.connect().await.unwrap();
            let commands: Vec<_> = (0..3)
                .map(|_| {
                    let mc = mc.clone();
                    tokio::spawn(async move {
                        for _ in 0..20 {
                            match mc.get_bat().await {
                                Ok(battery) => assert_eq!(battery.level, 3540),
                                Err(
                                    Error::ConnectionLost | Error::NotConnected | Error::Timeout,
                                ) => {}
                                Err(err) => panic!("{err}"),
                            }
                        }
                    })
                })
                .collect();
            sleep(random.millis(8)).await;
            bluez.drop_link();
            for command in commands {
                command.await.unwrap();
            }
            let back = async {
                while !(mc.is_connected() && mc.get_bat().await.is_ok()) {
                    sleep(millis(5)).await;
                }
            };
            timeout(2 * PATIENCE, back).await.expect("it stays down");
            mc.disconnect().await.unwrap();
            assert!(!bluez.connected());
        }
    });
}

#[test]
fn connects_that_are_broken_off_leave_no_link() {
    run(connects_that_are_broken_off_leave_no_link, async |bluez| {
        let mut random = Random::new(12_345);
        for round in 0..60 {
            let (connect, disconnect) = (random.micros(10_000), random.micros(20_000));
            bluez.script(|script| {
                script.connect_takes = Some(connect);
                script.disconnect_takes = disconnect;
            });
            let mc = paired();
            let wait = random.micros(25_000);
            match random.below(5) {
                0 => {
                    let _ = timeout(wait, mc.connect()).await;
                }
                1 => {
                    let _ = timeout(wait, mc.connect()).await;
                    mc.disconnect().await.unwrap();
                }
                kind => {
                    let connecting = tokio::spawn({
                        let mc = mc.clone();
                        async move { mc.connect().await }
                    });
                    sleep(wait).await;
                    match kind {
                        2 => mc.disconnect().await.unwrap(),
                        3 => {
                            mc.connect().await.unwrap();
                            works(&mc).await;
                            mc.disconnect().await.unwrap();
                        }
                        _ => {
                            connecting.abort();
                            let _ = timeout(random.millis(5), mc.disconnect()).await;
                        }
                    }
                    let _ = connecting.await;
                }
            }
            drop(mc);
            link_is_down(&bluez).await;
            // and the next one works
            let mc = paired();
            let result = mc.connect().await;
            assert!(result.is_ok(), "round {round}: {result:?}");
            works(&mc).await;
            mc.disconnect().await.unwrap();
            assert!(!bluez.connected());
        }
    });
}
