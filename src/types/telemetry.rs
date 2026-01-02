//! Telemetry data types and Cayenne LPP parsing.
//!
//! The Cayenne Low Power Payload (LPP) format is used for sensor data.

use std::collections::HashMap;

/// A single telemetry value.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryValue {
    /// Digital input (0 or 1).
    DigitalInput(u8),
    /// Digital output (0 or 1).
    DigitalOutput(u8),
    /// Analog input (0.01 resolution).
    AnalogInput(f32),
    /// Analog output (0.01 resolution).
    AnalogOutput(f32),
    /// Illuminance in lux.
    Illuminance(u16),
    /// Presence (0 or 1).
    Presence(u8),
    /// Temperature in Celsius (0.1 resolution).
    Temperature(f32),
    /// Relative humidity in % (0.5 resolution).
    Humidity(f32),
    /// Accelerometer values in G (0.001 resolution).
    Accelerometer { x: f32, y: f32, z: f32 },
    /// Barometric pressure in hPa (0.1 resolution).
    Barometer(f32),
    /// Gyrometer values in degrees/s (0.01 resolution).
    Gyrometer { x: f32, y: f32, z: f32 },
    /// GPS location.
    Gps {
        latitude: f64,
        longitude: f64,
        altitude: f32,
    },
    /// Color RGB values.
    Color { r: u8, g: u8, b: u8 },
    /// Voltage in V (0.01 resolution).
    Voltage(f32),
    /// Current in A (0.001 resolution).
    Current(f32),
    /// Frequency in Hz.
    Frequency(u32),
    /// Percentage (0-100).
    Percentage(u8),
    /// Altitude in m (0.01 resolution).
    Altitude(f32),
    /// Power in W.
    Power(u16),
    /// Distance in mm.
    Distance(u32),
    /// Energy in Wh.
    Energy(u32),
    /// Direction in degrees (0-360).
    Direction(u16),
    /// Unix timestamp.
    UnixTime(u32),
    /// Generic value.
    Generic(Vec<u8>),
}

/// A telemetry reading with channel and type info.
#[derive(Debug, Clone)]
pub struct TelemetryReading {
    /// Channel number.
    pub channel: u8,
    /// LPP type code.
    pub lpp_type: u8,
    /// Decoded value.
    pub value: TelemetryValue,
}

/// Collection of telemetry readings.
#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    /// All readings keyed by channel.
    pub readings: Vec<TelemetryReading>,
}

impl Telemetry {
    /// Creates a new empty telemetry collection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            readings: Vec::new(),
        }
    }

    /// Parses Cayenne LPP data.
    #[must_use]
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub fn parse_lpp(data: &[u8]) -> Self {
        let mut telemetry = Self::new();
        let mut pos = 0;

        while pos + 2 <= data.len() {
            let channel = data[pos];
            let lpp_type = data[pos + 1];
            pos += 2;

            let (value, consumed) = match lpp_type {
                // Digital Input
                0 => {
                    if pos < data.len() {
                        (Some(TelemetryValue::DigitalInput(data[pos])), 1)
                    } else {
                        (None, 0)
                    }
                }
                // Digital Output
                1 => {
                    if pos < data.len() {
                        (Some(TelemetryValue::DigitalOutput(data[pos])), 1)
                    } else {
                        (None, 0)
                    }
                }
                // Analog Input (2 bytes, 0.01 signed)
                2 => {
                    if pos + 2 <= data.len() {
                        let raw = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::AnalogInput(f32::from(raw) / 100.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Analog Output (2 bytes, 0.01 signed)
                3 => {
                    if pos + 2 <= data.len() {
                        let raw = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        (
                            Some(TelemetryValue::AnalogOutput(f32::from(raw) / 100.0)),
                            2,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // Illuminance (2 bytes, unsigned)
                101 => {
                    if pos + 2 <= data.len() {
                        let lux = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Illuminance(lux)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Presence
                102 => {
                    if pos < data.len() {
                        (Some(TelemetryValue::Presence(data[pos])), 1)
                    } else {
                        (None, 0)
                    }
                }
                // Temperature (2 bytes, 0.1 signed)
                103 => {
                    if pos + 2 <= data.len() {
                        let raw = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Temperature(f32::from(raw) / 10.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Humidity (1 byte, 0.5 unsigned)
                104 => {
                    if pos < data.len() {
                        (
                            Some(TelemetryValue::Humidity(f32::from(data[pos]) / 2.0)),
                            1,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // Accelerometer (6 bytes, 0.001 signed per axis)
                113 => {
                    if pos + 6 <= data.len() {
                        let x = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        let y = i16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                        let z = i16::from_be_bytes([data[pos + 4], data[pos + 5]]);
                        (
                            Some(TelemetryValue::Accelerometer {
                                x: f32::from(x) / 1000.0,
                                y: f32::from(y) / 1000.0,
                                z: f32::from(z) / 1000.0,
                            }),
                            6,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // Barometer (2 bytes, 0.1 unsigned)
                115 => {
                    if pos + 2 <= data.len() {
                        let raw = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Barometer(f32::from(raw) / 10.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Gyrometer (6 bytes, 0.01 signed per axis)
                134 => {
                    if pos + 6 <= data.len() {
                        let x = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        let y = i16::from_be_bytes([data[pos + 2], data[pos + 3]]);
                        let z = i16::from_be_bytes([data[pos + 4], data[pos + 5]]);
                        (
                            Some(TelemetryValue::Gyrometer {
                                x: f32::from(x) / 100.0,
                                y: f32::from(y) / 100.0,
                                z: f32::from(z) / 100.0,
                            }),
                            6,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // Color (3 bytes RGB)
                135 => {
                    if pos + 3 <= data.len() {
                        (
                            Some(TelemetryValue::Color {
                                r: data[pos],
                                g: data[pos + 1],
                                b: data[pos + 2],
                            }),
                            3,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // GPS (9 bytes: lat 3, lon 3, alt 3)
                136 => {
                    if pos + 9 <= data.len() {
                        // Latitude: 3 bytes signed, 0.0001 degree resolution
                        // Sign extend from 24-bit to 32-bit
                        let lat_raw = if data[pos] & 0x80 != 0 {
                            i32::from_be_bytes([0xFF, data[pos], data[pos + 1], data[pos + 2]])
                        } else {
                            i32::from_be_bytes([0x00, data[pos], data[pos + 1], data[pos + 2]])
                        };
                        let latitude = f64::from(lat_raw) / 10000.0;

                        // Longitude: 3 bytes signed, 0.0001 degree resolution
                        let lon_raw = if data[pos + 3] & 0x80 != 0 {
                            i32::from_be_bytes([0xFF, data[pos + 3], data[pos + 4], data[pos + 5]])
                        } else {
                            i32::from_be_bytes([0x00, data[pos + 3], data[pos + 4], data[pos + 5]])
                        };
                        let longitude = f64::from(lon_raw) / 10000.0;

                        // Altitude: 3 bytes signed, 0.01 meter resolution
                        let alt_raw = if data[pos + 6] & 0x80 != 0 {
                            i32::from_be_bytes([0xFF, data[pos + 6], data[pos + 7], data[pos + 8]])
                        } else {
                            i32::from_be_bytes([0x00, data[pos + 6], data[pos + 7], data[pos + 8]])
                        };
                        let altitude = alt_raw as f32 / 100.0;

                        (
                            Some(TelemetryValue::Gps {
                                latitude,
                                longitude,
                                altitude,
                            }),
                            9,
                        )
                    } else {
                        (None, 0)
                    }
                }
                // Voltage (2 bytes, 0.01 unsigned)
                116 => {
                    if pos + 2 <= data.len() {
                        let raw = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Voltage(f32::from(raw) / 100.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Current (2 bytes, 0.001 unsigned)
                117 => {
                    if pos + 2 <= data.len() {
                        let raw = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Current(f32::from(raw) / 1000.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Frequency (4 bytes unsigned)
                118 => {
                    if pos + 4 <= data.len() {
                        let freq = u32::from_be_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        (Some(TelemetryValue::Frequency(freq)), 4)
                    } else {
                        (None, 0)
                    }
                }
                // Percentage (1 byte)
                120 => {
                    if pos < data.len() {
                        (Some(TelemetryValue::Percentage(data[pos])), 1)
                    } else {
                        (None, 0)
                    }
                }
                // Altitude (2 bytes signed, 0.01)
                121 => {
                    if pos + 2 <= data.len() {
                        let raw = i16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Altitude(f32::from(raw) / 100.0)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Power (2 bytes unsigned)
                128 => {
                    if pos + 2 <= data.len() {
                        let power = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Power(power)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Distance (4 bytes unsigned, mm)
                130 => {
                    if pos + 4 <= data.len() {
                        let dist = u32::from_be_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        (Some(TelemetryValue::Distance(dist)), 4)
                    } else {
                        (None, 0)
                    }
                }
                // Energy (4 bytes unsigned, Wh)
                131 => {
                    if pos + 4 <= data.len() {
                        let energy = u32::from_be_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        (Some(TelemetryValue::Energy(energy)), 4)
                    } else {
                        (None, 0)
                    }
                }
                // Direction (2 bytes unsigned)
                132 => {
                    if pos + 2 <= data.len() {
                        let dir = u16::from_be_bytes([data[pos], data[pos + 1]]);
                        (Some(TelemetryValue::Direction(dir)), 2)
                    } else {
                        (None, 0)
                    }
                }
                // Unix time (4 bytes unsigned)
                133 => {
                    if pos + 4 <= data.len() {
                        let time = u32::from_be_bytes([
                            data[pos],
                            data[pos + 1],
                            data[pos + 2],
                            data[pos + 3],
                        ]);
                        (Some(TelemetryValue::UnixTime(time)), 4)
                    } else {
                        (None, 0)
                    }
                }
                // Unknown type - skip
                _ => {
                    // Try to consume remaining data as generic
                    let remaining = data.len() - pos;
                    if remaining > 0 {
                        (
                            Some(TelemetryValue::Generic(data[pos..].to_vec())),
                            remaining,
                        )
                    } else {
                        (None, 0)
                    }
                }
            };

            if let Some(val) = value {
                telemetry.readings.push(TelemetryReading {
                    channel,
                    lpp_type,
                    value: val,
                });
                pos += consumed;
            } else {
                break;
            }
        }

        telemetry
    }

    /// Returns readings as a hashmap by channel.
    #[must_use]
    pub fn by_channel(&self) -> HashMap<u8, Vec<&TelemetryReading>> {
        let mut map: HashMap<u8, Vec<&TelemetryReading>> = HashMap::new();
        for reading in &self.readings {
            map.entry(reading.channel).or_default().push(reading);
        }
        map
    }

    /// Gets the first temperature reading.
    #[must_use]
    pub fn temperature(&self) -> Option<f32> {
        self.readings.iter().find_map(|r| {
            if let TelemetryValue::Temperature(t) = r.value {
                Some(t)
            } else {
                None
            }
        })
    }

    /// Gets the first humidity reading.
    #[must_use]
    pub fn humidity(&self) -> Option<f32> {
        self.readings.iter().find_map(|r| {
            if let TelemetryValue::Humidity(h) = r.value {
                Some(h)
            } else {
                None
            }
        })
    }

    /// Gets the first GPS reading.
    #[must_use]
    pub fn gps(&self) -> Option<(f64, f64, f32)> {
        self.readings.iter().find_map(|r| {
            if let TelemetryValue::Gps {
                latitude,
                longitude,
                altitude,
            } = r.value
            {
                Some((latitude, longitude, altitude))
            } else {
                None
            }
        })
    }

    /// Gets the first voltage reading.
    #[must_use]
    pub fn voltage(&self) -> Option<f32> {
        self.readings.iter().find_map(|r| {
            if let TelemetryValue::Voltage(v) = r.value {
                Some(v)
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_temperature() {
        // Channel 1, Type 103 (temp), value 0x00FA = 250 = 25.0°C
        let data = [0x01, 0x67, 0x00, 0xFA];
        let telemetry = Telemetry::parse_lpp(&data);

        assert_eq!(telemetry.readings.len(), 1);
        assert_eq!(telemetry.readings[0].channel, 1);
        assert_eq!(telemetry.readings[0].lpp_type, 103);

        if let TelemetryValue::Temperature(t) = telemetry.readings[0].value {
            assert!((t - 25.0).abs() < 0.01);
        } else {
            panic!("Expected temperature value");
        }
    }

    #[test]
    fn test_parse_humidity() {
        // Channel 2, Type 104 (humidity), value 0x64 = 100 = 50.0%
        let data = [0x02, 0x68, 0x64];
        let telemetry = Telemetry::parse_lpp(&data);

        assert_eq!(telemetry.humidity(), Some(50.0));
    }

    #[test]
    fn test_parse_multiple() {
        // Temperature + Humidity
        let data = [
            0x01, 0x67, 0x00, 0xFA, // Temp: 25.0°C
            0x02, 0x68, 0x64, // Humidity: 50.0%
        ];
        let telemetry = Telemetry::parse_lpp(&data);

        assert_eq!(telemetry.readings.len(), 2);
        assert!((telemetry.temperature().unwrap() - 25.0).abs() < 0.01);
        assert!((telemetry.humidity().unwrap() - 50.0).abs() < 0.01);
    }
}
