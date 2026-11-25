//! Framing of the serial and TCP byte stream.
//!
//! ```text
//! host -> device:  0x3c ('<')  len_lo  len_hi  payload...
//! device -> host:  0x3e ('>')  len_lo  len_hi  payload...
//! ```

use crate::error::{Error, Result};

/// Start byte of frames the host sends (`'<'`).
pub const FRAME_START_OUT: u8 = 0x3c;

/// Start byte of frames the device sends (`'>'`).
pub const FRAME_START_IN: u8 = 0x3e;

/// The largest frame the firmware sends or accepts (from firmware 1.16 on;
/// 172 before).
pub const DEVICE_MAX_FRAME_SIZE: usize = 176;

/// Frames announcing more than this are taken for noise and skipped, as in
/// meshcore_py.
pub const MAX_ACCEPTED_FRAME_SIZE: usize = 300;

/// Wraps a payload in a host-to-device frame.
///
/// # Errors
///
/// [`Error::InvalidArgument`] when the payload does not fit a frame.
pub fn encode(payload: &[u8]) -> Result<Vec<u8>> {
    let len = u16::try_from(payload.len())
        .map_err(|_| Error::invalid(format!("frame of {} bytes is too long", payload.len())))?;
    if payload.len() > DEVICE_MAX_FRAME_SIZE {
        log::warn!(
            target: "meshcore",
            "sending a {} byte frame, the firmware keeps only {DEVICE_MAX_FRAME_SIZE}",
            payload.len()
        );
    }
    let mut frame = Vec::with_capacity(3 + payload.len());
    frame.push(FRAME_START_OUT);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Splits the device's byte stream into frames.
///
/// Bytes before a start byte are skipped: some radios print console output on
/// the same UART. A start byte whose header announces more than
/// [`MAX_ACCEPTED_FRAME_SIZE`] bytes is skipped alone, as a frame may start
/// inside that header, and the search goes on right after it. A header
/// announcing no bytes gives an empty frame, which the client logs as
/// meshcore_py does.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// Creates an empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Feeds received bytes and returns the frames they complete.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            match self.buffer.iter().position(|&b| b == FRAME_START_IN) {
                Some(0) => {}
                Some(start) => {
                    self.buffer.drain(..start);
                }
                None => {
                    self.buffer.clear();
                    break;
                }
            }
            if self.buffer.len() < 3 {
                break;
            }
            let len = usize::from(u16::from_le_bytes([self.buffer[1], self.buffer[2]]));
            if len > MAX_ACCEPTED_FRAME_SIZE {
                log::debug!(target: "meshcore", "skipping a start byte announcing {len} bytes");
                self.buffer.drain(..1);
                continue;
            }
            if self.buffer.len() < 3 + len {
                break;
            }
            let frame = self.buffer[3..3 + len].to_vec();
            self.buffer.drain(..3 + len);
            frames.push(frame);
        }
        frames
    }

    /// Bytes held back waiting for the rest of a frame.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Drops any partial frame.
    pub fn clear(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![FRAME_START_IN];
        frame.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_le_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn encodes_with_host_start_byte() {
        assert_eq!(encode(b"hello").unwrap(), b"<\x05\x00hello");
        assert!(encode(&vec![0; 70_000]).is_err());
    }

    #[test]
    fn decodes_complete_and_split_frames() {
        let mut decoder = FrameDecoder::new();
        let frame = device_frame(b"hello");
        assert_eq!(decoder.feed(&frame[..4]), Vec::<Vec<u8>>::new());
        assert_eq!(decoder.feed(&frame[4..]), vec![b"hello".to_vec()]);
        assert_eq!(decoder.buffered(), 0);
    }

    #[test]
    fn decodes_several_frames_in_one_read() {
        let mut decoder = FrameDecoder::new();
        let mut data = device_frame(b"hi");
        data.extend(device_frame(b"bye"));
        assert_eq!(decoder.feed(&data), vec![b"hi".to_vec(), b"bye".to_vec()]);
    }

    #[test]
    fn skips_junk_before_the_start_byte() {
        let mut decoder = FrameDecoder::new();
        let mut data = b"DEBUG: boot ok\r\n".to_vec();
        data.extend(device_frame(&[0x05, 0x01]));
        assert_eq!(decoder.feed(&data), vec![vec![0x05, 0x01]]);
    }

    #[test]
    fn frame_payload_may_contain_the_start_byte() {
        let mut decoder = FrameDecoder::new();
        let payload = [0x3e, 0x3e, 0x01];
        assert_eq!(
            decoder.feed(&device_frame(&payload)),
            vec![payload.to_vec()]
        );
    }

    #[test]
    fn resyncs_after_an_oversized_header() {
        let mut decoder = FrameDecoder::new();
        // '>' followed by a length of 0xffff, then a real frame
        let mut data = vec![FRAME_START_IN, 0xff, 0xff];
        data.extend(device_frame(b"ok"));
        assert_eq!(decoder.feed(&data), vec![b"ok".to_vec()]);
    }

    #[test]
    fn a_frame_right_behind_a_stray_start_byte_is_kept() {
        let mut decoder = FrameDecoder::new();
        // console text ending in '>', then a frame: the bogus header takes the
        // frame's start byte and length
        let mut data = b"tx -->".to_vec();
        data.extend(device_frame(&[0xaa, 0xbb]));
        assert_eq!(decoder.feed(&data), vec![vec![0xaa, 0xbb]]);
    }

    #[test]
    fn empty_frames_are_passed_on() {
        let mut decoder = FrameDecoder::new();
        let mut data = vec![FRAME_START_IN, 0, 0];
        data.extend(device_frame(b"ok"));
        assert_eq!(decoder.feed(&data), vec![Vec::new(), b"ok".to_vec()]);
    }

    #[test]
    fn host_frames_are_not_taken_for_device_frames() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.feed(&encode(b"echo").unwrap()),
            Vec::<Vec<u8>>::new()
        );
    }
}
