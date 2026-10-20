//! The discovery beacon wire format (`docs/pairing-protocol.md` section 4.1), as pure functions.
//!
//! ```text
//! off len field
//! 0   4   magic "CDBC"
//! 4   1   version = 1
//! 5   1   kind: 0x01 ANNOUNCE (PC) | 0x02 PROBE (phone)
//! 6   2   tcp_port, big endian (0 in a PROBE, never 0 in an ANNOUNCE)
//! 8   8   nonce (random)
//! 16  16  tag = BLAKE2s-128(key = beacon_key, "cd-beacon-v1" || bytes[0..16])
//! ```
//!
//! A datagram is exactly 32 bytes. The tag is the native keyed BLAKE2s with a 16-byte digest (not
//! the first 16 bytes of the 32-byte digest), computed over a domain string and the first 16
//! bytes, so changing the kind, the port or the nonce invalidates it. The tag is compared in
//! constant time.
//!
//! [`Beacon::decode`] never panics, rejects everything that is not exactly what [`Beacon::encode`]
//! would produce, and reports why through [`BeaconError`]. The decoder is canonical: a datagram
//! that decodes re-encodes to the same bytes. A beacon reveals only that some Cooldown device is
//! at an IP address and its TCP port; it carries no name, identifier or key, and the random nonce
//! keeps two tags from being linkable. It is never an authority: an address learned from a beacon
//! is only a candidate for a Noise connection.

use std::net::Ipv4Addr;

use blake2::digest::consts::U16;
use blake2::digest::{KeyInit, Mac};
use blake2::Blake2sMac;
use subtle::ConstantTimeEq;

use crate::error::BeaconError;

/// First four bytes of every beacon.
pub const BEACON_MAGIC: [u8; 4] = *b"CDBC";
/// The only beacon version this crate reads and writes.
pub const BEACON_VERSION: u8 = 1;
/// Length of every beacon datagram.
pub const BEACON_LEN: usize = 32;
/// Length of the tag at the end of a beacon.
pub const BEACON_TAG_LEN: usize = 16;
/// Length of the random nonce.
pub const BEACON_NONCE_LEN: usize = 8;
/// Bytes that the tag covers.
const SIGNED_LEN: usize = BEACON_LEN - BEACON_TAG_LEN;
/// Domain separation prepended to the signed bytes.
const TAG_DOMAIN: &[u8] = b"cd-beacon-v1";

/// The multicast group (chosen because some Android devices only receive 224.0.0.0/24).
pub const BEACON_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
/// The beacon's own UDP port (a placeholder in the protocol document; LocalSend uses 53317).
pub const BEACON_PORT: u16 = 53319;

/// Who sent a beacon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeaconKind {
    /// From the PC: "a Cooldown PC is here, on this TCP port".
    Announce = 0x01,
    /// From the phone: "any PC with my key, please announce yourself".
    Probe = 0x02,
}

impl BeaconKind {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0x01 => Some(BeaconKind::Announce),
            0x02 => Some(BeaconKind::Probe),
            _ => None,
        }
    }
}

/// A validated beacon: the fields that travel, without the tag. Only [`Beacon::announce`],
/// [`Beacon::probe`] and a successful [`Beacon::decode`] make one, so every `Beacon` encodes to a
/// datagram that decodes again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Beacon {
    kind: BeaconKind,
    tcp_port: u16,
    nonce: [u8; BEACON_NONCE_LEN],
}

impl Beacon {
    /// An ANNOUNCE for `tcp_port`. Port 0 is rejected: it names nothing a phone could dial.
    pub fn announce(tcp_port: u16, nonce: [u8; BEACON_NONCE_LEN]) -> Result<Beacon, BeaconError> {
        if tcp_port == 0 {
            return Err(BeaconError::Port);
        }
        Ok(Beacon {
            kind: BeaconKind::Announce,
            tcp_port,
            nonce,
        })
    }

    /// A PROBE (its port field is always 0).
    pub fn probe(nonce: [u8; BEACON_NONCE_LEN]) -> Beacon {
        Beacon {
            kind: BeaconKind::Probe,
            tcp_port: 0,
            nonce,
        }
    }

    /// Who sent it.
    pub fn kind(&self) -> BeaconKind {
        self.kind
    }

    /// The announced TCP port (0 for a PROBE).
    pub fn tcp_port(&self) -> u16 {
        self.tcp_port
    }

    /// The random nonce.
    pub fn nonce(&self) -> [u8; BEACON_NONCE_LEN] {
        self.nonce
    }

    /// The 32-byte datagram, tagged with `key`.
    pub fn encode(&self, key: &[u8; 32]) -> [u8; BEACON_LEN] {
        let mut out = [0u8; BEACON_LEN];
        out[0..4].copy_from_slice(&BEACON_MAGIC);
        out[4] = BEACON_VERSION;
        out[5] = self.kind as u8;
        out[6..8].copy_from_slice(&self.tcp_port.to_be_bytes());
        out[8..16].copy_from_slice(&self.nonce);
        let tag = compute_tag(key, &out[..SIGNED_LEN]);
        out[SIGNED_LEN..].copy_from_slice(&tag);
        out
    }

    /// Parse and authenticate a received datagram. The structural checks (length, magic, version,
    /// kind, port) come first and depend on no secret; the tag is verified last, in constant time.
    pub fn decode(datagram: &[u8], key: &[u8; 32]) -> Result<Beacon, BeaconError> {
        let bytes: &[u8; BEACON_LEN] = datagram.try_into().map_err(|_| BeaconError::Length)?;
        if bytes[0..4] != BEACON_MAGIC {
            return Err(BeaconError::Magic);
        }
        if bytes[4] != BEACON_VERSION {
            return Err(BeaconError::Version);
        }
        let kind = BeaconKind::from_byte(bytes[5]).ok_or(BeaconError::Kind)?;
        let tcp_port = u16::from_be_bytes([bytes[6], bytes[7]]);
        match (kind, tcp_port) {
            (BeaconKind::Announce, 0) | (BeaconKind::Probe, 1..=u16::MAX) => {
                return Err(BeaconError::Port)
            }
            _ => {}
        }
        let expected = compute_tag(key, &bytes[..SIGNED_LEN]);
        if !bool::from(expected.ct_eq(&bytes[SIGNED_LEN..])) {
            return Err(BeaconError::Tag);
        }
        let mut nonce = [0u8; BEACON_NONCE_LEN];
        nonce.copy_from_slice(&bytes[8..16]);
        Ok(Beacon {
            kind,
            tcp_port,
            nonce,
        })
    }
}

/// `BLAKE2s-128(key, "cd-beacon-v1" || signed)`: keyed BLAKE2s with a 16-byte digest.
fn compute_tag(key: &[u8; 32], signed: &[u8]) -> [u8; BEACON_TAG_LEN] {
    let mut mac = <Blake2sMac<U16> as KeyInit>::new(key.into());
    mac.update(TAG_DOMAIN);
    mac.update(signed);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = {
        let mut key = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            key[i] = i as u8;
            i += 1;
        }
        key
    };
    const OTHER_KEY: [u8; 32] = [0x5A; 32];
    const NONCE: [u8; 8] = [0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn announce() -> [u8; BEACON_LEN] {
        Beacon::announce(53320, NONCE).unwrap().encode(&KEY)
    }

    fn probe() -> [u8; BEACON_LEN] {
        Beacon::probe(NONCE).encode(&KEY)
    }

    /// Re-tag `bytes` with `KEY` after editing the signed part, so a test can isolate a
    /// structural rejection from a tag mismatch.
    fn retag(mut bytes: [u8; BEACON_LEN]) -> [u8; BEACON_LEN] {
        let tag = compute_tag(&KEY, &bytes[..SIGNED_LEN]);
        bytes[SIGNED_LEN..].copy_from_slice(&tag);
        bytes
    }

    /// Computed once with Python's `hashlib.blake2s(data, key=key, digest_size=16)`, an
    /// independent implementation of the native keyed 128-bit BLAKE2s.
    #[test]
    fn golden_vectors_match_an_independent_blake2s() {
        assert_eq!(
            hex(&announce()),
            "434442430101d048a1a2a3a4a5a6a7a892b8d859e78800602065cc6dec35040d"
        );
        assert_eq!(
            hex(&probe()),
            "4344424301020000a1a2a3a4a5a6a7a803fff8a341c88574fb0f2f6fa5d3d021"
        );
    }

    #[test]
    fn the_tag_is_native_blake2s_128_not_a_truncated_blake2s_256() {
        // The first 16 bytes of the keyed 256-bit digest of the same input.
        let truncated = "39ce13b135be84a8190085f961e55717";
        assert_ne!(hex(&announce()[SIGNED_LEN..]), truncated);
    }

    #[test]
    fn announce_and_probe_round_trip() {
        let a = Beacon::decode(&announce(), &KEY).unwrap();
        assert_eq!(a.kind(), BeaconKind::Announce);
        assert_eq!(a.tcp_port(), 53320);
        assert_eq!(a.nonce(), NONCE);
        assert_eq!(a.encode(&KEY), announce());

        let p = Beacon::decode(&probe(), &KEY).unwrap();
        assert_eq!(p.kind(), BeaconKind::Probe);
        assert_eq!(p.tcp_port(), 0);
        assert_eq!(p.encode(&KEY), probe());
    }

    #[test]
    fn constructors_refuse_what_the_decoder_would_refuse() {
        assert_eq!(Beacon::announce(0, NONCE), Err(BeaconError::Port));
        assert_eq!(Beacon::probe(NONCE).tcp_port(), 0);
    }

    #[test]
    fn wrong_length_is_rejected() {
        let good = announce();
        assert_eq!(Beacon::decode(&[], &KEY), Err(BeaconError::Length));
        for len in 0..BEACON_LEN {
            assert_eq!(
                Beacon::decode(&good[..len], &KEY),
                Err(BeaconError::Length),
                "truncated to {len}"
            );
        }
        let mut longer = good.to_vec();
        longer.push(0);
        assert_eq!(Beacon::decode(&longer, &KEY), Err(BeaconError::Length));
        assert_eq!(
            Beacon::decode(&[0u8; 65_535], &KEY),
            Err(BeaconError::Length)
        );
    }

    #[test]
    fn wrong_magic_version_and_kind_are_rejected() {
        for index in 0..4 {
            let mut bytes = announce();
            bytes[index] ^= 0x01;
            assert_eq!(
                Beacon::decode(&retag(bytes), &KEY),
                Err(BeaconError::Magic),
                "magic byte {index}"
            );
        }
        for version in [0u8, 2, 0xFF] {
            let mut bytes = announce();
            bytes[4] = version;
            assert_eq!(
                Beacon::decode(&retag(bytes), &KEY),
                Err(BeaconError::Version)
            );
        }
        for kind in [0u8, 3, 0x80, 0xFF] {
            let mut bytes = announce();
            bytes[5] = kind;
            assert_eq!(Beacon::decode(&retag(bytes), &KEY), Err(BeaconError::Kind));
        }
    }

    #[test]
    fn the_port_must_match_the_kind() {
        let mut zero_announce = announce();
        zero_announce[6..8].copy_from_slice(&0u16.to_be_bytes());
        assert_eq!(
            Beacon::decode(&retag(zero_announce), &KEY),
            Err(BeaconError::Port)
        );
        let mut port_probe = probe();
        port_probe[6..8].copy_from_slice(&1u16.to_be_bytes());
        assert_eq!(
            Beacon::decode(&retag(port_probe), &KEY),
            Err(BeaconError::Port)
        );
    }

    #[test]
    fn a_bad_tag_is_rejected_whichever_bit_flips() {
        for bit in 0..(BEACON_LEN * 8) {
            let mut bytes = announce();
            bytes[bit / 8] ^= 1 << (bit % 8);
            assert!(
                Beacon::decode(&bytes, &KEY).is_err(),
                "bit {bit} flipped but the datagram still decoded"
            );
        }
        // Flipping a tag bit leaves every structural field valid, so it must fail at the tag.
        let mut bytes = announce();
        bytes[31] ^= 0x80;
        assert_eq!(Beacon::decode(&bytes, &KEY), Err(BeaconError::Tag));
    }

    #[test]
    fn a_tag_from_another_key_is_rejected() {
        let foreign = Beacon::announce(53320, NONCE).unwrap().encode(&OTHER_KEY);
        assert_eq!(Beacon::decode(&foreign, &KEY), Err(BeaconError::Tag));
        assert_eq!(
            Beacon::decode(&foreign, &OTHER_KEY).unwrap().tcp_port(),
            53320
        );
        // A key that differs in one bit is just as wrong.
        let mut near = KEY;
        near[31] ^= 1;
        assert_eq!(Beacon::decode(&announce(), &near), Err(BeaconError::Tag));
    }

    #[test]
    fn a_replay_with_a_modified_port_is_rejected() {
        let mut replay = announce();
        replay[6..8].copy_from_slice(&6666u16.to_be_bytes());
        assert_eq!(Beacon::decode(&replay, &KEY), Err(BeaconError::Tag));
    }

    #[test]
    fn a_probe_cannot_be_turned_into_an_announce() {
        // Same tag, kind byte and port edited: the kind is inside the tagged bytes.
        let mut forged = probe();
        forged[5] = BeaconKind::Announce as u8;
        forged[6..8].copy_from_slice(&53320u16.to_be_bytes());
        assert_eq!(Beacon::decode(&forged, &KEY), Err(BeaconError::Tag));
    }

    #[test]
    fn the_nonce_is_covered_and_changes_the_tag() {
        let one = Beacon::announce(53320, [1; 8]).unwrap().encode(&KEY);
        let two = Beacon::announce(53320, [2; 8]).unwrap().encode(&KEY);
        assert_ne!(one[SIGNED_LEN..], two[SIGNED_LEN..]);
        let mut spliced = one;
        spliced[8..16].copy_from_slice(&[2; 8]);
        assert_eq!(Beacon::decode(&spliced, &KEY), Err(BeaconError::Tag));
    }

    #[test]
    fn debug_output_has_no_tag_or_key() {
        let shown = format!("{:?}", Beacon::decode(&announce(), &KEY).unwrap());
        assert!(!shown.contains("dd3d41b3"));
        assert!(!shown.to_lowercase().contains("key"));
    }
}
