//! The 24-byte cleartext `ClientHello` and the server status bytes.
//!
//! ```text
//! 0  4  magic "CDWN"
//! 4  1  proto_min
//! 5  1  proto_max
//! 6  1  mode: 0x01 PAIR | 0x02 RECONNECT
//! 7  1  flags (must be 0)
//! 8  16 pair_id (all zeros for RECONNECT)
//! ```
//!
//! The hello is unauthenticated but it is bound into the Noise prologue, so changing any byte of
//! it in transit makes the handshake fail.

/// First four bytes of every connection.
pub const MAGIC: [u8; 4] = *b"CDWN";
/// Length of the encoded hello.
pub const CLIENT_HELLO_LEN: usize = 24;
/// Lowest protocol version this crate implements.
pub const PROTO_MIN_SUPPORTED: u8 = 1;
/// Highest protocol version this crate implements.
pub const PROTO_MAX_SUPPORTED: u8 = 1;

/// What the connection is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// First pairing: `Noise_IKpsk1`, needs an open pairing window.
    Pair = 0x01,
    /// Reconnect of an already paired phone: `Noise_IK`.
    Reconnect = 0x02,
}

/// Why a hello was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HelloError {
    /// Not 24 bytes.
    #[error("wrong hello length")]
    Length,
    /// The magic is not `CDWN` (for example an HTTP request).
    #[error("bad magic")]
    Magic,
    /// `proto_min` is zero or above `proto_max`.
    #[error("invalid version range")]
    VersionRange,
    /// A mode other than PAIR or RECONNECT.
    #[error("unknown mode")]
    Mode,
    /// Non-zero flags.
    #[error("non-zero flags")]
    Flags,
    /// `pair_id` is zero in PAIR mode or non-zero in RECONNECT mode.
    #[error("pair_id does not match the mode")]
    PairId,
}

/// A parsed `ClientHello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHello {
    /// Lowest version the phone will speak (its stored floor for this PC).
    pub proto_min: u8,
    /// Highest version the phone speaks.
    pub proto_max: u8,
    /// PAIR or RECONNECT.
    pub mode: Mode,
    /// Names the pairing attempt (zeros for RECONNECT).
    pub pair_id: [u8; 16],
}

impl ClientHello {
    /// Encode to the 24 wire bytes.
    pub fn encode(&self) -> [u8; CLIENT_HELLO_LEN] {
        let mut out = [0u8; CLIENT_HELLO_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.proto_min;
        out[5] = self.proto_max;
        out[6] = self.mode as u8;
        out[7] = 0;
        out[8..24].copy_from_slice(&self.pair_id);
        out
    }

    /// Strictly decode the 24 wire bytes. Never panics.
    pub fn decode(bytes: &[u8]) -> Result<Self, HelloError> {
        let bytes: &[u8; CLIENT_HELLO_LEN] = bytes.try_into().map_err(|_| HelloError::Length)?;
        if bytes[0..4] != MAGIC {
            return Err(HelloError::Magic);
        }
        let (proto_min, proto_max) = (bytes[4], bytes[5]);
        if proto_min == 0 || proto_min > proto_max {
            return Err(HelloError::VersionRange);
        }
        let mode = match bytes[6] {
            0x01 => Mode::Pair,
            0x02 => Mode::Reconnect,
            _ => return Err(HelloError::Mode),
        };
        if bytes[7] != 0 {
            return Err(HelloError::Flags);
        }
        let mut pair_id = [0u8; 16];
        pair_id.copy_from_slice(&bytes[8..24]);
        let zero = pair_id.iter().all(|&b| b == 0);
        match mode {
            Mode::Pair if zero => return Err(HelloError::PairId),
            Mode::Reconnect if !zero => return Err(HelloError::PairId),
            _ => {}
        }
        Ok(ClientHello {
            proto_min,
            proto_max,
            mode,
            pair_id,
        })
    }

    /// The version both sides will use: the highest in `[proto_min, proto_max]` that this crate
    /// supports, or `None` when the ranges do not overlap.
    pub fn negotiate(&self) -> Option<u8> {
        let highest = self.proto_max.min(PROTO_MAX_SUPPORTED);
        let lowest = self.proto_min.max(PROTO_MIN_SUPPORTED);
        (lowest <= highest).then_some(highest)
    }
}

/// The one-byte reply that precedes message 2 (or replaces it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Message 2 follows.
    Ok,
    /// No common version; a second byte carries the server's highest version. Unauthenticated.
    Version,
    /// No pairing window is open (or it does not match). Sent before any DH.
    NotPairing,
    /// A pairing is already in progress.
    Busy,
    /// Handshake rate limit hit.
    RateLimited,
    /// Every crypto or unknown-peer failure, with no further detail. Unauthenticated.
    Fail,
}

impl Status {
    /// The wire byte.
    pub fn to_byte(self) -> u8 {
        match self {
            Status::Ok => 0x00,
            Status::Version => 0x01,
            Status::NotPairing => 0x02,
            Status::Busy => 0x03,
            Status::RateLimited => 0x04,
            Status::Fail => 0x10,
        }
    }

    /// Parse a wire byte; unknown bytes are `None`.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0x00 => Status::Ok,
            0x01 => Status::Version,
            0x02 => Status::NotPairing,
            0x03 => Status::Busy,
            0x04 => Status::RateLimited,
            0x10 => Status::Fail,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_hello() -> ClientHello {
        ClientHello {
            proto_min: 1,
            proto_max: 1,
            mode: Mode::Pair,
            pair_id: [5; 16],
        }
    }

    #[test]
    fn round_trip() {
        let hello = pair_hello();
        assert_eq!(ClientHello::decode(&hello.encode()).unwrap(), hello);
        let reconnect = ClientHello {
            mode: Mode::Reconnect,
            pair_id: [0; 16],
            ..hello
        };
        assert_eq!(ClientHello::decode(&reconnect.encode()).unwrap(), reconnect);
    }

    #[test]
    fn rejects_every_malformation() {
        let good = pair_hello().encode();
        assert_eq!(ClientHello::decode(&good[..23]), Err(HelloError::Length));
        assert_eq!(ClientHello::decode(&[0u8; 25]), Err(HelloError::Length));
        let mut bad = good;
        bad[..4].copy_from_slice(b"GET ");
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::Magic));
        let mut bad = good;
        bad[4] = 0;
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::VersionRange));
        let mut bad = good;
        bad[4] = 3;
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::VersionRange));
        let mut bad = good;
        bad[6] = 9;
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::Mode));
        let mut bad = good;
        bad[7] = 1;
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::Flags));
        let mut bad = good;
        bad[8..].fill(0);
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::PairId));
        let mut bad = good;
        bad[6] = 2;
        assert_eq!(ClientHello::decode(&bad), Err(HelloError::PairId));
    }

    #[test]
    fn negotiation_picks_the_highest_common_version_and_never_downgrades() {
        let mut hello = pair_hello();
        assert_eq!(hello.negotiate(), Some(1));
        hello.proto_min = 1;
        hello.proto_max = 9;
        assert_eq!(hello.negotiate(), Some(PROTO_MAX_SUPPORTED));
        hello.proto_min = 2;
        hello.proto_max = 3;
        assert_eq!(hello.negotiate(), None);
    }

    #[test]
    fn status_bytes_round_trip() {
        for status in [
            Status::Ok,
            Status::Version,
            Status::NotPairing,
            Status::Busy,
            Status::RateLimited,
            Status::Fail,
        ] {
            assert_eq!(Status::from_byte(status.to_byte()), Some(status));
        }
        assert_eq!(Status::from_byte(0x55), None);
    }
}
