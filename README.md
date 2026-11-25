# meshcore-rs

A Rust client library for [MeshCore](https://meshcore.co.uk) companion radios
connected over USB (serial), WiFi (TCP) or Bluetooth LE. It is a port of the Python library
[meshcore_py](https://github.com/meshcore-dev/meshcore_py) and keeps its command
names, event types and payload fields.

## Disclaimer

**This project is an independent, community-driven port of [meshcore_py](https://github.com/meshcore-dev/meshcore_py) to Rust.**

- This project is **not affiliated with, endorsed by, or officially associated with MeshCore** or its developers in any way.
- This is provided **as-is, without any warranty** of any kind, express or implied, including but not limited to the warranties of merchantability, fitness for a particular purpose, and noninfringement.
- **No guarantee is made regarding functionality, correctness, reliability, or compatibility** with any particular MeshCore device or firmware version.
- Use at your own risk. The authors are not responsible for any damage to hardware, data loss, or other issues that may arise from using this software.
- Protocol implementation is based on reverse engineering and may be incomplete or incorrect.

## Status

The library follows meshcore_py 2.3.19 and the companion protocol of firmware
1.18.0, including paths with 2 and 3 byte hashes per hop. The parser covers every
response and push code that firmware sends. A parity test runs the same 61 device
frames through meshcore_py and through this library and compares the JSON of
the resulting events; they match except for the deliberate fixes listed below.
The size checks assume frames of 176 bytes, which the firmware takes from 1.16
on; before, it took 172, so there a private message holds at most 159 bytes
(see `MAX_TEXT_LENGTH`).

Serial and TCP are tested end to end against a simulated device on a pseudo
terminal and on a local socket. BLE (feature `ble`) runs through BlueZ and is
available on Linux only. Serial and BLE were tested on a LilyGO T-LoRa V2.1-1.6
with the companion firmware 1.18.0, through meshcore-cli-rs and
`examples/check.rs`: scanning, pairing (also after flashing the device anew),
connecting by name and by address, the read commands, channel messages, login,
trace, reboot, reconnecting, and links BlueZ still held. `examples/check.rs`
runs such checks against any device (see Development).

## Installation

```toml
[dependencies]
meshcore = { git = "https://github.com/chrstnwhlrt/meshcore-rs" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Releases are git tags (`vX.Y.Z`); `tag = "vX.Y.Z"` in the dependency pins one.

Rust 1.89 or newer. Two features are optional:

- `ble` adds the BLE transport. It talks to BlueZ over D-Bus, so it works on
  Linux only and needs libdbus (`pkg-config` and the dbus development files) to
  build; on other systems the feature has no effect. It adds about 50 crates
  (bluer and its D-Bus stack) to the build.
- `serde` adds `serde::Serialize` to events and data types with meshcore_py's
  field names, so `serde_json::to_string(&event)` prints what meshcore_py would
  print for the same frame (keys, key order, hex for keys and tags).

## Quick start

```rust no_run
use meshcore::{MeshCore, Options, SerialConfig};

#[tokio::main]
async fn main() -> meshcore::Result<()> {
    let mc = MeshCore::create_serial(SerialConfig::new("/dev/ttyUSB0"), Options::default()).await?;
    if let Some(info) = mc.self_info() {
        println!("{} ({}), {} dBm", info.name, info.public_key, info.tx_power);
    }

    let battery = mc.get_bat().await?;
    println!("battery: {} mV", battery.level);

    for contact in mc.get_contacts(0).await?.contacts {
        println!("{:<20} {}", contact.name, contact.public_key);
    }

    mc.disconnect().await
}
```

## Connecting

`MeshCore::create_serial` opens the port and starts the session. When the node
does not answer, it tries again with DTR set and then with DTR inverted, as
meshcore_py does; a port that cannot be opened fails at once. On boards with the
usual ESP32 auto-reset circuit, such as the LilyGO T-LoRa V2.1-1.6, a try with
DTR off holds the chip in reset while RTS is on, and Linux turns both lines on
when it opens a port. That try gets no answer, and the board starts again when
the port closes, without the messages waiting in its queue and, if it has no
real-time clock, without its time. The try with DTR set changes nothing on
Linux, where DTR is on already; meshcore_py's tries have the same effects.
`connect` on `MeshCore::new(SerialTransport::new(config), options)` tries once,
with the lines as configured. `SerialConfig` sets the baud rate (115200), DTR
and RTS (left alone by default), the wait after opening (`settle_delay`, 300 ms,
for boards that reset on open) and how long old bytes are read and dropped
(`drain_duration`, 500 ms). `list_ports()` lists the serial ports of the system.

`MeshCore::create_tcp` connects to a WiFi companion (the firmware listens on
port 5000 unless built otherwise):

```rust no_run
use meshcore::{MeshCore, Options};

#[tokio::main]
async fn main() -> meshcore::Result<()> {
    let options = Options {
        auto_reconnect: true,
        ..Options::default()
    };
    let mc = MeshCore::create_tcp("192.168.1.50", 5000, options).await?;
    println!("{:?}", mc.send_device_query().await?.model);
    Ok(())
}
```

Like meshcore_py, the client counts the frames it sends and receives over TCP.
When the next frame would be the fifth more sent than received, it takes the
link for dead instead of sending, and reports `tcp_no_response`.

`MeshCore::create_ble` (feature `ble`) finds a companion by its address, by a
part of its name (it advertises as `MeshCore-<node name>`) or as the first one
in range, as meshcore_py does. A companion advertises only while no other app
(such as the phone app) is connected to it.

```rust ignore
use meshcore::{BleConfig, MeshCore, Options};

let config = BleConfig::new("Hamburg").pin(Some(123456));
let mc = MeshCore::create_ble(config, Options::default()).await?;
```

A device that is not paired with this computer needs its PIN, which it shows on
its display or has set in its firmware; once paired, it needs none. Without a
PIN the library writes nothing to an unpaired device, because MeshCore takes
frames only over a paired link and leaving the pairing to the desktop's agent
can leave BlueZ stuck. A missing or wrong PIN ends in `Error::PairingFailed`.
After the device was flashed anew, it has forgotten the pairing BlueZ still
keeps; with the PIN the library removes the old pairing and pairs again.

A device this computer is still connected to does not advertise, but the scan
lists it (without RSSI) and a name search finds it. The library uses such a
link when no program uses it, as after a program ended without disconnecting.
While another program on this computer gets the device's frames over it,
connecting fails with `Error::DeviceInUse` rather than mixing the frames of both
programs. When a program has just let go of the device, BlueZ shows the link as
connected for 2 s more but no longer serves it, so the library waits for that
link to go and connects anew.

BlueZ keeps a link up after its program is gone, and the device does not
advertise meanwhile. So call `disconnect` before the program ends; it waits the
2 s BlueZ takes, and a shorter timeout around it is fine. A client dropped
without it, or a connect given up halfway, takes its link down in the
background, which needs the Tokio runtime to keep running for a moment.

`meshcore::transport::ble::scan` lists the MeshCore devices in range. Frames go
one per write (with response) and one per notification; a write that takes
longer than 10 s takes the link down, as in meshcore_py.

`Options` holds the default command timeout (15 s), automatic reconnecting (off;
up to 3 attempts a second apart, a new link starts with `APP_START` again) and
the event buffer (1024 events per subscriber). Other links can implement
`Transport` and go to `MeshCore::new`.

## Commands

Every command is an async method named as in meshcore_py. It waits for the
device's reply and returns it typed; a device error comes back as
`Error::Device` with the firmware's `ErrorCode`. One command is in flight at a
time, so a `MeshCore` (it is `Clone`) can be shared between tasks.

| Area | Methods |
|------|---------|
| Device | `send_appstart`, `send_device_query`, `get_bat`, `get_time`, `set_time`, `reboot`, `get_stats_core`, `get_stats_radio`, `get_stats_packets`, `get_self_telemetry`, `run_cli_command` |
| Settings | `set_name`, `set_coords`, `set_tx_power`, `set_radio`, `get_tuning`, `set_tuning`, `set_other_params` and its single setters such as `set_manual_add_contacts`, `set_devicepin`, `get_custom_vars`, `set_custom_var`, `get_allowed_repeat_freq`, `get_path_hash_mode`, `set_path_hash_mode`, `get_autoadd_config`, `set_autoadd_config` |
| Contacts | `get_contacts`, `get_contact_by_key`, `add_contact`, `update_contact`, `remove_contact`, `reset_path`, `change_contact_path`, `change_contact_flags`, `share_contact`, `export_contact`, `import_contact`, `get_advert_path`, `send_advert` |
| Messages | `send_msg`, `send_msg_with_retry`, `send_cmd`, `get_msg`, `fetch_messages`, `start_auto_message_fetching` |
| Channels | `get_channel`, `set_channel`, `send_chan_msg`, `send_channel_data`, `set_flood_scope`, `set_default_flood_scope`, `get_default_flood_scope` |
| Remote nodes | `send_login_sync`, `send_logout`, `req_status_sync`, `req_telemetry_sync`, `req_mma_sync`, `req_acl_sync`, `req_neighbours_sync`, `fetch_all_neighbours`, `req_owner_info_sync`, `req_subscribe_sync`, `req_unsubscribe_sync`, `req_binary_sync`, `req_regions_sync`, `req_owner_sync`, `req_basic_sync`, `send_path_discovery_sync`, `trace_sync`, `send_node_discover_req`, `has_connection` |
| Keys, reset | `export_private_key`, `import_private_key`, `sign`, `request_factory_reset` with `confirm_factory_reset` |
| Raw | `send_raw_packet`, `send_raw_data`, `send_control_data` |

### Messages and channels

```rust no_run
use meshcore::{Event, MeshCore, Options, RetryOptions, SerialConfig};

#[tokio::main]
async fn main() -> meshcore::Result<()> {
    let mc = MeshCore::create_serial(SerialConfig::new("/dev/ttyUSB0"), Options::default()).await?;
    mc.get_contacts(0).await?;

    // Sends up to three times and floods after two unanswered direct
    // attempts; None if no ACK came back.
    if let Some(alice) = mc.get_contact_by_name("Alice") {
        let acked = mc.send_msg_with_retry(&alice, "Hello!", RetryOptions::default()).await?;
        println!("acknowledged: {}", acked.is_some());
    }

    // A hashtag channel takes its key from the name; a private channel
    // gets its 16 byte secret instead of None.
    mc.set_channel(1, "#hamburg", None).await?;
    mc.send_chan_msg(1, "Moin!", None).await?;

    // Messages waiting on the device arrive as events.
    let mut events = mc.subscribe();
    mc.start_auto_message_fetching();
    while let Some(event) = events.recv().await {
        match event {
            Event::ContactMessage(msg) => {
                println!("{}: {}", meshcore::hex::encode(msg.pubkey_prefix), msg.text);
            }
            Event::ChannelMessage(msg) => println!("channel {}: {}", msg.channel_idx, msg.text),
            _ => {}
        }
    }
    Ok(())
}
```

### Remote nodes

Requests to repeaters, rooms and sensors go out over the mesh. The `_sync`
methods wait for the answer, which the device matches to the request by a tag;
the other variants return the `SentInfo` and the answer arrives as an event. The
firmware keeps one pending request of each kind (login, status, telemetry, path
discovery, binary or anonymous request) and drops the answer to one that the
next of its kind replaced, so the client sends them one after the other, as
meshcore_py does. An answer that arrives while the client reconnects still
reaches the waiting request.

```rust no_run
use meshcore::{MeshCore, Options, RequestOptions, SerialConfig};

#[tokio::main]
async fn main() -> meshcore::Result<()> {
    let mc = MeshCore::create_serial(SerialConfig::new("/dev/ttyUSB0"), Options::default()).await?;
    mc.get_contacts(0).await?;
    let Some(repeater) = mc.get_contact_by_name("Repeater 1") else {
        return Ok(());
    };
    let key = repeater.public_key;

    mc.send_login_sync(&key, "password", RequestOptions::default()).await?;
    let status = mc.req_status_sync(&key, RequestOptions::default()).await?;
    println!("battery {} mV, noise floor {} dBm", status.bat, status.noise_floor);

    let telemetry = mc.req_telemetry_sync(&key, RequestOptions::default()).await?;
    for reading in &telemetry.readings {
        println!("{} {}: {:?}", reading.channel, reading.lpp_type.name(), reading.value);
    }
    Ok(())
}
```

## Events and state

Every frame from the device becomes an `Event`; an empty frame or a push that
cannot be read is only logged, and a reply that cannot be read becomes an
`Event::Error`. `subscribe()` returns a `Subscription` for all events from
then on, `wait_for()` and `wait_for_event()` wait for one.
`event.kind().as_str()` gives meshcore_py's event type name
(`contact_message`, `acknowledgement`, ...; `contacts_start` and `unknown` are
this library's own).

The client keeps what meshcore_py keeps:

- the device's self info (`self_info()`), updated by the settings commands;
- a contact cache (`contacts()`, `get_contact_by_name()`), replaced by a full
  `get_contacts(0)` and merged by incremental fetches; with
  `set_auto_update_contacts(true)` it follows adverts and path changes;
- contacts the device heard but did not add (`pending_contacts()`);
- channel secrets (from `get_channel` and `set_channel`), so that
  `set_decrypt_channel_logs(true)` decrypts group packets in the RX log and
  adds path and signal to received channel messages.

A command whose link drops fails at once with `Error::ConnectionLost`. As in
meshcore_py, a write that fails takes the link down too. Without automatic
reconnecting the client then sends `Event::Disconnected`.

The library logs through the `log` crate with the target `meshcore`. Where
meshcore_py logs the same case at info level or above, the message has its
wording (spelling included), so a program shows the lines a meshcore_py
program would show. The one exception is a command without a link: it fails
at once with `Error::NotConnected` and logs nothing, where meshcore_py tries
to send and logs that the transport is not connected. Some cases meshcore_py
does not log at that level get lines of the library's own, such as a lost
link. A frame or remote answer the library cannot read is logged with
meshcore_py's error line for it, also where meshcore_py logs nothing at that
level (it fills in zeros for the missing bytes or skips the frame).

## Differences from meshcore_py

The library differs from meshcore_py in these points, most of them where the
firmware behaves differently from what meshcore_py expects or where
meshcore_py leaves a gap:

- The TX power is a signed byte (the firmware takes -9 dBm and up); meshcore_py
  reads 247 for -9, and its `set_tx_power` raises an error for a negative
  power.
- `set_radio` rounds the frequency to whole kHz and the bandwidth to whole Hz;
  meshcore_py cuts off the rest after a float multiplication and sends 869462
  kHz for 869.4625 MHz and 512001 kHz for 512.002 MHz.
- `confirm_factory_reset` sends the command with the `reset` text the firmware
  requires; meshcore_py sends the bare command, which the firmware rejects as
  unsupported.
- MMA readings from sensors use the firmware's LPP table (humidity has 2 bytes
  in 0.1 % steps), not the CayenneLPP one. A series of a type meshcore_py does
  not know, or one cut short, ends the reading, and the series before it are
  kept; meshcore_py then reports no series (`mma_data` is null for an unknown
  type, and an answer cut short is dropped).
- A full contact fetch replaces the cache instead of merging into it, so
  contacts removed on the device disappear. It does so only when the list
  brings as many contacts as the device reports to hold; a shorter one may
  be the late answer to an earlier request since a time and is merged.
- `get_contacts` fails when the list stalls for 5 s; meshcore_py looks every
  5 s whether a contact came, so it waits 5 to 10 s. When such a list goes on
  later, the next `get_contacts` does not take its end for its own answer;
  meshcore_py takes the first end of a list that comes after its request.
- The contact cache follows what the client does to the device's contacts
  (`add_contact`, `update_contact`, `remove_contact`, `reset_path`,
  `change_contact_path`, `change_contact_flags`, `get_contact_by_key`) and
  drops the contacts the device deletes to make room (`ContactDeleted`).
  A contact list coming in meanwhile (as with `get_contacts_async`) does not
  undo such a change with the older copy it brought before.
  meshcore_py's cache gains and loses contacts only through contact lists,
  and its commands change at most the path and flags of a contact it holds.
- An advert or path update that arrives while a contact list comes in leaves
  the contacts marked as changed (`contacts_dirty()`), as the list may hold
  that contact from before the change, so `ensure_contacts(true)` and the
  automatic contact update read again. meshcore_py clears the mark at the end
  of the list; its automatic update reads the list again on every advert and
  path update anyway.
- `self_info()` follows the name, location, TX power and radio settings the
  client sets, in the form the device reports them back: the firmware keeps
  the location as a double and frequency and bandwidth as floats, so some
  coordinates come back a millionth of a degree closer to zero (and a few
  frequencies and bandwidths LoRa does not use 1 kHz or 1 Hz lower).
  meshcore_py updates its self info only for the settings of
  `CMD_SET_OTHER_PARAMS` and takes the others from the next `APP_START`.
- `start_auto_message_fetching` empties the device's message queue right away
  and again after every `connect` and automatic reconnect, as the device
  reports only messages that arrive while an app is connected; meshcore_py
  fetches one message at the start and the rest only after the next
  `MESSAGES_WAITING`, so messages that came during a reconnect wait for the
  next new one.
- `send_login_sync` returns `Error::LoginFailed` as soon as the node rejects
  the password; meshcore_py waits for a successful login only and runs into its
  timeout.
- `import_private_key` returns `Error::Disabled` as soon as the firmware
  answers that importing keys is disabled; meshcore_py waits for `OK` or an
  error only and runs into its timeout.
- `send_cmd` looks up the node type of a bare key in the contact cache to pick
  the text type; meshcore_py assumes a repeater then.
- `send_msg_with_retry` to a key prefix whose contact is not found, not even
  in the contacts read for the path reset, goes on without the reset;
  meshcore_py raises an error there.
- `get_path_hash_mode`, which `change_contact_path` without a hash mode uses,
  fails when the device query fails; meshcore_py takes mode 0 then.
- The single setters of the `CMD_SET_OTHER_PARAMS` settings (such as
  `set_multi_acks`) and `sign` run one after the other when several tasks
  call them at once. In meshcore_py they interleave, so a setter can write
  back a value another one has just changed, and two signing sessions mix.
- A command fails at once when its link drops instead of waiting for its
  timeout.
- `send_trace` refuses an empty path before sending; the firmware only traces
  along at least one hop.
- `fetch_all_neighbours` stops when a page comes back empty; meshcore_py asks
  again forever.
- A neighbours answer to a request sent with `send_binary_req` is read too,
  with the prefix length from the request; meshcore_py logs an error for it.
  Prefixes asked for longer than 32 bytes are read at the 32 the repeater
  sends, where meshcore_py misreads the entries.
- `get_self_telemetry` takes only the answer with the device's own key, not
  a late answer to an earlier telemetry request of another node.
- A contact set to zero-hop for an anonymous request (`send_anon_req`, used
  by `req_regions_sync`, `req_owner_sync`, `req_basic_sync` and their
  `_async` variants) is set back to flood also when the request is given up
  halfway, as by a timeout around it, and when setting zero-hop got no
  answer, which the device may have taken anyway. meshcore_py leaves it at
  zero-hop in both cases.
- `set_channel` keeps the channel's secret for the RX log as `get_channel`
  does; meshcore_py learns secrets from `get_channel` only.
- An RX log packet that was logged before its channel was known, or while
  decryption was off, is decrypted when it comes again; meshcore_py takes
  over the empty entry it logged then.
- A group text in the RX log that ends within its channel hash and MAC, or an
  advert that ends within the fields its flags announce, gets no decoded
  fields; meshcore_py reports the parts it could read (a location from the
  bytes that are left). A group text that matches a known channel but whose
  encrypted part is empty or not a whole number of 16-byte blocks comes
  undecrypted; with decryption on, meshcore_py logs a parse error for it and
  drops the packet.
- A push too short to read is only logged, and a reply too short to read
  ends its command with an error (reason `invalid_frame_length`); meshcore_py
  fills the missing fields of most frames with zeros, or reports a
  `parse_error` where a missing byte raises an exception. A neighbours or
  node discovery answer cut short likewise comes only as its raw
  `BinaryResponse` or `ControlData`.
- A start byte whose header announces more than 300 bytes is skipped alone,
  so a frame that starts right behind it (after console output ending in `>`)
  is still read; meshcore_py drops the whole three byte header and loses that
  frame.
- Names are cut to the 31 bytes the firmware keeps, at a character boundary:
  contact names, which meshcore_py writes uncut (a name over 32 bytes shifts
  the fields after it), and node and channel names, which the firmware would
  cut in the middle of a character.
- A command longer than the 176 bytes the firmware takes is refused before it
  is sent; meshcore_py sends it, and the firmware cuts it short over serial and
  drops it over BLE.
- A text the firmware would not send whole is refused too: a private message
  or CLI command to a node over 160 bytes (`MAX_TEXT_LENGTH`; a private
  message from its fifth attempt on over 158), which the firmware answers
  with `ERR_CODE_TABLE_FULL`, a channel message that does not fit 160 bytes
  together with the node name and `": "`, and a login password over 15 bytes;
  the firmware cuts these two short.
- Input the firmware would answer as an unknown command is refused as well:
  an empty private message, node name or CLI command to a node, a CLI command
  on the device (`run_cli_command`) under 2 bytes, a custom variable whose
  name and value have fewer than 2 bytes together, an empty chunk to sign, a
  raw packet under 2 bytes, a contact card under 98 bytes, a control type
  without its top bit and a private key under 64 bytes
  (`import_private_key` takes exactly 64). So is input it would read
  differently: a NUL byte in a text, name or password (it ends them there) and
  a `:` in a custom variable's name (it splits there).
- Input the firmware answers with `ERR_CODE_ILLEGAL_ARG` is refused before
  sending too: coordinates beyond -90 to 90 degrees latitude or -180 to 180
  degrees longitude after rounding to millionths (`set_coords`; the firmware
  checks the rounded values) and a custom variable without a name. meshcore_py
  sends both.
- A path its length byte cannot describe is refused before sending: one that
  is not a whole number of hops, has more than 63 hops or more than 64 bytes,
  or uses hash mode 3 (reserved) or above (`change_contact_path`,
  `send_channel_data` and `send_raw_data`; `parse_path_hex` refuses such a
  hash mode too), and a contact path that does not match its length byte or
  is longer than 64 bytes (`update_contact`). meshcore_py sends such a path.
  It counts only whole hops, so the firmware takes fewer and, in channel
  data, reads the rest as the data type. It packs hop count and hash mode
  into one byte unchecked, so more hops spill into the mode bits, and mode 4
  and above raise an error. A contact path over 64 bytes goes out uncut and
  shifts the fields after it.
- `connect` takes the link down again when the device does not answer
  `APP_START`, and returns the error; meshcore_py's `connect` returns `None`
  and leaves the link up (its `create_*` functions take it down).
- Over serial, the client waits 300 ms after opening the port, then reads and
  drops for 500 ms what the board sent before (boot output, an answer of an
  earlier session); meshcore_py reads from the first byte on, where a stale
  answer can pass for the reply to `APP_START`.
- Over TCP the connection info (`connection_info()`, the `connected` event) is
  `host:port`, where meshcore_py gives the host only, and connecting gives up
  after 10 s (`TcpTransport::connect_timeout`), where meshcore_py waits as
  long as the system does.
- A BLE scan or name search also finds a device this computer is still
  connected to, for example right after another program let go of it;
  meshcore_py finds only advertising devices and misses it for a few seconds.
- Connecting over BLE to a device another program on this computer is
  connected to fails with `Error::DeviceInUse`; meshcore_py joins that link,
  and both programs then get each other's frames.
- Over BLE, a device that is not paired needs the PIN, and one that forgot
  its pairing (flashed anew) is paired again with it; meshcore_py leaves the
  pairing to the desktop's agent and keeps reconnecting to a device that
  forgot it for 60 s.
- BLE works on Linux only (through BlueZ); meshcore_py's BLE (through bleak)
  also runs on macOS and Windows.

## Development

```text
cargo test --all-features   # unit, client, transport, parity and logging tests, README examples
                            # (--all-features includes ble, which needs libdbus)
cargo clippy --all-targets --all-features
```

- `tests/client.rs` runs the client against a simulated device on paused time.
- `tests/transports.rs` runs the TCP transport over a local socket and the
  serial transport over a pseudo terminal (Unix).
- `tests/parity.rs` (feature `serde`) compares with meshcore_py. After updating
  meshcore_py, regenerate its fixtures as described in
  `tests/fixtures/meshcore_py_reader.py`.
- `tests/logging.rs` checks the log lines that follow meshcore_py's wording.

`examples/check.rs` checks a real device with read-only commands over any
transport and prints each result with its time; `--watch` then stays connected
until Ctrl-C and reports link losses and reconnects (it does not fetch
messages):

```text
cargo run --example check -- serial /dev/ttyUSB0
cargo run --example check -- tcp 192.168.1.50:5000
cargo run --example check --features ble -- scan
cargo run --example check --features ble -- ble MeshCore-Hamburg --pin 123456 --watch
```

`MESHCORE_LOG=debug` shows the library's log.

`nix flake check` builds the crate and runs clippy (pedantic, warnings denied)
with and without the optional features, the tests with the newest stable Rust
and with the declared minimum version, rustdoc with warnings denied, and
rustfmt. `nix develop` gives a shell with the toolchain. The flake sees only
files known to git, so new files need a `git add` first.

## License

MIT License - see [LICENSE](LICENSE) file.

## Related Projects

- [meshcore_py](https://github.com/meshcore-dev/meshcore_py) - Python library (reference implementation)
- [meshcore-cli-rs](https://github.com/chrstnwhlrt/meshcore-cli-rs) - Rust CLI for MeshCore (uses this library)
- [meshcore-cli](https://github.com/meshcore-dev/meshcore-cli) - Original Python CLI

## Contributing

Contributions are welcome! Please note that this is an independent community project.

## Acknowledgments

This project is inspired by and based on the protocol implementation in [meshcore_py](https://github.com/meshcore-dev/meshcore_py) by fdlamotte.
