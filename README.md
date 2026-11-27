# meshcore-rs

A Rust client library for [MeshCore](https://meshcore.co.uk) companion radios
connected over USB (serial), WiFi or Ethernet (TCP) or Bluetooth LE. It is a
port of the Python library [meshcore_py](https://github.com/meshcore-dev/meshcore_py)
and keeps its command names, event types and payload fields.

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
response and push code that firmware sends. A parity test runs the same 71 device
frames through meshcore_py and through this library, with channel log decryption
on and a binary request of each kind waiting for its answer, and compares the
JSON of the resulting events; they match except for the deliberate fixes listed
below. A second one runs 184 cases of commands through both, against stand-ins
for a device that answer alike (refusing or leaving unanswered the same
commands where a case says so), and compares the frames they write. In 122
cases they write the same frames byte for byte, the random nonce of a
neighbours request aside (five of these write none: meshcore_py raises an
error, this library refuses the input). The other 62 show the differences
listed below.
The size checks assume frames of 176 bytes, which the firmware takes from 1.16
on; before, it took 172, so there a private message holds at most 159 bytes
(see `MAX_TEXT_LENGTH`).

Serial and TCP are tested end to end against a simulated device on a pseudo
terminal and on a local socket, BLE (feature `ble`, through BlueZ and on Linux
only) against a stand-in for BlueZ. Serial and BLE were also tested on a LilyGO
T-LoRa V2.1-1.6 with the companion firmware 1.18.0, through meshcore-cli-rs and
`examples/check.rs`: scanning, pairing (also after flashing the device anew),
connecting by name and by address, the read commands, channel messages, login,
trace, reboot, reconnecting, and links BlueZ still held. BLE ran on that board
last with the code of v0.10.15. The code of v0.10.22 ran on a LilyGO T-Echo
(nRF52840 with native USB, OTAFIX bootloader 0.9.2-OTAFIX2.3) with the
companion firmware 1.18.0, over USB and BLE: opening and closing the port
without a restart, DTR off and `create_serial`'s tries, 1200 baud, reboot and
factory reset with reconnecting, pairing with the PIN from its display, pairing
again after the device or BlueZ forgot it, connecting by name and by address,
links BlueZ still held or a killed program left, another program's link
(`DeviceInUse`), fetching messages, a trace over a repeater nearby, and, with
the T-LoRa, direct and channel messages both ways, path discovery and
telemetry. Two BLE changes since
v0.10.15 ran against the stand-in only: later links of a client given a name
going to the device found first, and a clone finding its device anew.
`examples/check.rs` runs such checks against any device (see Development).

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
does not answer, it tries again with DTR set (unless `SerialConfig` sets it) and
then with DTR inverted, as meshcore_py does; a port that cannot be opened fails
at once. On boards with the usual ESP32 auto-reset circuit, such as the LilyGO
T-LoRa V2.1-1.6, a try with DTR off holds the chip in reset while RTS is on, and
Linux turns both lines on when it opens a port. That try gets no answer, and the
board starts again when the port closes, without the messages waiting in its
queue. If it has no real-time clock, its time is lost with firmware before 1.18
and goes on from where it stopped with 1.18. The try with DTR set changes nothing
on Linux, where DTR is on already; meshcore_py's tries have the same effects.
`connect` on `MeshCore::new(SerialTransport::new(config), options)` tries once,
with the lines as configured. `SerialConfig` sets the baud rate (115200), DTR and
RTS (left alone by default), the wait after opening (`settle_delay`, 300 ms, for
boards that reset on open) and how long old bytes are read and dropped
(`drain_duration`, 500 ms). `list_ports()` lists the serial ports of the system.

`MeshCore::create_tcp` connects to a WiFi or Ethernet companion. The firmware
listens on port 5000 unless built otherwise and serves one TCP client: a new
one takes the place of the one before.

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

From firmware 1.17 on a companion can serve several links at once (such as BLE
and WiFi, depending on the build) and sends every frame over all of them. With
another app on another link, the client gets that app's replies too and can
take one for its own.

`MeshCore::create_ble` (feature `ble`) finds a companion by its address, by a
part of its name (it advertises as `MeshCore-<node name>`) or as the first one
in range, as meshcore_py does. A reconnect or a later `connect` goes to the
device found first, by its address. A companion advertises only while no other
app (such as the phone app) is connected to it over BLE.

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
keeps. BlueZ then only tells that the link drops while it is being made, as it
may also do when the radio link is bad. When that happens three times in a row,
the library removes the old pairing and pairs again if it has the PIN; without
it, connecting ends in `Error::PairingFailed`.

A device this computer is still connected to does not advertise, but the scan
lists it (without RSSI) and a name search finds it. The library uses such a
link when no program uses it, as after a program ended without disconnecting.
While another program on this computer gets the device's frames over it,
connecting fails with `Error::DeviceInUse` rather than mixing the frames of both
programs, and so it does while another program is connecting. When a program has
just let go of the device, BlueZ shows the link as connected for 2 s more but no
longer serves it, so the library waits for that link to go and connects anew.

BlueZ keeps a link up after its program is gone, and the device does not
advertise meanwhile. So call `disconnect` before the program ends; it waits the
2 s BlueZ takes, and a shorter timeout around it is fine. A client dropped
without it, or a connect given up or stopped while it opens the link, takes the
link down in the background, which needs the Tokio runtime to keep running for a
moment. A link BlueZ reports gone is not taken down again: coming late, that
would end the next link to the device, such as another program's.

`meshcore::transport::ble::scan` lists the MeshCore devices in range. Frames go
one per write (with response) and one per notification; a write that takes
longer than 10 s takes the link down, as in meshcore_py.

`Options` holds the default command timeout (15 s), automatic reconnecting (off;
up to 3 attempts a second apart, a new link starts with `APP_START` again) and
the event buffer (1024 events per subscriber, which is also the least). Other
links can implement `Transport` and go to `MeshCore::new`.

## Commands

Every command is an async method, named as in meshcore_py (a few, such as
`trace_sync` and `fetch_messages`, are this library's own). It waits for the
device's reply (`reboot` has none) and returns it typed; a device error comes
back as `Error::Device` with the firmware's `ErrorCode`. One command is in
flight at a time, so a `MeshCore` (it is `Clone`) can be shared between tasks.
Replies do not name their command: one that comes after its command was given
up can pass for the reply to the next, as in meshcore_py.

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
methods wait for the answer; the other variants return the `SentInfo` and the
answer arrives as an event. The firmware keeps one pending request (login,
status, telemetry, path discovery, binary or anonymous request), and the next
one replaces it whatever its kind. From 1.18 on the answer to a replaced request
still comes, but only as a `BinaryResponse`; before, the device drops it. So the
`_sync` methods of these requests run one after the other, as in meshcore_py;
the other variants are not held back. With automatic reconnecting, a request
goes on waiting when its link drops, and an answer the device sends after the
reconnect still reaches it.

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
  `set_auto_update_contacts(true)` it follows adverts and path changes, after
  a reconnect also those from meanwhile;
- contacts the device heard but did not add (`pending_contacts()`);
- channel secrets (from `get_channel` and `set_channel`), so that
  `set_decrypt_channel_logs(true)` decrypts group packets in the RX log and
  adds path and signal to received channel messages.

It forgets all that when another device answers `APP_START` (one with another
public key, unless the client imported it) and after a factory reset.

A command whose link drops fails at once with `Error::ConnectionLost`. As in
meshcore_py, a write that fails takes the link down too. Without automatic
reconnecting the client then sends `Event::Disconnected`; with it, it does so
once the attempts have failed, or when a `disconnect` or `connect` ends them.

The library logs through the `log` crate with the target `meshcore`. Where
meshcore_py logs the same case at info level or above, the message has its
wording (spelling included), so a program shows the lines a meshcore_py
program would show. A command without a link fails at once with
`Error::NotConnected` and logs nothing, where meshcore_py tries to send and
logs that the transport is not connected. Input the library refuses before
sending comes back as `Error::InvalidArgument` without a line either.
meshcore_py logs a trace path it cannot read and sends most other such input,
after which `send_msg_with_retry` logs the device's refusal of each attempt.
Some cases meshcore_py does not log at that level get lines of the library's
own, such as a lost link. A frame or remote answer the library cannot read is
logged with meshcore_py's error line for it, also where meshcore_py logs
nothing at that level (it fills in zeros for the missing bytes or skips the
frame). A status or subscription answer, for which meshcore_py has no such
line, gets one in the form of those (`Error parsing binary STATUS response:
...`).

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
  in 0.1 % steps, voltage and current are unsigned), not the CayenneLPP one;
  meshcore_py takes voltages above 327.67 V and currents above 32.767 A for
  negative there. A series of a type meshcore_py does not know, or one cut
  short, ends the reading, and the series before it are kept; meshcore_py
  then reports no series (`mma_data` is null for an unknown type, and an
  answer cut short is dropped).
- A full contact fetch replaces the cache instead of merging into it, so
  contacts removed on the device disappear. It does so only when the list
  brings as many contacts as the device reports to hold (or is as short as the
  one before: firmware 1.16.0 counts entries it does not list); a shorter one
  may be the late answer to an earlier request since a time and is merged.
  An incremental fetch does not bring the removal of a contact; when it leaves
  the cache with another number of contacts than the device holds, the contacts
  stay marked as changed and the next fetch is a full one. meshcore_py's cache
  keeps removed contacts.
- A contact list the client did not ask for (another app's on another link),
  or one during which it removes a contact (the firmware then leaves out
  another), is merged but does not count as read; meshcore_py goes on from its
  `lastmod` and can miss what changed before or was left out. A list with a
  contact that cannot be read does not count as read either, and the next one
  is read in full; `get_contacts` fails with `invalid_frame_length` once that
  list has ended, with the rest of it in the cache. meshcore_py lists such a
  contact with zeros for what is missing (an empty name, say), or ends
  `get_contacts` at once with a `parse_error` when the contact ends before its
  flags, while the rest of the list keeps coming; it takes over the list's
  `lastmod` all the same.
- `get_contacts_async` waits for the start of the list, so a refusal (while
  another list is coming in) is its own error; in meshcore_py the refusal
  can pass for the reply to the next command.
- `get_contacts` fails when the list stalls for 5 s; meshcore_py looks every
  5 s whether a contact came, so it waits 5 to 10 s. When such a list goes on
  later, the next `get_contacts` does not take its end for its own answer;
  meshcore_py takes the first end of a list that comes after its request.
- The contact cache follows what the client does to the device's contacts
  (`add_contact`, `update_contact`, `remove_contact`, `reset_path`,
  `change_contact_path`, `change_contact_flags`, `get_contact_by_key`) and
  drops the contacts the device deletes to make room (`ContactDeleted`).
  A contact list coming in meanwhile (as with `get_contacts_async`) does not
  undo such a change with the older copy it brought before. When such a command
  went out and ends without an answer (its link drops, it runs into its timeout
  or is given up), the device may have taken it, so the contact counts as
  changed until a list brings it or a full one was read. So does a cached
  contact for which the device refuses one, as it may not hold the contact any
  more.
  meshcore_py's cache gains contacts only through contact lists and never
  loses one; its commands change at most the path and flags of a contact it
  holds.
- When another device answers `APP_START`, and after a factory reset, the
  client forgets what it held of the one before; meshcore_py keeps it and
  merges the contacts of both.
- An advert or path update that arrives while a contact list comes in leaves
  the contacts marked as changed (`contacts_dirty()`), as the list may hold
  that contact from before the change, so `ensure_contacts(true)` and the
  automatic contact update read again. meshcore_py clears the mark at the end
  of the list; its automatic update reads the list again on every advert and
  path update anyway.
- A new link marks the contacts as changed too, as the device reports adverts
  and path updates only while an app is connected. The automatic contact
  update, once a list was read, asks for what changed since when the session
  started; meshcore_py's waits for the next advert or path update.
- A contact list since a time that brings nothing ends with the `lastmod` 0;
  the client goes on from the time it had, where meshcore_py takes over the 0
  and reads in full the next time. A change the device reported that a list
  lacks (it came in the second the list starts after, or the device's clock was
  set back) leaves the contacts marked as changed, and `ensure_contacts(true)`
  and the automatic update read in full right away; meshcore_py does not look
  for it. `ensure_contacts` also reads in full when the cache is empty, not
  since the last `lastmod`.
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
- `send_login_sync` returns `Error::LoginFailed` as soon as the device reports
  a refused login; meshcore_py waits for a successful login only and runs into
  its timeout. (Repeaters, room servers and sensors with firmware 1.18 do not
  answer a wrong password, which ends in `Error::Timeout`; a room server set to
  allow read-only access logs the client in read-only instead.)
- `import_private_key` returns `Error::Disabled` as soon as the firmware
  answers that importing keys is disabled; meshcore_py waits for `OK` or an
  error only and runs into its timeout.
- `send_cmd` looks up the node type of a bare key in the contact cache to pick
  the text type; meshcore_py assumes a repeater then.
- `send_msg_with_retry` to a key prefix whose contact is not found, not even
  in the contacts read for the path reset, goes on without the reset;
  meshcore_py raises an error there.
- `get_path_hash_mode`, which `change_contact_path` without a hash mode uses,
  fails when the device query fails; meshcore_py takes mode 0 then. An
  anonymous request takes mode 0 then too, as meshcore_py does, for the
  zero-hop path it sets: zero hops have no size a mode could give.
- Commands from several tasks go out one after the other, so each gets its
  own reply; meshcore_py sends them at once, where one's `OK` or error can end
  another. The single setters of the `CMD_SET_OTHER_PARAMS` settings (such as
  `set_multi_acks`) and `sign`, which take several commands, run one after the
  other as a whole. In meshcore_py they interleave, so a setter can write back
  a value another one has just changed, and two signing sessions mix.
- A command fails at once when its link drops instead of waiting for its
  timeout.
- `send_trace` refuses an empty path before sending, as the firmware only
  traces along at least one hop; meshcore_py sends a zero byte as its hop.
  It also refuses a path that is no whole number of hops of the hash size its
  flags give, or has more than 64 hops, which the firmware answers with
  `ERR_CODE_ILLEGAL_ARG`; meshcore_py sends such a path given as bytes.
- `fetch_all_neighbours` stops when a page comes back empty; meshcore_py asks
  again forever.
- A neighbours answer to a request sent with `send_binary_req` is read too,
  with the prefix length from the request; meshcore_py logs an error for it.
  Prefixes asked for longer than 32 bytes are read at the 32 the repeater
  sends, where meshcore_py misreads the entries.
- `get_channel`, `get_contact_by_key`, `send_login_sync`,
  `send_path_discovery_sync` and `get_self_telemetry` take only the answer for
  the channel, key or node they asked about (the last one the device itself).
  meshcore_py takes the first of the kind, such as a contact of a list that is
  coming in or a late answer of another node.
- An anonymous request (`send_anon_req`, used by `req_regions_sync`,
  `req_owner_sync`, `req_basic_sync` and their `_async` variants) takes its
  reply path from the device's own record of the node, along which the device
  sends. meshcore_py takes it from its contact cache, where the contact may be
  missing (it then asks for a zero-hop reply) or have an older path, and reads
  the record only for a contact cached as flood. A contact set to zero-hop for
  such a request is set back to flood also when the request is given up
  halfway, as by a timeout around it, and when setting zero-hop got no answer,
  which the device may have taken anyway. meshcore_py leaves it at zero-hop in
  both cases.
- `req_owner_sync` takes an answer without a line break for the name alone,
  with no owner info, and `req_owner_info_sync` leaves the fields empty that
  an answer with fewer than two line breaks lacks; meshcore_py fails on both
  with an `IndexError`. Repeaters always send the line breaks.
- `set_channel` keeps the channel's secret for the RX log as `get_channel`
  does; meshcore_py learns secrets from `get_channel` only.
- An RX log packet that was logged before its channel was known, or while
  decryption was off, is decrypted when it comes again; meshcore_py takes
  over the empty entry it logged then.
- A channel message in a v3 frame loses the NUL bytes at either end of its
  text, as in the older frame, and its hash for the RX log is taken without
  them, as the log's own is; meshcore_py keeps them in v3 frames (the
  firmware sends none).
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
  `parse_error` where a missing byte raises an exception. meshcore_py drops a
  battery reply without its level and a private key reply cut short, so there
  `get_bat` and `export_private_key` run into its timeout. A channel datagram
  with less data than its length byte announces is such a reply too;
  meshcore_py reports the bytes there are, with the length byte as `data_len`.
  A neighbours answer cut short likewise comes only as its raw `BinaryResponse`,
  and a node discovery answer that ends before 8 bytes of the node's key (the
  firmware's display needs them too) only as its `ControlData`; meshcore_py
  reports such an answer with the bytes there are, an empty key included. A
  subscription answer without the 2 bytes of its expiry ends
  `req_subscribe_sync` with `invalid_frame_length`, where meshcore_py reads
  zeros for the missing bytes. `get_allowed_repeat_freq` leaves out a last
  frequency pair that is cut short, and a contact list whose `lastmod` at the
  end is cut short ends with the `lastmod` 0; meshcore_py fills in zeros for
  the missing bytes of both.
- A trace whose path is no whole number of hops (one another node built; the
  device refuses to send such a path) is read as the firmware sends it: all
  hash bytes, an SNR for each whole hop, then the final SNR. meshcore_py reads
  the SNRs right after the whole hops, where the hash cut short begins, so they
  and the final SNR come from the wrong bytes.
- A start byte whose header announces more than 300 bytes is skipped alone,
  so a frame that starts right behind it (after console output ending in `>`)
  is still read; meshcore_py drops the whole three byte header and loses that
  frame.
- Names are cut to the 31 bytes the firmware keeps, at a character boundary:
  contact names, which meshcore_py writes uncut (a name over 32 bytes shifts
  the fields after it), and node and channel names, which the firmware would
  cut in the middle of a character.
- A contact's name ends at its first NUL byte. When a known contact
  advertises a shorter name, the firmware leaves the rest of the old one
  behind that NUL; meshcore_py removes the NUL bytes and shows that rest as
  part of the name.
- A command longer than the 176 bytes the firmware takes is refused before it
  is sent; meshcore_py sends it, and the firmware cuts it short over serial and
  Ethernet and drops it over BLE and WiFi.
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
  sending too, where the client can tell without asking the device:
  coordinates beyond -90 to 90 degrees latitude or -180 to 180 degrees
  longitude after rounding to millionths (`set_coords`; the firmware checks
  the rounded values), a custom variable without a name, a BLE PIN other than
  0 or 100000 to 999999 (`set_devicepin`), radio settings beyond 150 to 2500
  MHz, 7 to 500 kHz, SF 5 to 12 or CR 5 to 8 (`set_radio`), a TX power below
  -9 dBm or above the device's maximum (`set_tx_power`) and a path hash mode
  above 2 (`set_path_hash_mode`). A time before the device's own (`set_time`)
  and repeating on a frequency the device does not allow (`set_radio`) go out.
  meshcore_py sends all of them, except a negative TX power, frequency or
  bandwidth, which it cannot encode and raises an error for.
- A path its length byte cannot describe is refused before sending: one that
  is not a whole number of hops, has more than 63 hops or more than 64 bytes,
  or uses hash mode 3 (reserved) or above (`change_contact_path`,
  `send_channel_data` and `send_raw_data`; `parse_path_hex` refuses such a
  hash mode too), and a contact path that does not match its length byte (a
  contact that floods has none) or is longer than 64 bytes (`update_contact`).
  meshcore_py sends such a path. In `change_contact_path` and
  `send_channel_data` it counts only whole hops, so the firmware takes fewer
  and, in channel data, reads the rest as the data type. There it also packs
  hop count and hash mode into one byte unchecked, so more hops spill into the
  mode bits, and mode 4 and above raise an error. A contact path over 64 bytes
  goes out uncut and shifts the fields after it.
- `update_contact` refuses a `lastmod` of 0 before sending; without one
  (`None`) the device stamps the record with its clock. The firmware lists only
  contacts whose `lastmod` is later than the list's `since`, so it would hold
  the contact but leave it out of every list, a full one too, and the cache
  would lose it again with the next list read in full. meshcore_py sends the 0.
- `send_raw_data` takes a hash mode, as `send_channel_data` does, and puts
  hop count and mode into the path's length byte, which is how firmware 1.18
  reads it. meshcore_py's `send_raw_data` takes no hash mode and sends the
  path's length in bytes, as the firmware read it before 1.18. Both are the
  same for 1-byte hashes. Firmware 1.18 reads a path of 2- or 3-byte hashes
  from meshcore_py as one hop per byte, and firmware before 1.18 sends raw
  data along 1-byte hashes only.
- `connect` takes the link down again when the device does not answer
  `APP_START`, and returns the error; meshcore_py's `connect` returns `None`
  and leaves the link up (its `create_*` functions take it down). Automatic
  message fetching stays on through a `connect` that fails, also one that
  cannot open the link, for which meshcore_py stops it. A `connect` while
  connected closes the link first and reports it gone (`manual_disconnect`)
  before the new one comes up; meshcore_py's serial and TCP connections open a
  second link next to the first, and it reports only the new one.
- A `disconnect` or another `connect` that comes in while a `connect` opens its
  link counts, and the first one fails with `Error::NotConnected`; meshcore_py
  opens that link all the same. One that comes in while the client reconnects
  reports the connection gone; meshcore_py reports nothing then.
- With `max_reconnect_attempts` 0, the `Disconnected` event of a lost link
  serializes with `reconnect_failed` false; meshcore_py reports true there,
  with or without automatic reconnecting, as its count of attempts (none)
  has reached the limit.
- `disconnect` lets the command in flight finish for up to half a second and
  sends none of those that wait for their turn; meshcore_py cuts the one in
  flight off, and a message the automatic fetching has just asked for is lost:
  the device has taken it out of its queue. `disconnect` reports the
  connection gone (`manual_disconnect`) right away; meshcore_py stops its event
  dispatcher first, so its subscribers get that event only with the next
  `connect`, right before `connected`.
- A write that does not finish within 10 s takes the link down over serial and
  TCP too (`serial_write_timeout`, `tcp_write_timeout`); meshcore_py limits
  writes over BLE only and queues the others in memory.
- Over serial, the client waits 300 ms after opening the port, then reads and
  drops for 500 ms what the board sent before (boot output, an answer of an
  earlier session); meshcore_py reads from the first byte on, where a stale
  answer can pass for the reply to `APP_START`.
- Over TCP the connection info (`connection_info()`, the `connected` event) is
  `host:port`, where meshcore_py gives the host only, and connecting gives up
  after 10 s (`TcpTransport::connect_timeout`), where meshcore_py waits as
  long as the system does.
- Over BLE, a reconnect or a later `connect` goes to the device found first,
  by its address. meshcore_py keeps that device only for the attempts before a
  link is made. Once a link went down, it searches again by the part of the
  name it was given, or for the first device in range, and can reach another
  device than the one it lost, although its comment says it reconnects to the
  same one.
- A BLE scan or name search also finds a device this computer is still
  connected to, for example right after another program let go of it;
  meshcore_py finds only advertising devices and misses it for a few seconds.
- Connecting over BLE to a device another program on this computer is
  connected to fails with `Error::DeviceInUse`; meshcore_py joins that link,
  and both programs then get each other's frames.
- Over BLE, a device that is not paired needs the PIN, and one that forgot
  its pairing (flashed anew) is paired again with it. meshcore_py leaves the
  pairing to the desktop's agent (bleak gets no PIN from it) and does not pair
  a device again that BlueZ holds as paired, so one that forgot its pairing
  stays unusable until it is removed from BlueZ.
- BLE works on Linux only (through BlueZ); meshcore_py's BLE (through bleak)
  also runs on macOS and Windows.

## Development

```text
cargo test --all-features   # unit and integration tests, README examples
                            # (--all-features includes ble, which needs libdbus)
cargo clippy --all-targets --all-features
```

- `tests/client.rs` runs the client against a simulated device, all but one test
  on paused time.
- `tests/contacts.rs` checks the contact cache against a model of the firmware's
  contact table, with random events and commands.
- `tests/transports.rs` runs the TCP transport over a local socket and the
  serial transport over a pseudo terminal (Unix).
- `tests/ble.rs` (feature `ble`, Linux) runs the BLE transport against a stand-in
  for BlueZ, on a D-Bus bus within the test.
- `tests/parity.rs` (feature `serde`) compares the events with meshcore_py's.
  After updating meshcore_py, regenerate its fixtures as described in
  `tests/fixtures/meshcore_py_reader.py`.
- `tests/command_parity.rs` compares the frames the commands write with what
  meshcore_py writes for the same calls. After updating meshcore_py,
  regenerate its fixture as described in
  `tests/fixtures/meshcore_py_commands.py`.
- `tests/logging.rs` checks the log lines at info level and above (a unit
  test those of the retries of `create_serial`, `tests/ble.rs` those of BLE):
  meshcore_py's wording where it logs the same case, lines of the library's
  own where meshcore_py logs nothing at that level (such as for a lost link),
  and none for a command without a link and input refused before sending.

`examples/check.rs` checks a real device with read-only commands over any
transport and prints each result with its time; `--watch` then stays connected
until Ctrl-C and reports link losses and reconnects (it does not fetch
messages). Over serial it connects with `create_serial`, which can restart a
board that does not answer (see Connecting).

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
