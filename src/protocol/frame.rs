//! Frame encoding and decoding for the `MeshCore` protocol.
//!
//! The wire format uses a simple framing protocol:
//! ```text
//! ┌──────────┬──────────────┬─────────────────┐
//! │  0x3c    │  size (LE)   │    payload      │
//! │  1 byte  │   2 bytes    │   size bytes    │
//! └──────────┴──────────────┴─────────────────┘
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};

use crate::error::FrameError;

/// Frame header byte.
pub const FRAME_HEADER: u8 = 0x3c;

/// Maximum frame payload size (64KB - 1).
pub const MAX_FRAME_SIZE: usize = 65535;

/// Minimum frame size (header + 2-byte length).
pub const MIN_FRAME_SIZE: usize = 3;

/// Encodes a payload into a framed message.
///
/// # Arguments
///
/// * `payload` - The data to frame
///
/// # Returns
///
/// A `Bytes` containing the framed message.
///
/// # Panics
///
/// Panics if the payload exceeds `MAX_FRAME_SIZE`.
#[must_use]
pub fn encode(payload: &[u8]) -> Bytes {
    assert!(
        payload.len() <= MAX_FRAME_SIZE,
        "payload exceeds maximum frame size"
    );

    let mut buf = BytesMut::with_capacity(MIN_FRAME_SIZE + payload.len());
    buf.put_u8(FRAME_HEADER);
    // SAFETY: assert above guarantees payload.len() <= MAX_FRAME_SIZE (65535)
    buf.put_u16_le(u16::try_from(payload.len()).expect("length checked above"));
    buf.put_slice(payload);
    buf.freeze()
}

/// Frame decoder that handles partial data.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: BytesMut,
}

impl FrameDecoder {
    /// Creates a new frame decoder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffer: BytesMut::new(),
        }
    }

    /// Feeds data into the decoder.
    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Attempts to decode the next complete frame.
    ///
    /// Returns `Ok(Some(payload))` if a complete frame was decoded,
    /// `Ok(None)` if more data is needed, or an error if the frame is invalid.
    ///
    /// The frame format is: `<header> <length_lo> <length_hi> <payload...>`
    /// where header is typically `0x3c` but we accept any header byte
    /// (matching Python library behavior).
    ///
    /// # Errors
    ///
    /// Returns a `FrameError` if:
    /// - The frame size exceeds the maximum
    pub fn decode(&mut self) -> Result<Option<Bytes>, FrameError> {
        if self.buffer.len() < MIN_FRAME_SIZE {
            return Ok(None);
        }

        // Read length (little-endian u16) from bytes 1-2
        // Note: We don't validate the header byte (byte 0) - matching Python behavior
        let length = u16::from_le_bytes([self.buffer[1], self.buffer[2]]) as usize;

        if length > MAX_FRAME_SIZE {
            return Err(FrameError::TooLarge {
                size: length,
                max: MAX_FRAME_SIZE,
            });
        }

        let total_frame_size = MIN_FRAME_SIZE + length;

        // Check if we have the complete frame
        if self.buffer.len() < total_frame_size {
            return Ok(None);
        }

        // Extract the frame
        self.buffer.advance(MIN_FRAME_SIZE); // Skip header and length
        let payload = self.buffer.split_to(length).freeze();

        Ok(Some(payload))
    }

    /// Returns the number of bytes currently buffered.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Clears the internal buffer.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_simple() {
        let payload = b"hello";
        let frame = encode(payload);

        assert_eq!(frame[0], FRAME_HEADER);
        assert_eq!(frame[1], 5); // length low byte
        assert_eq!(frame[2], 0); // length high byte
        assert_eq!(&frame[3..], b"hello");
    }

    #[test]
    fn test_decode_complete_frame() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0x3c, 0x05, 0x00, b'h', b'e', b'l', b'l', b'o']);

        let result = decoder.decode().unwrap();
        assert_eq!(result, Some(Bytes::from_static(b"hello")));
    }

    #[test]
    fn test_decode_partial_frame() {
        let mut decoder = FrameDecoder::new();

        // Feed partial data
        decoder.feed(&[0x3c, 0x05, 0x00, b'h', b'e']);
        assert_eq!(decoder.decode().unwrap(), None);

        // Feed remaining data
        decoder.feed(b"llo");
        let result = decoder.decode().unwrap();
        assert_eq!(result, Some(Bytes::from_static(b"hello")));
    }

    #[test]
    fn test_decode_any_header() {
        // Any header byte should be accepted (matching Python behavior)
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[0x3e, 0x02, 0x00, b'o', b'k']); // 0x3e instead of 0x3c

        let result = decoder.decode().unwrap();
        assert_eq!(result, Some(Bytes::from_static(b"ok")));
    }

    #[test]
    fn test_decode_multiple_frames() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&[
            0x3c, 0x02, 0x00, b'h', b'i', // first frame
            0x3c, 0x03, 0x00, b'b', b'y', b'e', // second frame
        ]);

        let first = decoder.decode().unwrap();
        assert_eq!(first, Some(Bytes::from_static(b"hi")));

        let second = decoder.decode().unwrap();
        assert_eq!(second, Some(Bytes::from_static(b"bye")));
    }
}
