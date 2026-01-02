//! Protocol definitions for `MeshCore` communication.
//!
//! This module contains the low-level protocol types including:
//! - Frame encoding/decoding
//! - Packet type definitions
//! - Command opcodes
//! - Binary data parsing

pub mod command;
pub mod frame;
pub mod packet;
pub mod parser;

pub use command::{BinaryReqType, CommandOpcode, ControlDataType, MessageType, StatsType};
pub use frame::{FRAME_HEADER, FrameDecoder, MAX_FRAME_SIZE, encode as encode_frame};
pub use packet::PacketType;
pub use parser::{
    parse_battery, parse_channel, parse_channel_message, parse_contact, parse_contact_message,
    parse_core_stats, parse_device_info, parse_device_status, parse_packet_stats,
    parse_radio_stats, parse_self_info,
};
