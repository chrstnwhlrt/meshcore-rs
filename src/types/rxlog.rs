//! Packets the radio received, as the RX log reports them (`PUSH_CODE_LOG_RX_DATA`).

use crate::types::contact::PublicKey;

/// The route type of a mesh packet (low 2 bits of the header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteType {
    /// Flood with transport codes.
    TransportFlood,
    /// Flood.
    Flood,
    /// Direct.
    Direct,
    /// Direct with transport codes.
    TransportDirect,
}

impl RouteType {
    /// Maps the 2-bit route type.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0x03 {
            0 => Self::TransportFlood,
            1 => Self::Flood,
            2 => Self::Direct,
            _ => Self::TransportDirect,
        }
    }

    /// The 2-bit route type.
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::TransportFlood => 0,
            Self::Flood => 1,
            Self::Direct => 2,
            Self::TransportDirect => 3,
        }
    }

    /// meshcore_py's name (`TC_FLOOD`, `FLOOD`, `DIRECT`, `TC_DIRECT`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TransportFlood => "TC_FLOOD",
            Self::Flood => "FLOOD",
            Self::Direct => "DIRECT",
            Self::TransportDirect => "TC_DIRECT",
        }
    }

    /// True when the packet carries transport codes.
    #[must_use]
    pub const fn has_transport_codes(self) -> bool {
        matches!(self, Self::TransportFlood | Self::TransportDirect)
    }
}

/// The payload type of a mesh packet (bits 2-5 of the header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadType {
    /// A request.
    Req,
    /// A response.
    Response,
    /// A text message.
    TextMsg,
    /// An ACK.
    Ack,
    /// An advert.
    Advert,
    /// A group text message.
    GrpTxt,
    /// A group datagram.
    GrpData,
    /// An anonymous request.
    AnonReq,
    /// A returned path.
    Path,
    /// A trace.
    Trace,
    /// One packet of a multipart set.
    Multipart,
    /// Control data.
    Control,
    /// A type this library does not name (e.g. 15, raw custom packets).
    Other(u8),
}

impl PayloadType {
    /// Maps the 4-bit payload type.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        match bits & 0x0F {
            0 => Self::Req,
            1 => Self::Response,
            2 => Self::TextMsg,
            3 => Self::Ack,
            4 => Self::Advert,
            5 => Self::GrpTxt,
            6 => Self::GrpData,
            7 => Self::AnonReq,
            8 => Self::Path,
            9 => Self::Trace,
            10 => Self::Multipart,
            11 => Self::Control,
            other => Self::Other(other),
        }
    }

    /// The 4-bit payload type.
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Req => 0,
            Self::Response => 1,
            Self::TextMsg => 2,
            Self::Ack => 3,
            Self::Advert => 4,
            Self::GrpTxt => 5,
            Self::GrpData => 6,
            Self::AnonReq => 7,
            Self::Path => 8,
            Self::Trace => 9,
            Self::Multipart => 10,
            Self::Control => 11,
            Self::Other(bits) => bits,
        }
    }

    /// meshcore_py's name (`REQ`, `RESPONSE`, `TEXT_MSG`, ... or `UNK`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Req => "REQ",
            Self::Response => "RESPONSE",
            Self::TextMsg => "TEXT_MSG",
            Self::Ack => "ACK",
            Self::Advert => "ADVERT",
            Self::GrpTxt => "GRP_TXT",
            Self::GrpData => "GRP_DATA",
            Self::AnonReq => "ANON_REQ",
            Self::Path => "PATH",
            Self::Trace => "TRACE",
            Self::Multipart => "MULTIPART",
            Self::Control => "CONTROL",
            Self::Other(_) => "UNK",
        }
    }
}

/// One received packet as the RX log reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct RxLogData {
    /// When the frame was received (Unix seconds, host clock).
    pub recv_time: u64,
    /// SNR in dB.
    pub snr: Option<f32>,
    /// RSSI in dBm.
    pub rssi: Option<i8>,
    /// The frame after its code byte: SNR, RSSI and the packet.
    pub raw: Vec<u8>,
    /// The raw packet.
    pub payload: Vec<u8>,
    /// The parsed packet, or `None` when it is too short to read.
    pub packet: Option<PacketInfo>,
}

/// A parsed mesh packet.
#[derive(Debug, Clone, PartialEq)]
pub struct PacketInfo {
    /// The header byte.
    pub header: u8,
    /// The route type.
    pub route_type: RouteType,
    /// The payload type.
    pub payload_type: PayloadType,
    /// The payload version (top 2 bits of the header).
    pub payload_ver: u8,
    /// The transport codes, for transport routes.
    pub transport_code: Option<[u8; 4]>,
    /// The number of hops in the path.
    pub path_len: u8,
    /// Bytes per hop.
    pub path_hash_size: u8,
    /// The path.
    pub path: Vec<u8>,
    /// The payload after the path.
    pub pkt_payload: Vec<u8>,
    /// The first 4 bytes (LE) of the payload's SHA-256, to spot repeats.
    pub pkt_hash: u32,
    /// What the payload holds, as far as it is decoded.
    pub content: PacketContent,
}

/// The decoded payload of a mesh packet.
#[derive(Debug, Clone, PartialEq)]
pub enum PacketContent {
    /// Not decoded.
    None,
    /// A group text message.
    GroupText(GroupText),
    /// An advert.
    Advert(AdvertInfo),
}

/// A group text packet.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupText {
    /// The channel hash.
    pub chan_hash: u8,
    /// The 2-byte MAC.
    pub cipher_mac: [u8; 2],
    /// The encrypted message.
    pub crypted: Vec<u8>,
    /// The channel's name, when a known channel's MAC matches.
    pub chan_name: Option<String>,
    /// The decrypted message, when channel log decryption is on, the channel
    /// is known and the encrypted part is one or more whole 16-byte blocks.
    pub decrypted: Option<DecryptedGroupText>,
}

/// A decrypted group text message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecryptedGroupText {
    /// The text, usually `"sender: message"`.
    pub message: String,
    /// Hash of timestamp and text, matching [`ChannelMessage`](crate::ChannelMessage)s.
    pub msg_hash: u32,
    /// When the sender sent it, by its clock.
    pub sender_timestamp: u32,
    /// The sender's attempt counter.
    pub attempt: u8,
    /// The text type.
    pub txt_type: u8,
}

/// An advert packet.
#[derive(Debug, Clone, PartialEq)]
pub struct AdvertInfo {
    /// The node's key.
    pub adv_key: PublicKey,
    /// The advert's timestamp, by the node's clock.
    pub adv_timestamp: u32,
    /// The advert's signature.
    pub signature: Vec<u8>,
    /// The flags byte.
    pub adv_flags: u8,
    /// The node type (low nibble of the flags).
    pub adv_type: u8,
    /// Latitude in degrees, when advertised.
    pub adv_lat: Option<f64>,
    /// Longitude in degrees, when advertised.
    pub adv_lon: Option<f64>,
    /// Feature field 1, when present.
    pub adv_feat1: Option<[u8; 2]>,
    /// Feature field 2, when present.
    pub adv_feat2: Option<[u8; 2]>,
    /// The node's name, when present.
    pub adv_name: Option<String>,
}
