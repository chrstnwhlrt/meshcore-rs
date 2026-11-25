//! A bounds-checked reader for frame payloads.

use crate::error::{Error, Result};

/// Reads little-endian fields from a byte slice and reports a short frame as
/// an error (`invalid_frame_length`, as meshcore_py names it) instead of
/// panicking.
#[derive(Debug, Clone)]
pub(crate) struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    #[cfg(test)]
    pub(crate) const fn position(&self) -> usize {
        self.pos
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.remaining() < len {
            log::debug!(
                target: "meshcore",
                "frame ends after {} bytes, {} more needed at offset {}",
                self.data.len(),
                len - self.remaining(),
                self.pos
            );
            return Err(Error::protocol("invalid_frame_length"));
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub(crate) fn i8(&mut self) -> Result<i8> {
        Ok(i8::from_le_bytes(self.array()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    /// The next byte, or `None` at the end (for optional trailing fields; a
    /// missing one is no short frame and not logged as one).
    pub(crate) fn opt_u8(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(byte)
    }

    /// The next four bytes as `u32`, or `None` when fewer are left (not
    /// logged either).
    pub(crate) fn opt_u32(&mut self) -> Option<u32> {
        if self.remaining() >= 4 {
            self.u32().ok()
        } else {
            None
        }
    }

    pub(crate) fn skip(&mut self, len: usize) -> Result<()> {
        self.bytes(len).map(|_| ())
    }

    /// Everything not read yet.
    pub(crate) fn rest(&mut self) -> &'a [u8] {
        let slice = &self.data[self.pos..];
        self.pos = self.data.len();
        slice
    }
}

/// The bytes of a NUL-padded field up to the first NUL.
pub(crate) fn until_nul(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

/// Decodes a NUL-padded text field: the text ends at the first NUL, invalid
/// UTF-8 is dropped (as meshcore_py's `decode("utf-8", "ignore")`).
pub(crate) fn text_until_nul(field: &[u8]) -> String {
    lossy_ignore(until_nul(field))
}

/// Decodes text, dropping invalid UTF-8 sequences instead of replacing them.
pub(crate) fn lossy_ignore(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_little_endian_fields() {
        let data = [0x01, 0x34, 0x12, 0xff, 0xff, 0x78, 0x56, 0x34, 0x12];
        let mut reader = ByteReader::new(&data);
        assert_eq!(reader.u8().unwrap(), 1);
        assert_eq!(reader.u16().unwrap(), 0x1234);
        assert_eq!(reader.i16().unwrap(), -1);
        assert_eq!(reader.u32().unwrap(), 0x1234_5678);
        assert_eq!(reader.remaining(), 0);
        assert!(reader.u8().is_err());
        assert_eq!(reader.opt_u8(), None);
    }

    #[test]
    fn short_reads_are_errors_not_panics() {
        let mut reader = ByteReader::new(&[1, 2]);
        assert!(matches!(
            reader.u32(),
            Err(Error::Protocol(reason)) if reason == "invalid_frame_length"
        ));
        assert_eq!(reader.position(), 0);
        assert_eq!(reader.opt_u32(), None);
        assert_eq!(reader.rest(), &[1, 2]);
    }

    #[test]
    fn text_fields() {
        assert_eq!(text_until_nul(b"Node\0junk"), "Node");
        assert_eq!(text_until_nul(b"Caf\xc3"), "Caf");
        assert_eq!(lossy_ignore(b"a\xffb"), "ab");
    }
}
