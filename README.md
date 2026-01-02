# meshcore-rs

A Rust client library for interacting with [MeshCore](https://meshcore.co.uk) companion radio nodes.

## Disclaimer

**This project is an independent, community-driven port of [meshcore_py](https://github.com/fdlamotte/meshcore_py) to Rust.**

- This project is **not affiliated with, endorsed by, or officially associated with MeshCore** or its developers in any way.
- This is provided **as-is, without any warranty** of any kind, express or implied, including but not limited to the warranties of merchantability, fitness for a particular purpose, and noninfringement.
- **No guarantee is made regarding functionality, correctness, reliability, or compatibility** with any particular MeshCore device or firmware version.
- Use at your own risk. The authors are not responsible for any damage to hardware, data loss, or other issues that may arise from using this software.
- Protocol implementation is based on reverse engineering and may be incomplete or incorrect.

## Features

- **Async/await API** using Tokio runtime
- **Event-driven architecture** for handling device notifications
- **Type-safe protocol implementation** with comprehensive error handling
- **Serial/USB transport** for companion radio communication
- **Full command set** matching the Python library capabilities

### Supported Operations

| Category | Commands |
|----------|----------|
| **Device** | `app_start`, `device_query`, `get_battery`, `get_time`, `set_time`, `reboot`, `get_stats` |
| **Configuration** | `set_name`, `set_coords`, `set_tx_power`, `set_radio`, `set_tuning`, `set_device_pin`, `set_other_params` |
| **Contacts** | `get_contacts`, `update_contact`, `remove_contact`, `reset_path`, `share_contact`, `export_contact`, `import_contact` |
| **Messaging** | `send_message`, `send_command`, `send_channel_message`, `get_message`, `send_login`, `send_logout` |
| **Channels** | `get_channel`, `set_channel` |
| **Binary Protocol** | `binary_status_request`, `binary_telemetry_request`, `binary_mma_request`, `binary_acl_request`, `binary_neighbours_request` |
| **Path Discovery** | `path_discovery`, `send_trace`, `set_flood_scope`, `node_discover` |
| **Telemetry** | `get_self_telemetry`, `send_telemetry_request` |
| **Security** | `export_private_key`, `import_private_key`, `sign_start`, `sign_data`, `sign_finish` |
| **Custom Variables** | `get_custom_vars`, `set_custom_var` |

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
meshcore = { git = "https://github.com/chrstnwhlrt/meshcore-rs" }
```

### Feature Flags

- `sha2` - Enable SHA256-based flood scope topic hashing

## Quick Start

```rust
use meshcore::MeshCore;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    // Connect to a MeshCore device
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    let info = client.connect().await?;

    println!("Connected to: {}", info.name);
    println!("Public key: {}", info.public_key);

    // Get battery status
    let battery = client.get_battery().await?;
    println!("Battery: {}mV", battery.millivolts);

    // Disconnect
    client.disconnect().await?;
    Ok(())
}
```

## Architecture

The library is organized into several modules:

| Module | Description |
|--------|-------------|
| `client` | High-level `MeshCore` client API |
| `commands` | Command handler for device operations |
| `protocol` | Low-level protocol types (frames, packets, commands) |
| `types` | Data structures (contacts, devices, messages, statistics, telemetry) |
| `transport` | Transport implementations (USB/Serial) |
| `event` | Async event system for handling notifications |
| `error` | Error types and result definitions |

## Usage Examples

### Device Information

```rust
use meshcore::MeshCore;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    let self_info = client.connect().await?;

    println!("Device: {}", self_info.name);
    println!("TX Power: {} dBm", self_info.tx_power);
    println!("Location: {:.6}, {:.6}",
        self_info.latitude.unwrap_or(0.0),
        self_info.longitude.unwrap_or(0.0));

    // Query detailed device info
    let device_info = client.get_device_info().await?;
    println!("Firmware version: {}", device_info.firmware_version);
    if let Some(model) = &device_info.model {
        println!("Model: {}", model);
    }
    if let Some(max_contacts) = device_info.max_contacts {
        println!("Max contacts: {}", max_contacts);
    }

    client.disconnect().await?;
    Ok(())
}
```

### Working with Contacts

```rust
use meshcore::MeshCore;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    // Get contacts
    let contacts = client.get_contacts().await?;

    for contact in contacts.values() {
        println!("{}: {} ({})",
            contact.name,
            contact.public_key,
            if contact.is_flood() { "flood" } else { "direct" });
    }

    client.disconnect().await?;
    Ok(())
}
```

### Sending Messages

```rust
use meshcore::MeshCore;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    // Find a contact by name
    let contacts = client.get_contacts().await?;
    let recipient = contacts.values()
        .find(|c| c.name == "Alice")
        .expect("Contact not found");

    // Send a message (automatically waits for ACK)
    client.send_message(&recipient.public_key, "Hello!").await?;
    println!("Message sent and acknowledged!");

    client.disconnect().await?;
    Ok(())
}
```

### Using the Low-Level Commands API

For more control, use the commands handler directly:

```rust
use meshcore::MeshCore;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    let contacts = client.get_contacts().await?;
    let recipient = contacts.values()
        .find(|c| c.name == "Alice")
        .expect("Contact not found");

    // Get current timestamp
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;

    // Send message with manual ACK handling
    let event = client.commands()
        .send_message(&recipient.public_key, "Hello!", 0, timestamp)
        .await?;

    if let meshcore::Event::MessageSent { expected_ack, timeout_ms } = event {
        println!("Message sent, ACK code: {:08x}", expected_ack);

        // Wait for acknowledgment
        let timeout = Duration::from_millis(u64::from(timeout_ms));
        client.commands().wait_for_ack(expected_ack, timeout).await?;
        println!("Message acknowledged!");
    }

    client.disconnect().await?;
    Ok(())
}
```

### Event Subscription

```rust
use meshcore::{MeshCore, Event};
use tokio::time::Duration;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    // Subscribe to all events
    let mut subscription = client.subscribe();

    // Process events in background
    tokio::spawn(async move {
        while let Some(event) = subscription.recv().await {
            match event {
                Event::ContactMessage(msg) => {
                    println!("Message from {:02x?}: {}",
                        &msg.sender_prefix[..3], msg.text);
                }
                Event::ChannelMessage(msg) => {
                    println!("Channel {} message: {}", msg.channel_index, msg.text);
                }
                Event::Ack(ack) => {
                    println!("ACK received: {:08x}", ack.code);
                }
                _ => {}
            }
        }
    });

    // Keep running...
    tokio::time::sleep(Duration::from_secs(3600)).await;

    client.disconnect().await?;
    Ok(())
}
```

### Channel Operations

```rust
use meshcore::MeshCore;
use sha2::{Sha256, Digest};

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    // Get channel info
    let channel = client.get_channel(0).await?;
    println!("Channel 0: {}", channel.name);

    // Set up a new channel with SHA256-derived key
    let mut hasher = Sha256::new();
    hasher.update(b"#MyChannel");
    let hash = hasher.finalize();
    let mut key = [0u8; 16];
    key.copy_from_slice(&hash[..16]);

    // Use the commands handler for set operations
    client.commands().set_channel(1, "MyChannel", &key).await?;

    // Send a channel message
    client.send_channel_message(1, "Hello channel!").await?;

    client.disconnect().await?;
    Ok(())
}
```

### Binary Protocol Requests

```rust
use meshcore::MeshCore;

#[tokio::main]
async fn main() -> Result<(), meshcore::Error> {
    let mut client = MeshCore::serial("/dev/ttyUSB0");
    client.connect().await?;

    let contacts = client.get_contacts().await?;
    let repeater = contacts.values()
        .find(|c| c.name.contains("Repeater"))
        .expect("No repeater found");

    // Request status via binary protocol (using commands handler)
    let status = client.commands()
        .binary_status_request(&repeater.public_key)
        .await?;
    println!("Status request sent: {:?}", status);

    // Request telemetry
    let telemetry = client.commands()
        .binary_telemetry_request(&repeater.public_key)
        .await?;
    println!("Telemetry request sent: {:?}", telemetry);

    // Request neighbours list
    let neighbours = client.commands()
        .binary_neighbours_request(
            &repeater.public_key,
            20,  // max results
            0,   // offset
            0,   // order by
            6,   // prefix length
        )
        .await?;
    println!("Neighbours request sent: {:?}", neighbours);

    client.disconnect().await?;
    Ok(())
}
```

## Data Types

### SelfInfo

Returned by `connect()`:

```rust
pub struct SelfInfo {
    pub advert_type: u8,
    pub tx_power: u8,
    pub max_tx_power: u8,
    pub public_key: PublicKey,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub radio: RadioConfig,
    pub name: String,
    // ... and more
}
```

### Contact

```rust
pub struct Contact {
    pub public_key: PublicKey,
    pub device_type: ContactType,  // Node, Repeater, Room, Unknown
    pub flags: ContactFlags,        // Trusted, Hidden
    pub out_path_len: i8,          // -1 = flood, 0+ = direct hops
    pub out_path: Bytes,
    pub name: String,
    pub last_advert: u32,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub last_modified: u32,
}
```

### DeviceInfo

Returned by `get_device_info()`:

```rust
pub struct DeviceInfo {
    pub firmware_version: u8,
    pub max_contacts: Option<u16>,
    pub max_channels: Option<u8>,
    pub ble_pin: Option<u32>,
    pub build: Option<String>,
    pub model: Option<String>,
    pub version: Option<String>,
}
```

### Telemetry

Supports LPP-encoded telemetry with various sensor readings:

- Temperature, Humidity, Barometric Pressure
- GPS Location (Latitude, Longitude, Altitude)
- Accelerometer, Gyroscope
- Battery Voltage, Analog/Digital Inputs
- Air Quality, Luminosity
- And more...

### Statistics

Three types of device statistics:

- **Core**: Battery voltage, uptime, error counts, queue lengths
- **Radio**: Noise floor, last RSSI/SNR, TX/RX airtime
- **Packets**: RX/TX totals, flood vs. direct breakdown

## Requirements

- Rust 1.85+ (Edition 2024)
- Tokio async runtime
- USB/Serial access to MeshCore companion device

## License

MIT License - see [LICENSE](LICENSE) file.

## Related Projects

- [meshcore_py](https://github.com/fdlamotte/meshcore_py) - Original Python library (reference implementation)
- [meshcli-rs](https://github.com/chrstnwhlrt/meshcore-cli-rs) - Rust CLI for MeshCore (uses this library)
- [meshcore-cli](https://github.com/meshcore-dev/meshcore-cli) - Original Python CLI

## Contributing

Contributions are welcome! Please note that this is an independent community project.

## Acknowledgments

This project is inspired by and based on the protocol implementation in [meshcore_py](https://github.com/fdlamotte/meshcore_py) by fdlamotte.
