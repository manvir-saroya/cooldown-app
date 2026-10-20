//! The QR pairing payload (v1): `CD1:` followed by upper-case RFC 4648 base32 without padding.
//!
//! ```text
//! off  len  field
//! 0    1    qr_version = 0x01
//! 1    32   pc_static_pub (X25519)
//! 33   32   psk                (single use)
//! 65   16   pair_id
//! 81   2    tcp_port (u16 big endian)
//! 83   1    n_addr (1..=4)
//! 84   ..   addr: family u8 (0x04 or 0x06) + 4 or 16 bytes
//! ```
//!
//! The text is opaque (not a URL), so camera apps do not act on it, and the base32 alphabet fits
//! QR alphanumeric mode. The decoder is strict: one valid spelling per payload, and every
//! malformed input is an error, never a panic.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use zeroize::Zeroizing;

use crate::codec::{base32_decode, base32_encode_into};
use crate::error::QrError;
use crate::keys::{is_small_order, DeviceId, PublicKey};

/// Prefix of every payload.
pub const QR_PREFIX: &str = "CD1:";
/// The only `qr_version` this crate reads and writes.
pub const QR_VERSION: u8 = 1;
/// Fixed part of the binary payload, before the addresses.
const HEADER_LEN: usize = 84;
/// Longest binary payload: the header plus four IPv6 addresses.
pub const MAX_BINARY_LEN: usize = HEADER_LEN + 4 * 17;
/// Longest valid text: the prefix plus the base32 of [`MAX_BINARY_LEN`] bytes.
pub const MAX_TEXT_LEN: usize = QR_PREFIX.len() + (MAX_BINARY_LEN * 8).div_ceil(5);
/// Most addresses a payload may carry.
pub const MAX_ADDRESSES: usize = 4;

/// Everything the phone learns from the QR code.
///
/// The `psk` is a secret: `Debug` hides it and the type wipes it on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct QrPayload {
    /// The PC's static X25519 public key.
    pub pc_static_pub: PublicKey,
    /// The single-use pairing pre-shared key.
    pub psk: Zeroizing<[u8; 32]>,
    /// Names this pairing attempt in the cleartext hello.
    pub pair_id: [u8; 16],
    /// The PC's TCP port.
    pub tcp_port: u16,
    /// Addresses to try (1 to 4), in preference order.
    pub addrs: Vec<IpAddr>,
}

impl fmt::Debug for QrPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QrPayload")
            .field("pc", &DeviceId::from_public(&self.pc_static_pub))
            .field("psk", &"<redacted>")
            .field("tcp_port", &self.tcp_port)
            .field("addrs", &self.addrs.len())
            .finish()
    }
}

impl QrPayload {
    /// The PC's device id.
    pub fn pc_device_id(&self) -> DeviceId {
        DeviceId::from_public(&self.pc_static_pub)
    }

    /// The addresses with the port attached.
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.addrs
            .iter()
            .map(|ip| SocketAddr::new(*ip, self.tcp_port))
            .collect()
    }

    /// Encode to the text shown as a QR code. The text contains the PSK: treat it as a secret and
    /// never log it or put it on the clipboard.
    pub fn encode(&self) -> Result<Zeroizing<String>, QrError> {
        self.validate()?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_BINARY_LEN));
        bytes.push(QR_VERSION);
        bytes.extend_from_slice(&self.pc_static_pub);
        bytes.extend_from_slice(&*self.psk);
        bytes.extend_from_slice(&self.pair_id);
        bytes.extend_from_slice(&self.tcp_port.to_be_bytes());
        // validate() bounded the count to 1..=4.
        bytes.push(self.addrs.len() as u8);
        for addr in &self.addrs {
            match addr {
                IpAddr::V4(v4) => {
                    bytes.push(0x04);
                    bytes.extend_from_slice(&v4.octets());
                }
                IpAddr::V6(v6) => {
                    bytes.push(0x06);
                    bytes.extend_from_slice(&v6.octets());
                }
            }
        }
        let mut text = Zeroizing::new(String::with_capacity(MAX_TEXT_LEN));
        text.push_str(QR_PREFIX);
        base32_encode_into(&bytes, &mut text);
        Ok(text)
    }

    /// Decode and strictly validate QR text. Never panics, whatever the input.
    pub fn decode(text: &str) -> Result<Self, QrError> {
        if text.len() > MAX_TEXT_LEN {
            return Err(QrError::TooLong);
        }
        let body = text.strip_prefix(QR_PREFIX).ok_or(QrError::BadPrefix)?;
        let bytes = Zeroizing::new(base32_decode(body)?);
        Self::decode_bytes(&bytes)
    }

    /// Decode the binary form (everything after the `CD1:` prefix, base32-decoded).
    pub fn decode_bytes(bytes: &[u8]) -> Result<Self, QrError> {
        let mut reader = Reader { bytes, pos: 0 };
        if reader.u8()? != QR_VERSION {
            return Err(QrError::UnsupportedVersion);
        }
        let pc_static_pub: PublicKey = reader.array()?;
        let mut psk = Zeroizing::new([0u8; 32]);
        psk.copy_from_slice(reader.take(32)?);
        let pair_id: [u8; 16] = reader.array()?;
        let tcp_port = u16::from_be_bytes(reader.array()?);
        let count = usize::from(reader.u8()?);
        if !(1..=MAX_ADDRESSES).contains(&count) {
            return Err(QrError::AddressCount);
        }
        let mut addrs = Vec::with_capacity(count);
        for _ in 0..count {
            let ip = match reader.u8()? {
                0x04 => IpAddr::V4(Ipv4Addr::from(reader.array::<4>()?)),
                0x06 => IpAddr::V6(Ipv6Addr::from(reader.array::<16>()?)),
                _ => return Err(QrError::AddressFamily),
            };
            addrs.push(ip);
        }
        if reader.pos != bytes.len() {
            return Err(QrError::TrailingBytes);
        }
        let payload = QrPayload {
            pc_static_pub,
            psk,
            pair_id,
            tcp_port,
            addrs,
        };
        payload.validate()?;
        Ok(payload)
    }

    /// The structural rules shared by encoding and decoding.
    fn validate(&self) -> Result<(), QrError> {
        if self.tcp_port == 0 {
            return Err(QrError::BadPort);
        }
        if !(1..=MAX_ADDRESSES).contains(&self.addrs.len()) {
            return Err(QrError::AddressCount);
        }
        if is_small_order(&self.pc_static_pub) {
            return Err(QrError::BadKey);
        }
        for (index, addr) in self.addrs.iter().enumerate() {
            if !address_is_dialable(addr) || self.addrs[..index].contains(addr) {
                return Err(QrError::BadAddress);
            }
        }
        Ok(())
    }
}

/// Loopback is accepted on purpose (same-host development and the tests); everything that can
/// never be a unicast LAN peer is not.
fn address_is_dialable(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => !(v4.is_unspecified() || v4.is_multicast() || v4.is_broadcast()),
        IpAddr::V6(v6) => {
            let link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
            !(v6.is_unspecified()
                || v6.is_multicast()
                || link_local
                || v6.to_ipv4_mapped().is_some())
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], QrError> {
        let end = self.pos.checked_add(len).ok_or(QrError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(QrError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, QrError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], QrError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::StaticKeypair;

    fn sample() -> QrPayload {
        QrPayload {
            pc_static_pub: *StaticKeypair::generate().unwrap().public(),
            psk: Zeroizing::new([9u8; 32]),
            pair_id: [3u8; 16],
            tcp_port: 53320,
            addrs: vec![
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 7)),
            ],
        }
    }

    #[test]
    fn round_trip() {
        let payload = sample();
        let text = payload.encode().unwrap();
        assert!(text.starts_with("CD1:"));
        assert!(text.len() <= MAX_TEXT_LEN);
        assert_eq!(QrPayload::decode(&text).unwrap(), payload);
    }

    #[test]
    fn text_uses_only_the_qr_alphanumeric_alphabet() {
        let text = sample().encode().unwrap();
        assert!(text
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ':'));
    }

    #[test]
    fn maximum_payload_fits_the_documented_size() {
        let mut payload = sample();
        payload.addrs = (1..=4u16)
            .map(|i| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, i)))
            .collect();
        let text = payload.encode().unwrap();
        assert_eq!(text.len(), MAX_TEXT_LEN);
        assert_eq!(MAX_BINARY_LEN, 152);
    }

    #[test]
    fn rejects_malformed_text() {
        let good = sample().encode().unwrap();
        assert_eq!(QrPayload::decode(""), Err(QrError::BadPrefix));
        assert_eq!(QrPayload::decode("RB2:AAAA"), Err(QrError::BadPrefix));
        assert_eq!(
            QrPayload::decode(&good.to_lowercase()),
            Err(QrError::BadPrefix)
        );
        let lower_body = format!("CD1:{}", good[4..].to_lowercase());
        assert!(matches!(
            QrPayload::decode(&lower_body),
            Err(QrError::Base32(_))
        ));
        assert!(matches!(
            QrPayload::decode(&format!("{}=", *good)),
            Err(QrError::Base32(_))
        ));
        assert_eq!(
            QrPayload::decode(&format!("{}AAAAAAAA", *good)),
            Err(QrError::TrailingBytes)
        );
        assert_eq!(
            QrPayload::decode(&good[..good.len() - 8]),
            Err(QrError::Truncated)
        );
        assert_eq!(
            QrPayload::decode(&"A".repeat(MAX_TEXT_LEN + 1)),
            Err(QrError::TooLong)
        );
    }

    fn tweak(edit: impl FnOnce(&mut Vec<u8>)) -> Result<QrPayload, QrError> {
        let mut bytes = base32_decode(&sample().encode().unwrap()[4..]).unwrap();
        edit(&mut bytes);
        QrPayload::decode_bytes(&bytes)
    }

    #[test]
    fn rejects_bad_fields() {
        assert_eq!(tweak(|b| b[0] = 2), Err(QrError::UnsupportedVersion));
        assert_eq!(
            tweak(|b| {
                b[81] = 0;
                b[82] = 0;
            }),
            Err(QrError::BadPort)
        );
        assert_eq!(tweak(|b| b[83] = 0), Err(QrError::AddressCount));
        assert_eq!(tweak(|b| b[83] = 5), Err(QrError::AddressCount));
        assert_eq!(tweak(|b| b[84] = 0x05), Err(QrError::AddressFamily));
        // 0.0.0.0
        assert_eq!(
            tweak(|b| b[85..89].copy_from_slice(&[0, 0, 0, 0])),
            Err(QrError::BadAddress)
        );
        // fe80::1 (link-local IPv6 is excluded)
        assert_eq!(
            tweak(|b| {
                b[89] = 0x06;
                b[90..106].copy_from_slice(&Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1).octets());
            }),
            Err(QrError::BadAddress)
        );
        // A small-order PC key.
        assert_eq!(
            tweak(|b| b[1..33].copy_from_slice(&[0u8; 32])),
            Err(QrError::BadKey)
        );
        // Duplicate addresses.
        assert_eq!(
            tweak(|b| {
                b[89] = 0x04;
                b.truncate(90);
                b.extend_from_slice(&[192, 168, 1, 20]);
                b[83] = 2;
            }),
            Err(QrError::BadAddress)
        );
    }

    #[test]
    fn encoding_refuses_invalid_payloads() {
        let mut payload = sample();
        payload.tcp_port = 0;
        assert_eq!(payload.encode().unwrap_err(), QrError::BadPort);
        let mut payload = sample();
        payload.addrs.clear();
        assert_eq!(payload.encode().unwrap_err(), QrError::AddressCount);
    }

    #[test]
    fn debug_hides_the_psk() {
        let payload = sample();
        let shown = format!("{payload:?}");
        assert!(shown.contains("redacted"));
        assert!(!shown.contains("[9, 9"));
    }
}
