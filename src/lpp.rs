//! Cayenne LPP telemetry.
//!
//! Readings are decoded with the type table of pycayennelpp, which meshcore_py
//! uses. Decoding stops at the first reading on channel 0 (encrypted answers
//! are padded with zero bytes) and at the first reading of an unknown type or
//! one cut short; the bytes from there on are kept as `undecoded`.

use crate::types::remote::MmaEntry;

/// The LPP data types meshcore_py knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LppType {
    /// 0: digital input.
    DigitalInput,
    /// 1: digital output.
    DigitalOutput,
    /// 2: analog input (0.01, signed).
    AnalogInput,
    /// 3: analog output (0.01, signed).
    AnalogOutput,
    /// 100: generic sensor (4 bytes).
    GenericSensor,
    /// 101: illuminance in lux.
    Illuminance,
    /// 102: presence.
    Presence,
    /// 103: temperature in °C (0.1, signed).
    Temperature,
    /// 104: relative humidity in % (0.5).
    Humidity,
    /// 113: accelerometer in G (0.001 per axis, signed).
    Accelerometer,
    /// 115: barometric pressure in hPa (0.1).
    Barometer,
    /// 116: voltage in V (0.01).
    Voltage,
    /// 117: current in A (0.001).
    Current,
    /// 118: frequency in Hz.
    Frequency,
    /// 120: percentage.
    Percentage,
    /// 121: altitude in m (signed).
    Altitude,
    /// 122: load (0.001, signed, 3 bytes).
    Load,
    /// 125: concentration in ppm.
    Concentration,
    /// 128: power in W.
    Power,
    /// 130: distance in m (0.001).
    Distance,
    /// 131: energy in kWh (0.001).
    Energy,
    /// 132: direction in degrees.
    Direction,
    /// 133: Unix time.
    Time,
    /// 134: gyrometer in °/s (0.01 per axis, signed).
    Gyrometer,
    /// 135: colour (RGB).
    Colour,
    /// 136: GPS location (lat/lon 0.0001°, altitude 0.01 m).
    Gps,
    /// 142: switch.
    Switch,
}

/// Size, scale and signedness of each value of a type, as pycayennelpp has them.
struct Layout {
    sizes: &'static [usize],
    scales: &'static [f64],
    signed: bool,
}

impl LppType {
    /// All types, in code order.
    pub const ALL: [Self; 27] = [
        Self::DigitalInput,
        Self::DigitalOutput,
        Self::AnalogInput,
        Self::AnalogOutput,
        Self::GenericSensor,
        Self::Illuminance,
        Self::Presence,
        Self::Temperature,
        Self::Humidity,
        Self::Accelerometer,
        Self::Barometer,
        Self::Voltage,
        Self::Current,
        Self::Frequency,
        Self::Percentage,
        Self::Altitude,
        Self::Load,
        Self::Concentration,
        Self::Power,
        Self::Distance,
        Self::Energy,
        Self::Direction,
        Self::Time,
        Self::Gyrometer,
        Self::Colour,
        Self::Gps,
        Self::Switch,
    ];

    /// Maps the type code, or `None` for a type meshcore_py does not know.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::DigitalInput,
            1 => Self::DigitalOutput,
            2 => Self::AnalogInput,
            3 => Self::AnalogOutput,
            100 => Self::GenericSensor,
            101 => Self::Illuminance,
            102 => Self::Presence,
            103 => Self::Temperature,
            104 => Self::Humidity,
            113 => Self::Accelerometer,
            115 => Self::Barometer,
            116 => Self::Voltage,
            117 => Self::Current,
            118 => Self::Frequency,
            120 => Self::Percentage,
            121 => Self::Altitude,
            122 => Self::Load,
            125 => Self::Concentration,
            128 => Self::Power,
            130 => Self::Distance,
            131 => Self::Energy,
            132 => Self::Direction,
            133 => Self::Time,
            134 => Self::Gyrometer,
            135 => Self::Colour,
            136 => Self::Gps,
            142 => Self::Switch,
            _ => return None,
        })
    }

    /// The type code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::DigitalInput => 0,
            Self::DigitalOutput => 1,
            Self::AnalogInput => 2,
            Self::AnalogOutput => 3,
            Self::GenericSensor => 100,
            Self::Illuminance => 101,
            Self::Presence => 102,
            Self::Temperature => 103,
            Self::Humidity => 104,
            Self::Accelerometer => 113,
            Self::Barometer => 115,
            Self::Voltage => 116,
            Self::Current => 117,
            Self::Frequency => 118,
            Self::Percentage => 120,
            Self::Altitude => 121,
            Self::Load => 122,
            Self::Concentration => 125,
            Self::Power => 128,
            Self::Distance => 130,
            Self::Energy => 131,
            Self::Direction => 132,
            Self::Time => 133,
            Self::Gyrometer => 134,
            Self::Colour => 135,
            Self::Gps => 136,
            Self::Switch => 142,
        }
    }

    /// meshcore_py's name for the type (`"temperature"`, `"gps"`, ...).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::DigitalInput => "digital input",
            Self::DigitalOutput => "digital output",
            Self::AnalogInput => "analog input",
            Self::AnalogOutput => "analog output",
            Self::GenericSensor => "generic sensor",
            Self::Illuminance => "illuminance",
            Self::Presence => "presence",
            Self::Temperature => "temperature",
            Self::Humidity => "humidity",
            Self::Accelerometer => "accelerometer",
            Self::Barometer => "barometer",
            Self::Voltage => "voltage",
            Self::Current => "current",
            Self::Frequency => "frequency",
            Self::Percentage => "percentage",
            Self::Altitude => "altitude",
            Self::Load => "load",
            Self::Concentration => "concentration",
            Self::Power => "power",
            Self::Distance => "distance",
            Self::Energy => "energy",
            Self::Direction => "direction",
            Self::Time => "time",
            Self::Gyrometer => "gyrometer",
            Self::Colour => "colour",
            Self::Gps => "gps",
            Self::Switch => "switch",
        }
    }

    /// The type with meshcore_py's name `name` (`lpp_type_from_name`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The encoded size of one value of this type in bytes.
    #[must_use]
    pub fn size(self) -> usize {
        self.layout().sizes.iter().sum()
    }

    #[allow(clippy::match_same_arms)]
    const fn layout(self) -> Layout {
        const fn scalar(size: usize, scale: &'static [f64], signed: bool) -> Layout {
            let sizes: &'static [usize] = match size {
                1 => &[1],
                2 => &[2],
                3 => &[3],
                _ => &[4],
            };
            Layout {
                sizes,
                scales: scale,
                signed,
            }
        }
        match self {
            Self::DigitalInput | Self::DigitalOutput | Self::Presence => scalar(1, &[1.0], false),
            Self::Percentage | Self::Switch => scalar(1, &[1.0], false),
            Self::AnalogInput | Self::AnalogOutput => scalar(2, &[100.0], true),
            Self::GenericSensor | Self::Frequency | Self::Time => scalar(4, &[1.0], false),
            Self::Illuminance | Self::Concentration | Self::Power | Self::Direction => {
                scalar(2, &[1.0], false)
            }
            Self::Temperature => scalar(2, &[10.0], true),
            Self::Humidity => scalar(1, &[2.0], false),
            Self::Barometer => scalar(2, &[10.0], false),
            Self::Voltage => scalar(2, &[100.0], false),
            Self::Current => scalar(2, &[1000.0], false),
            Self::Altitude => scalar(2, &[1.0], true),
            Self::Load => scalar(3, &[1000.0], true),
            Self::Distance | Self::Energy => scalar(4, &[1000.0], false),
            Self::Accelerometer => Layout {
                sizes: &[2, 2, 2],
                scales: &[1000.0, 1000.0, 1000.0],
                signed: true,
            },
            Self::Gyrometer => Layout {
                sizes: &[2, 2, 2],
                scales: &[100.0, 100.0, 100.0],
                signed: true,
            },
            Self::Colour => Layout {
                sizes: &[1, 1, 1],
                scales: &[1.0, 1.0, 1.0],
                signed: false,
            },
            Self::Gps => Layout {
                sizes: &[3, 3, 3],
                scales: &[10000.0, 10000.0, 100.0],
                signed: true,
            },
        }
    }

    /// Decodes one value of this type from exactly [`size`](Self::size) bytes.
    fn decode(self, buf: &[u8]) -> LppValue {
        let layout = self.layout();
        let mut values = [0f64; 3];
        let mut pos = 0;
        for (i, (&size, &scale)) in layout.sizes.iter().zip(layout.scales).enumerate() {
            values[i] = be_value(&buf[pos..pos + size], layout.signed) / scale;
            pos += size;
        }
        match self {
            Self::Accelerometer | Self::Gyrometer => LppValue::Xyz {
                x: values[0],
                y: values[1],
                z: values[2],
            },
            Self::Colour => LppValue::Rgb {
                r: buf[0],
                g: buf[1],
                b: buf[2],
            },
            Self::Gps => LppValue::Gps {
                latitude: values[0],
                longitude: values[1],
                altitude: values[2],
            },
            // meshcore_py reads voltage and current above the signed range as negative
            Self::Voltage | Self::Current => {
                LppValue::Scalar(be_value(buf, true) / layout.scales[0])
            }
            _ => LppValue::Scalar(values[0]),
        }
    }
}

/// A big-endian integer of 1 to 8 bytes as `f64`, two's complement when signed.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
fn be_value(bytes: &[u8], signed: bool) -> f64 {
    let raw = bytes.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
    let bits = bytes.len() * 8;
    if signed && bits < 64 && raw & (1 << (bits - 1)) != 0 {
        (raw as i64 - (1i64 << bits)) as f64
    } else {
        raw as f64
    }
}

/// A decoded LPP value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LppValue {
    /// A single number.
    Scalar(f64),
    /// A three-axis value (accelerometer, gyrometer).
    Xyz {
        /// X axis.
        x: f64,
        /// Y axis.
        y: f64,
        /// Z axis.
        z: f64,
    },
    /// A colour.
    Rgb {
        /// Red.
        r: u8,
        /// Green.
        g: u8,
        /// Blue.
        b: u8,
    },
    /// A GPS location.
    Gps {
        /// Latitude in degrees.
        latitude: f64,
        /// Longitude in degrees.
        longitude: f64,
        /// Altitude in m.
        altitude: f64,
    },
}

impl LppValue {
    /// The number of a scalar value.
    #[must_use]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Scalar(value) => Some(*value),
            _ => None,
        }
    }
}

/// One telemetry reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LppReading {
    /// The LPP channel (1 is the node itself).
    pub channel: u8,
    /// The type.
    pub lpp_type: LppType,
    /// The value.
    pub value: LppValue,
}

/// Telemetry readings, as far as they could be decoded.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Telemetry {
    /// The readings.
    pub readings: Vec<LppReading>,
    /// The bytes from the first reading that could not be decoded (an unknown
    /// type or a reading cut short); empty when everything was decoded.
    pub undecoded: Vec<u8>,
}

impl Telemetry {
    /// Decodes LPP readings (meshcore_py's `lpp_parse_readings`).
    #[must_use]
    pub fn parse(buf: &[u8]) -> Self {
        let mut telemetry = Self::default();
        let mut i = 0;
        while i < buf.len() && buf[i] != 0 {
            let Some(lpp_type) = buf.get(i + 1).and_then(|&code| LppType::from_code(code)) else {
                telemetry.undecoded = buf[i..].to_vec();
                break;
            };
            let size = lpp_type.size();
            if buf.len() - i < 2 + size {
                telemetry.undecoded = buf[i..].to_vec();
                break;
            }
            telemetry.readings.push(LppReading {
                channel: buf[i],
                lpp_type,
                value: lpp_type.decode(&buf[i + 2..i + 2 + size]),
            });
            i += 2 + size;
        }
        telemetry
    }

    /// The first reading of a type.
    #[must_use]
    pub fn first(&self, lpp_type: LppType) -> Option<&LppReading> {
        self.readings.iter().find(|r| r.lpp_type == lpp_type)
    }

    /// The first temperature in °C.
    #[must_use]
    pub fn temperature(&self) -> Option<f64> {
        self.first(LppType::Temperature)
            .and_then(|r| r.value.as_f64())
    }

    /// The first relative humidity in %.
    #[must_use]
    pub fn humidity(&self) -> Option<f64> {
        self.first(LppType::Humidity).and_then(|r| r.value.as_f64())
    }

    /// The first voltage in V (channel 1 is the battery).
    #[must_use]
    pub fn voltage(&self) -> Option<f64> {
        self.first(LppType::Voltage).and_then(|r| r.value.as_f64())
    }

    /// The first GPS location as (latitude, longitude, altitude).
    #[must_use]
    pub fn gps(&self) -> Option<(f64, f64, f64)> {
        match self.first(LppType::Gps)?.value {
            LppValue::Gps {
                latitude,
                longitude,
                altitude,
            } => Some((latitude, longitude, altitude)),
            _ => None,
        }
    }
}

/// Size, multiplier and signedness the firmware's `LPPData` helper (which the
/// sensor uses for min/max/average series) gives a type code.
fn mma_layout(code: u8) -> (usize, f64, bool) {
    let size = match code {
        136 => 9,
        240 => 8,
        113 | 134 => 6,
        100 | 118 | 130 | 131 | 133 => 4,
        135 => 3,
        2 | 3 | 101 | 103 | 125 | 115 | 104 | 121 | 116 | 117 | 132 | 128 => 2,
        _ => 1,
    };
    let multiplier = match code {
        117 | 130 | 131 => 1000.0,
        116 | 2 | 3 => 100.0,
        103 | 115 | 104 => 10.0,
        _ => 1.0,
    };
    let signed = matches!(code, 121 | 103 | 134 | 2 | 3 | 136 | 113);
    (size, multiplier, signed)
}

/// Decodes the series of a min/max/average answer: per series the channel, the
/// type and three values (min, max, average). Returns the series and the bytes
/// from the first one that could not be decoded.
#[must_use]
pub fn parse_mma(buf: &[u8]) -> (Vec<MmaEntry>, Vec<u8>) {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < buf.len() && buf[i] != 0 {
        let Some(lpp_type) = buf.get(i + 1).and_then(|&code| LppType::from_code(code)) else {
            return (entries, buf[i..].to_vec());
        };
        let (size, multiplier, signed) = mma_layout(lpp_type.code());
        if buf.len() - i < 2 + 3 * size {
            return (entries, buf[i..].to_vec());
        }
        let value = |n: usize| {
            let start = i + 2 + n * size;
            LppValue::Scalar(be_value(&buf[start..start + size], signed) / multiplier)
        };
        entries.push(MmaEntry {
            channel: buf[i],
            lpp_type,
            min: value(0),
            max: value(1),
            avg: value(2),
        });
        i += 2 + 3 * size;
    }
    (entries, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_readings() {
        // ch1 voltage 4.12 V, ch1 temperature -3.5 °C, ch2 humidity 50 %
        let buf = [1, 116, 0x01, 0x9c, 1, 103, 0xff, 0xdd, 2, 104, 100];
        let telemetry = Telemetry::parse(&buf);
        assert_eq!(telemetry.undecoded, Vec::<u8>::new());
        assert_eq!(telemetry.voltage(), Some(4.12));
        assert_eq!(telemetry.temperature(), Some(-3.5));
        assert_eq!(telemetry.humidity(), Some(50.0));
    }

    #[test]
    fn stops_at_channel_zero_padding() {
        let mut buf = vec![1, 116, 0x01, 0x9c];
        buf.extend([0; 12]);
        let telemetry = Telemetry::parse(&buf);
        assert_eq!(telemetry.readings.len(), 1);
        assert_eq!(telemetry.undecoded, Vec::<u8>::new());
    }

    #[test]
    fn keeps_readings_before_an_unknown_type() {
        let buf = [1, 116, 0x01, 0x9c, 1, 200, 1, 2, 3];
        let telemetry = Telemetry::parse(&buf);
        assert_eq!(telemetry.readings.len(), 1);
        assert_eq!(telemetry.undecoded, vec![1, 200, 1, 2, 3]);
    }

    #[test]
    fn keeps_readings_before_a_cut_reading() {
        let buf = [1, 116, 0x01, 0x9c, 2, 103, 0x01];
        let telemetry = Telemetry::parse(&buf);
        assert_eq!(telemetry.readings.len(), 1);
        assert_eq!(telemetry.undecoded, vec![2, 103, 0x01]);
    }

    #[test]
    fn altitude_is_in_metres_and_gps_is_signed() {
        let buf = [1, 121, 0x00, 0x96];
        assert_eq!(
            Telemetry::parse(&buf).readings[0].value,
            LppValue::Scalar(150.0)
        );
        // lat 53.5511, lon -9.9937 (signed), alt 12.34 m
        let lat = 535_511i32.to_be_bytes();
        let lon = (-99_937i32).to_be_bytes();
        let alt = 1234i32.to_be_bytes();
        let mut gps = vec![1, 136];
        gps.extend(&lat[1..]);
        gps.extend(&lon[1..]);
        gps.extend(&alt[1..]);
        let (latitude, longitude, altitude) = Telemetry::parse(&gps).gps().unwrap();
        assert!((latitude - 53.5511).abs() < 1e-9);
        assert!((longitude + 9.9937).abs() < 1e-9);
        assert!((altitude - 12.34).abs() < 1e-9);
    }

    #[test]
    fn current_and_voltage_wrap_negative_like_meshcore_py() {
        // 0xfe0c = 65036 -> -0.5 A
        let buf = [1, 117, 0xfe, 0x0c];
        assert_eq!(
            Telemetry::parse(&buf).readings[0].value,
            LppValue::Scalar(-0.5)
        );
    }

    #[test]
    fn names_round_trip() {
        for lpp_type in LppType::ALL {
            assert_eq!(LppType::from_name(lpp_type.name()), Some(lpp_type));
            assert_eq!(LppType::from_code(lpp_type.code()), Some(lpp_type));
        }
        assert_eq!(LppType::Gps.size(), 9);
    }

    #[test]
    fn decodes_mma_series_with_the_firmware_layout() {
        // temperature min 20.0, max 25.5, avg 22.1; humidity (2 bytes x10) 40.0/60.0/50.5
        let mut buf = vec![1, 103];
        for v in [200i16, 255, 221] {
            buf.extend(v.to_be_bytes());
        }
        buf.extend([1, 104]);
        for v in [400u16, 600, 505] {
            buf.extend(v.to_be_bytes());
        }
        buf.extend([0; 6]);
        let (entries, undecoded) = parse_mma(&buf);
        assert_eq!(undecoded, Vec::<u8>::new());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].max, LppValue::Scalar(25.5));
        assert_eq!(entries[1].avg, LppValue::Scalar(50.5));
    }
}
