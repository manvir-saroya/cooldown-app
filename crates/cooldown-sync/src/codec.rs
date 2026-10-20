//! Small, strict encoders: lowercase hex and RFC 4648 base32 (upper case, no padding).
//!
//! Everything here rejects non-canonical input (wrong case, padding, stray characters, non-zero
//! trailing bits) so an encoded value has exactly one valid spelling.

/// Lowercase hex of `bytes`.
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Decode exactly `N` bytes of lowercase hex. Returns `None` for any other input.
pub fn hex_decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = hex_digit(*bytes.get(2 * index)?)?;
        let low = hex_digit(*bytes.get(2 * index + 1)?)?;
        *slot = (high << 4) | low;
    }
    Some(out)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

const BASE32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Why a base32 string was rejected. Never echoes the input (it may be a secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Base32Error {
    /// A character outside `A-Z2-7` (this includes lower case and `=` padding).
    #[error("character outside the base32 alphabet")]
    BadCharacter,
    /// A length no byte string encodes to.
    #[error("impossible base32 length")]
    BadLength,
    /// The unused trailing bits are not zero, so the text is not the canonical encoding.
    #[error("non-canonical trailing bits")]
    NonCanonical,
}

/// Encode as upper-case base32 without padding.
pub fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
    base32_encode_into(bytes, &mut out);
    out
}

/// Append the base32 encoding of `bytes` to `out`, so a caller holding secrets can encode straight
/// into a buffer it wipes (no unwiped intermediate string).
pub fn base32_encode_into(bytes: &[u8], out: &mut String) {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(BASE32_ALPHABET[((acc >> bits) & 0x1f) as usize]));
        }
        acc &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(
            BASE32_ALPHABET[((acc << (5 - bits)) & 0x1f) as usize],
        ));
    }
}

/// Decode canonical upper-case base32 without padding.
pub fn base32_decode(text: &str) -> Result<Vec<u8>, Base32Error> {
    let symbols = text.as_bytes();
    // 8 symbols carry 5 bytes; the valid partial group sizes are 0, 2, 4, 5 and 7 symbols.
    if !matches!(symbols.len() % 8, 0 | 2 | 4 | 5 | 7) {
        return Err(Base32Error::BadLength);
    }
    let mut out = Vec::with_capacity(symbols.len() * 5 / 8);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &symbol in symbols {
        let value = match symbol {
            b'A'..=b'Z' => symbol - b'A',
            b'2'..=b'7' => symbol - b'2' + 26,
            _ => return Err(Base32Error::BadCharacter),
        };
        acc = (acc << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return Err(Base32Error::NonCanonical);
    }
    Ok(out)
}

/// URL-safe base64 without padding, for 32-byte keys in JSON and files.
pub fn b64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Strict inverse of [`b64_encode`] for exactly `N` bytes: no padding, no whitespace, no
/// non-zero trailing bits.
pub fn b64_decode_fixed<const N: usize>(text: &str) -> Option<[u8; N]> {
    use base64::Engine as _;
    // 4 symbols per 3 bytes, so N bytes need ceil(N * 4 / 3) symbols and nothing else.
    if text.len() != (N * 4).div_ceil(3) {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()?;
    <[u8; N]>::try_from(bytes.as_slice()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_base32_vectors() {
        // RFC 4648 section 10, with the padding removed.
        let cases: [(&[u8], &str); 7] = [
            (b"", ""),
            (b"f", "MY"),
            (b"fo", "MZXQ"),
            (b"foo", "MZXW6"),
            (b"foob", "MZXW6YQ"),
            (b"fooba", "MZXW6YTB"),
            (b"foobar", "MZXW6YTBOI"),
        ];
        for (raw, encoded) in cases {
            assert_eq!(base32_encode(raw), encoded);
            assert_eq!(base32_decode(encoded).unwrap(), raw);
        }
    }

    #[test]
    fn base32_rejects_noncanonical_input() {
        assert_eq!(base32_decode("my"), Err(Base32Error::BadCharacter));
        assert_eq!(base32_decode("MY======"), Err(Base32Error::BadCharacter));
        assert_eq!(base32_decode("M"), Err(Base32Error::BadLength));
        assert_eq!(base32_decode("MZ"), Err(Base32Error::NonCanonical));
        assert_eq!(base32_decode("M Y"), Err(Base32Error::BadLength));
        assert_eq!(base32_decode("MY1"), Err(Base32Error::BadLength));
        assert_eq!(base32_decode("MY18"), Err(Base32Error::BadCharacter));
    }

    #[test]
    fn b64_round_trip_and_strictness() {
        let key = [0xfbu8; 32];
        let text = b64_encode(&key);
        assert_eq!(text.len(), 43);
        assert_eq!(b64_decode_fixed::<32>(&text), Some(key));
        assert_eq!(b64_decode_fixed::<32>(&format!("{text}=")), None);
        assert_eq!(b64_decode_fixed::<32>(&text[..42]), None);
        assert_eq!(b64_decode_fixed::<32>(&text.replace('_', "/")), None);
        // Non-zero trailing bits: change the last symbol so the unused bits are set.
        let mut tampered = text.clone();
        tampered.pop();
        tampered.push('_');
        assert_eq!(b64_decode_fixed::<32>(&tampered), None);
    }

    #[test]
    fn hex_round_trip_and_strictness() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(hex_decode_fixed::<3>("00abff"), Some([0x00, 0xab, 0xff]));
        assert_eq!(hex_decode_fixed::<3>("00ABFF"), None);
        assert_eq!(hex_decode_fixed::<3>("00abf"), None);
        assert_eq!(hex_decode_fixed::<3>("00abgg"), None);
    }
}
