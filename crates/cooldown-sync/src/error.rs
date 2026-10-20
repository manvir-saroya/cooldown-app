//! Error types. None of them carries key material, payload text or device names, so they are safe
//! to log or show.

use std::io;

use crate::codec::Base32Error;

/// Why a QR payload was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QrError {
    /// The text is longer than any valid payload.
    #[error("QR text too long")]
    TooLong,
    /// The text does not start with `CD1:`.
    #[error("not a Cooldown QR payload")]
    BadPrefix,
    /// The base32 body is malformed.
    #[error("malformed base32 body: {0}")]
    Base32(#[from] Base32Error),
    /// The binary payload is shorter than the fixed header, or cut off inside an address.
    #[error("QR payload truncated")]
    Truncated,
    /// Bytes remain after the last address.
    #[error("trailing bytes after the last address")]
    TrailingBytes,
    /// `qr_version` is not 1.
    #[error("unsupported QR version")]
    UnsupportedVersion,
    /// The address count is not 1 to 4.
    #[error("address count out of range")]
    AddressCount,
    /// An address family byte other than 0x04 or 0x06.
    #[error("unknown address family")]
    AddressFamily,
    /// An address that can never be dialled (unspecified, multicast, broadcast, link-local IPv6,
    /// IPv4-mapped IPv6) or a duplicate.
    #[error("unusable address")]
    BadAddress,
    /// Port 0.
    #[error("invalid port")]
    BadPort,
    /// The PC public key is a low-order point (all-zero DH output).
    #[error("invalid public key")]
    BadKey,
}

/// Why a discovery beacon datagram was rejected (or could not be built).
///
/// The decoder reports the first check that failed, in this order, but a caller that receives
/// datagrams from the network must treat every variant the same way: drop it silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BeaconError {
    /// Not exactly 32 bytes (short, truncated or oversized).
    #[error("wrong beacon length")]
    Length,
    /// The magic is not `CDBC`.
    #[error("bad beacon magic")]
    Magic,
    /// A version other than 1.
    #[error("unsupported beacon version")]
    Version,
    /// A kind byte other than ANNOUNCE (0x01) or PROBE (0x02).
    #[error("unknown beacon kind")]
    Kind,
    /// An ANNOUNCE with port 0, or a PROBE with a non-zero port.
    #[error("beacon port does not match its kind")]
    Port,
    /// The keyed tag does not verify (wrong key, or a field was altered).
    #[error("beacon tag mismatch")]
    Tag,
}

/// A framing or envelope violation.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer closed the connection cleanly between frames.
    #[error("connection closed")]
    Closed,
    /// The stream ended inside a frame.
    #[error("truncated frame")]
    Truncated,
    /// A zero-length frame.
    #[error("zero-length frame")]
    ZeroLength,
    /// A frame longer than the limit for this stage.
    #[error("oversized frame")]
    Oversize,
    /// The underlying socket failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

/// A malformed decrypted envelope (`type | flags | body_len | body | padding`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    /// Shorter than the 4-byte header.
    #[error("envelope shorter than its header")]
    Short,
    /// Non-zero flags.
    #[error("non-zero envelope flags")]
    Flags,
    /// A message type this version does not define.
    #[error("unknown message type")]
    UnknownType,
    /// `body_len` above the 61,440-byte cap.
    #[error("body too long")]
    BodyTooLong,
    /// `body_len` runs past the end of the plaintext.
    #[error("body runs past the envelope")]
    Truncated,
    /// A non-zero byte in the padding.
    #[error("non-zero padding")]
    Padding,
}

/// A trust-store failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Reading or writing the backing file failed.
    #[error("trust store i/o: {0}")]
    Io(#[from] io::Error),
    /// The stored data is not valid. The store is left untouched; it is never silently reset.
    #[error("trust store is corrupt: {0}")]
    Corrupt(&'static str),
}

/// An identity-store failure.
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    /// The platform credential store is unavailable or refused.
    #[error("credential store: {0}")]
    Backend(String),
    /// The stored secret is not a 32-byte key.
    #[error("stored identity is malformed")]
    Malformed,
    /// No credential store exists for this platform; supply your own `IdentityStore`.
    #[error("no credential store for this platform")]
    Unsupported,
    /// The system random number generator failed.
    #[error("random number generator failed")]
    Rng,
}

/// Why a PC-side API call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServerError {
    /// A pairing window is already open (one at a time).
    #[error("a pairing window is already open")]
    PairingAlreadyOpen,
    /// The PC already has the maximum number of paired phones.
    #[error("the maximum number of paired phones is reached")]
    PeerLimit,
    /// There is no pairing waiting for confirmation.
    #[error("no pairing is waiting for confirmation")]
    NoPendingPairing,
    /// The typed code did not match; the pairing was rejected.
    #[error("confirmation code mismatch")]
    WrongCode,
    /// The payload is over a size limit or contains something that must not be sent.
    #[error("sync payload rejected: {0}")]
    Payload(&'static str),
    /// No such paired device.
    #[error("unknown device")]
    UnknownDevice,
    /// The QR needs at least one usable address.
    #[error("no address to advertise")]
    NoAddress,
    /// Persisting trust data failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Loading or creating the identity key failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Binding or using a socket failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The system random number generator failed.
    #[error("random number generator failed")]
    Rng,
}

/// Why a phone-side call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The QR text was rejected.
    #[error("invalid QR payload: {0}")]
    Qr(#[from] QrError),
    /// No address worked.
    #[error("could not connect to the PC")]
    Connect,
    /// The PC speaks no version we offer. The report is unauthenticated, so it never lowers what
    /// we offer next time.
    #[error("the PC does not support our protocol version")]
    VersionMismatch {
        /// Highest version the (unauthenticated) reply claims.
        server_max: u8,
    },
    /// Unauthenticated `NOT_PAIRING`.
    #[error("the PC is not in pairing mode")]
    NotPairing,
    /// Unauthenticated `BUSY`.
    #[error("the PC is busy with another pairing")]
    Busy,
    /// Unauthenticated `RATE_LIMITED`.
    #[error("the PC is rate limiting us")]
    RateLimited,
    /// Unauthenticated `FAIL`. Callers MUST NOT unpair because of it: the byte can be forged.
    #[error("the PC rejected the handshake")]
    Rejected,
    /// The PC refused the pairing (user rejected, timed out, or a revoke arrived).
    #[error("pairing was not confirmed")]
    PairingRefused,
    /// An authenticated `REVOKE`: this phone has been unpaired by the PC.
    #[error("unpaired by the PC")]
    Revoked,
    /// The peer broke the protocol (bad frame, bad message, wrong order).
    #[error("protocol violation: {0}")]
    Protocol(&'static str),
    /// A deadline passed.
    #[error("timed out")]
    Timeout,
    /// The connection failed or was closed.
    #[error("connection closed")]
    Closed,
    /// No such paired PC.
    #[error("unknown PC")]
    UnknownPc,
    /// Persisting trust data failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Loading or creating the identity key failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// The system random number generator failed.
    #[error("random number generator failed")]
    Rng,
}
