//! Checks a companion radio with read-only commands, over any transport.
//!
//! ```text
//! cargo run --example check -- serial /dev/ttyUSB0
//! cargo run --example check -- tcp 192.168.1.50:5000
//! cargo run --example check --features ble -- ble [ADDRESS|NAME] [--pin PIN]
//! cargo run --example check --features ble -- scan [SECONDS]
//! ```
//!
//! With `--watch` the program stays connected afterwards, reconnects when the
//! link drops and prints what the device reports until Ctrl-C. It does not
//! fetch messages, so they stay queued for other apps. Ctrl-C at any point
//! leaves no BLE link behind.
//! `MESHCORE_LOG=debug` (or `trace`) prints the library's log to stderr.

use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use meshcore::{Error, ErrorCode, Event, MeshCore, Options};

const USAGE: &str = "usage: check serial PORT | tcp HOST:PORT | ble [ADDRESS|NAME] [--pin PIN] | scan [SECONDS]  [--watch]";

struct StderrLogger;

impl log::Log for StderrLogger {
    // Only the library's lines: mio-serial and bluer log through `log` too.
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "meshcore" && metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "[{:<5} {}] {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_log() {
    if let Ok(level) = std::env::var("MESHCORE_LOG")
        && log::set_logger(&LOGGER).is_ok()
    {
        log::set_max_level(level.parse().unwrap_or(log::LevelFilter::Debug));
    }
}

/// The value after `flag`, if given.
#[cfg(all(feature = "ble", target_os = "linux"))]
fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

async fn connect(args: &[String], options: Options) -> Result<MeshCore, String> {
    let fail = |err: Error| err.to_string();
    match args.first().map(String::as_str) {
        Some("serial") => {
            let port = args.get(1).ok_or(USAGE)?;
            MeshCore::create_serial(meshcore::SerialConfig::new(port.as_str()), options)
                .await
                .map_err(fail)
        }
        Some("tcp") => {
            let target = args.get(1).ok_or(USAGE)?;
            let (host, port) = target.rsplit_once(':').ok_or(USAGE)?;
            let port = port.parse().map_err(|_| format!("{port} is not a port"))?;
            MeshCore::create_tcp(host, port, options)
                .await
                .map_err(fail)
        }
        #[cfg(all(feature = "ble", target_os = "linux"))]
        Some("ble") => {
            let pin = value_of(args, "--pin")
                .map(str::parse)
                .transpose()
                .map_err(|_| "the PIN is a number".to_string())?;
            let config = meshcore::BleConfig {
                address: args.get(1).filter(|arg| !arg.starts_with("--")).cloned(),
                pin,
                ..meshcore::BleConfig::default()
            };
            MeshCore::create_ble(config, options).await.map_err(fail)
        }
        _ => Err(USAGE.into()),
    }
}

#[cfg(all(feature = "ble", target_os = "linux"))]
async fn scan(args: &[String]) -> ExitCode {
    let seconds = args.get(1).and_then(|arg| arg.parse().ok()).unwrap_or(10);
    println!("scanning for {seconds} s ...");
    match meshcore::transport::ble::scan(Duration::from_secs(seconds)).await {
        Ok(devices) if devices.is_empty() => {
            println!("no MeshCore device in range");
            ExitCode::FAILURE
        }
        Ok(devices) => {
            for device in devices {
                let signal = device
                    .rssi
                    .map_or_else(|| "connected".to_string(), |rssi| format!("{rssi} dBm"));
                println!("{}  {signal:>9}  {}", device.address, device.name);
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            println!("scan failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Prints each check with its time and counts the failures. A command the
/// firmware does not support is shown but not counted.
#[derive(Default)]
struct Report {
    failed: usize,
}

impl Report {
    fn show<T>(
        &mut self,
        name: &str,
        started: Instant,
        result: meshcore::Result<T>,
        describe: impl FnOnce(&T) -> String,
    ) -> Option<T> {
        let ms = started.elapsed().as_millis();
        match result {
            Ok(value) => {
                println!("ok    {name:<20} {ms:>5} ms  {}", describe(&value));
                Some(value)
            }
            Err(Error::Device(ErrorCode::UnsupportedCmd)) => {
                println!("n/a   {name:<20} {ms:>5} ms  not supported by this firmware");
                None
            }
            Err(err) => {
                self.failed += 1;
                println!("FAIL  {name:<20} {ms:>5} ms  {err}");
                None
            }
        }
    }
}

macro_rules! check {
    ($report:expr, $name:expr, $call:expr, $describe:expr) => {{
        let started = Instant::now();
        let result = $call.await;
        $report.show($name, started, result, $describe)
    }};
}

async fn run_checks(mc: &MeshCore) -> Report {
    let mut report = Report::default();
    if let Some(info) = mc.self_info() {
        println!(
            "node  {} ({}), {:.3} MHz, BW {} kHz, SF {}, CR {}, {} dBm",
            info.name,
            info.public_key,
            info.radio_freq,
            info.radio_bw,
            info.radio_sf,
            info.radio_cr,
            info.tx_power
        );
    }
    let device = check!(report, "device query", mc.send_device_query(), |d| format!(
        "{} {} (build {}), firmware code {}",
        d.model.as_deref().unwrap_or("?"),
        d.ver.as_deref().unwrap_or("?"),
        d.fw_build.as_deref().unwrap_or("?"),
        d.fw_ver
    ));
    check!(report, "battery", mc.get_bat(), |b| format!(
        "{} mV, storage {:?} of {:?} kB",
        b.level, b.used_kb, b.total_kb
    ));
    check!(report, "time", mc.get_time(), |time| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        format!(
            "{time}, {} s off this computer",
            i64::from(*time) - i64::try_from(now).unwrap_or(0)
        )
    });
    check!(report, "core stats", mc.get_stats_core(), |s| format!(
        "{s:?}"
    ));
    check!(report, "radio stats", mc.get_stats_radio(), |s| format!(
        "{s:?}"
    ));
    check!(report, "packet stats", mc.get_stats_packets(), |s| format!(
        "{s:?}"
    ));
    check!(report, "tuning", mc.get_tuning(), |t| format!("{t:?}"));
    check!(
        report,
        "path hash mode",
        mc.get_path_hash_mode(),
        |m| format!("{m}")
    );
    check!(
        report,
        "auto-add config",
        mc.get_autoadd_config(),
        |c| format!("{c:?}")
    );
    check!(report, "custom vars", mc.get_custom_vars(), |v| format!(
        "{v:?}"
    ));
    check!(
        report,
        "self telemetry",
        mc.get_self_telemetry(),
        |t| format!("{} readings", t.telemetry.readings.len())
    );
    check!(report, "contacts", mc.get_contacts(0), |list| format!(
        "{} contacts",
        list.contacts.len()
    ));
    let channels = device.and_then(|d| d.max_channels).unwrap_or(8);
    let started = Instant::now();
    let mut names = Vec::new();
    let mut result = Ok(());
    for index in 0..channels {
        match mc.get_channel(index).await {
            Ok(channel) if channel.is_empty() => {}
            Ok(channel) => names.push(format!("{index}: {}", channel.channel_name)),
            Err(Error::Device(ErrorCode::NotFound)) => break,
            Err(err) => {
                result = Err(err);
                break;
            }
        }
    }
    report.show("channels", started, result, |()| names.join(", "));
    report
}

async fn watch(mc: &MeshCore) {
    println!("watching, stop with Ctrl-C");
    let mut events = mc.subscribe();
    while let Some(event) = events.recv().await {
        match &event {
            Event::Connected { info, reconnected } => {
                println!("connected to {info} (reconnected: {reconnected})");
            }
            Event::Disconnected { reason } => println!("disconnected: {reason}"),
            Event::MessagesWaiting => println!("messages waiting on the device"),
            Event::Advertisement(key) => println!("advert from {key}"),
            Event::Ack(ack) => println!("ack {:08x}", ack.code),
            Event::RxLog(_) => {}
            other => println!("{}", other.kind().as_str()),
        }
    }
}

/// Whether all checks passed, once they ran (for the exit code after Ctrl-C
/// while watching).
static PASSED: OnceLock<bool> = OnceLock::new();

async fn run(args: &[String]) -> ExitCode {
    #[cfg(all(feature = "ble", target_os = "linux"))]
    if args.first().is_some_and(|arg| arg == "scan") {
        return scan(args).await;
    }
    let watching = args.iter().any(|arg| arg == "--watch");
    let options = Options {
        auto_reconnect: watching,
        ..Options::default()
    };
    let started = Instant::now();
    let mc = match connect(args, options).await {
        Ok(mc) => mc,
        Err(err) => {
            println!(
                "connect failed after {} ms: {err}",
                started.elapsed().as_millis()
            );
            return ExitCode::FAILURE;
        }
    };
    println!(
        "connected to {} in {} ms",
        mc.connection_info().unwrap_or_default(),
        started.elapsed().as_millis()
    );
    let report = run_checks(&mc).await;
    if report.failed == 0 {
        println!("all checks passed");
    } else {
        println!("{} checks failed", report.failed);
    }
    let _ = PASSED.set(report.failed == 0);
    if watching {
        watch(&mc).await;
    }
    if let Err(err) = mc.disconnect().await {
        println!("disconnect failed: {err}");
    }
    if report.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    init_log();
    let args: Vec<String> = std::env::args().skip(1).collect();
    tokio::select! {
        code = run(&args) => code,
        _ = tokio::signal::ctrl_c() => {
            println!();
            // Dropping `run` dropped the client, which takes a BLE link down
            // in the background (BlueZ keeps the link of a killed program
            // up); this gives it a moment before the runtime ends.
            tokio::time::sleep(Duration::from_millis(500)).await;
            if PASSED.get() == Some(&true) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
