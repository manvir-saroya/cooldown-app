//! Static identities: X25519 key pairs and the `device_id` derived from a public key.

use std::fmt;

use blake2::{Blake2s256, Digest};
use curve25519_dalek::constants::X25519_BASEPOINT;
use curve25519_dalek::montgomery::MontgomeryPoint;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::{Zeroize, Zeroizing};

use crate::codec::{hex_decode_fixed, hex_encode};
use crate::error::IdentityError;
use crate::noise::NOISE_PARAMS_RECONNECT;

/// An X25519 public key.
pub type PublicKey = [u8; 32];

/// A 16-byte device identifier: the first 16 bytes of `BLAKE2s-256(static_pub)`.
///
/// It names a device in records and logs. It is not a secret and not an authenticator; the static
/// public key is what the handshake authenticates.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub [u8; 16]);

impl DeviceId {
    /// Derive the id of a static public key.
    pub fn from_public(public: &PublicKey) -> Self {
        let digest = Blake2s256::digest(public);
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        DeviceId(id)
    }

    /// Lowercase hex (32 characters), the form used in `SyncPayload.deviceId`.
    pub fn to_hex(&self) -> String {
        hex_encode(&self.0)
    }

    /// Parse the 32-character lowercase hex form.
    pub fn from_hex(text: &str) -> Option<Self> {
        hex_decode_fixed::<16>(text).map(DeviceId)
    }

    /// The first 4 bytes as hex: the only part of an id that may appear in logs.
    pub fn short(&self) -> String {
        hex_encode(&self.0[..4])
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceId({})", self.short())
    }
}

impl Serialize for DeviceId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        DeviceId::from_hex(&text).ok_or_else(|| serde::de::Error::custom("invalid device id"))
    }
}

/// True when `public` has small order, so any X25519 with it yields the all-zero secret.
///
/// Snow does not reject an all-zero DH output, so every public key we learn outside a handshake
/// (the QR key, a phone's static key) is screened here. Multiplying by a clamped scalar clears the
/// cofactor, so the product is zero exactly for the small-order points.
pub fn is_small_order(public: &PublicKey) -> bool {
    MontgomeryPoint(*public).mul_clamped([0x55; 32]).to_bytes() == [0u8; 32]
}

/// A static X25519 key pair. The private half is wiped on drop and never printed.
pub struct StaticKeypair {
    private: Zeroizing<[u8; 32]>,
    public: PublicKey,
}

impl StaticKeypair {
    /// Generate a fresh pair with `snow::Builder::generate_keypair`.
    pub fn generate() -> Result<Self, IdentityError> {
        let params = NOISE_PARAMS_RECONNECT
            .parse()
            .map_err(|_| IdentityError::Rng)?;
        let mut pair = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|_| IdentityError::Rng)?;
        let mut private = Zeroizing::new([0u8; 32]);
        let mut public = [0u8; 32];
        let ok = pair.private.len() == 32 && pair.public.len() == 32;
        if ok {
            private.copy_from_slice(&pair.private);
            public.copy_from_slice(&pair.public);
        }
        pair.private.zeroize();
        if ok {
            Ok(StaticKeypair { private, public })
        } else {
            Err(IdentityError::Rng)
        }
    }

    /// Rebuild a pair from a stored private key, deriving the public key (X25519 of the base
    /// point). Rejects an all-zero private key.
    pub fn from_private(private: Zeroizing<[u8; 32]>) -> Result<Self, IdentityError> {
        if private.iter().all(|&b| b == 0) {
            return Err(IdentityError::Malformed);
        }
        let public = X25519_BASEPOINT.mul_clamped(*private).to_bytes();
        Ok(StaticKeypair { private, public })
    }

    /// The public key.
    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    /// The private key, for the handshake builder and for persisting.
    pub(crate) fn private(&self) -> &[u8; 32] {
        &self.private
    }

    /// A wiped-on-drop copy of the private key, for identity stores.
    pub fn private_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(*self.private)
    }

    /// This identity's device id.
    pub fn device_id(&self) -> DeviceId {
        DeviceId::from_public(&self.public)
    }

    /// An independent copy.
    pub fn duplicate(&self) -> Self {
        StaticKeypair {
            private: Zeroizing::new(*self.private),
            public: self.public,
        }
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "StaticKeypair({}, private: <redacted>)",
            self.device_id()
        )
    }
}

/// Fill `buf` from the operating system's random number generator.
pub fn random_bytes(buf: &mut [u8]) -> Result<(), IdentityError> {
    getrandom::fill(buf).map_err(|_| IdentityError::Rng)
}

/// 32 random bytes in a wiped-on-drop box.
pub fn random_secret() -> Result<Zeroizing<[u8; 32]>, IdentityError> {
    let mut out = Zeroizing::new([0u8; 32]);
    random_bytes(&mut *out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::hex_decode_fixed;

    #[test]
    fn rfc7748_public_key_derivation() {
        // RFC 7748 section 6.1, Alice.
        let private = hex_decode_fixed::<32>(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        )
        .unwrap();
        let expected = hex_decode_fixed::<32>(
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
        )
        .unwrap();
        let pair = StaticKeypair::from_private(Zeroizing::new(private)).unwrap();
        assert_eq!(pair.public(), &expected);
    }

    #[test]
    fn generated_pairs_are_consistent_with_derivation() {
        let pair = StaticKeypair::generate().unwrap();
        let again = StaticKeypair::from_private(pair.private_bytes()).unwrap();
        assert_eq!(pair.public(), again.public());
        assert_eq!(pair.device_id(), again.device_id());
        let other = StaticKeypair::generate().unwrap();
        assert_ne!(pair.public(), other.public());
    }

    #[test]
    fn debug_never_shows_the_private_key() {
        let pair = StaticKeypair::generate().unwrap();
        let shown = format!("{pair:?}");
        assert!(shown.contains("redacted"));
        assert!(!shown.contains(&hex_encode(pair.private())));
    }

    #[test]
    fn device_id_is_truncated_blake2s_of_the_public_key() {
        let public = [7u8; 32];
        let digest = Blake2s256::digest(public);
        assert_eq!(DeviceId::from_public(&public).0[..], digest[..16]);
        let id = DeviceId::from_public(&public);
        assert_eq!(DeviceId::from_hex(&id.to_hex()), Some(id));
        assert_eq!(id.short().len(), 8);
    }

    #[test]
    fn small_order_points_are_detected() {
        assert!(is_small_order(&[0u8; 32]));
        let mut one = [0u8; 32];
        one[0] = 1;
        assert!(is_small_order(&one));
        // The order-8 point from the curve25519 small-subgroup list.
        let order8 = hex_decode_fixed::<32>(
            "e0eb7a7c3b41b8ae1656e3faf19fc46ada098deb9c32b1fd866205165f49b800",
        )
        .unwrap();
        assert!(is_small_order(&order8));
        let pair = StaticKeypair::generate().unwrap();
        assert!(!is_small_order(pair.public()));
    }

    #[test]
    fn zero_private_key_is_rejected() {
        assert!(StaticKeypair::from_private(Zeroizing::new([0u8; 32])).is_err());
    }
}
