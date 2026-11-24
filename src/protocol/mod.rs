//! The companion protocol: codes, framing and path encoding.

pub(crate) mod bytes;
pub mod codes;
pub mod frame;
pub(crate) mod parse;
pub mod path;

pub use codes::{AnonReqType, BinaryReqType, CommandType, ControlType, PacketType, StatsType};
pub use frame::{
    DEVICE_MAX_FRAME_SIZE, FRAME_START_IN, FRAME_START_OUT, FrameDecoder, MAX_ACCEPTED_FRAME_SIZE,
    encode as encode_frame,
};
pub use path::{MAX_PATH_SIZE, PathLen, encode_reply_path, parse_path_hex, split_hops};
