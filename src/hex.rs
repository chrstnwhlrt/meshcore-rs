//! Lower-case hex encoding and decoding, as meshcore_py prints keys, paths and tags.

const DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Encodes bytes as lower-case hex.
#[must_use]
pub fn encode(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Decodes hex (upper or lower case). Surrounding whitespace is ignored.
///
/// Returns `None` for an odd number of digits or a character that is not a
/// hex digit.
#[must_use]
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let text = text.trim().as_bytes();
    let (pairs, rest) = text.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|&[high, low]| Some((nibble(high)? << 4) | nibble(low)?))
        .collect()
}

const fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let bytes = [0x00, 0x01, 0xab, 0xff, 0x7f];
        assert_eq!(encode(bytes), "0001abff7f");
        assert_eq!(decode("0001ABff7f"), Some(bytes.to_vec()));
        assert_eq!(decode("  0a  "), Some(vec![0x0a]));
        assert_eq!(decode(""), Some(Vec::new()));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(decode("abc"), None);
        assert_eq!(decode("zz"), None);
        assert_eq!(decode("0g"), None);
    }
}
