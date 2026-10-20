//! The phone's static Noise key at rest.
//!
//! On Android the 32 raw bytes are wrapped (AES-256-GCM) by a non-exportable Android Keystore key
//! held by the Kotlin plugin (`wrapKey` / `unwrapKey`); only the wrapped blob is written to disk
//! (`phone-identity.wrapped`, app-private, excluded from backup). Off Android (desktop builds of
//! this crate, tests) a pass-through wrapper is used: development only.
//!
//! A raw key left by the spike (`phone-identity.key`) is wrapped once and the raw file deleted.

use std::fs;
use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use cooldown_sync::StaticKeypair;
use zeroize::Zeroizing;

const WRAPPED_FILE: &str = "phone-identity.wrapped";
const LEGACY_RAW_FILE: &str = "phone-identity.key";

/// Seals and opens the raw key.
pub trait KeyWrapper {
    /// Wrap 32 raw bytes into an opaque text blob.
    fn wrap(&self, raw: &[u8; 32]) -> Result<String, String>;
    /// Open a blob made by `wrap`.
    fn unwrap(&self, blob: &str) -> Result<Zeroizing<[u8; 32]>, String>;
}

/// Development only: the "blob" is the base64 of the raw key.
pub struct PassThrough;

impl KeyWrapper for PassThrough {
    fn wrap(&self, raw: &[u8; 32]) -> Result<String, String> {
        Ok(STANDARD.encode(raw))
    }

    fn unwrap(&self, blob: &str) -> Result<Zeroizing<[u8; 32]>, String> {
        decode_raw(blob)
    }
}

/// Base64 text to exactly 32 bytes.
pub fn decode_raw(text: &str) -> Result<Zeroizing<[u8; 32]>, String> {
    let bytes = Zeroizing::new(
        STANDARD
            .decode(text.trim())
            .map_err(|_| "The stored phone key is damaged.".to_owned())?,
    );
    let mut raw = Zeroizing::new([0u8; 32]);
    if bytes.len() != raw.len() {
        return Err("The stored phone key is damaged.".into());
    }
    raw.copy_from_slice(&bytes);
    Ok(raw)
}

/// Base64 of 32 raw bytes, in a buffer that is wiped on drop.
pub fn encode_raw(raw: &[u8; 32]) -> Zeroizing<String> {
    Zeroizing::new(STANDARD.encode(raw))
}

fn store(dir: &Path, pair: &StaticKeypair, wrapper: &dyn KeyWrapper) -> Result<(), String> {
    let blob = wrapper.wrap(&pair.private_bytes())?;
    let tmp = dir.join(format!("{WRAPPED_FILE}.tmp"));
    fs::write(&tmp, blob.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, dir.join(WRAPPED_FILE)).map_err(|e| e.to_string())
}

/// Load the phone key from `dir`, migrating a raw spike key, or create one.
///
/// A wrapped key that cannot be opened is an error, never a silent new identity: a new key would
/// mean every PC has to pair again, and the user should be told why.
pub fn load_or_create(dir: &Path, wrapper: &dyn KeyWrapper) -> Result<StaticKeypair, String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if let Ok(blob) = fs::read_to_string(dir.join(WRAPPED_FILE)) {
        let raw = wrapper.unwrap(&blob)?;
        return StaticKeypair::from_private(raw).map_err(|e| e.to_string());
    }
    let legacy = dir.join(LEGACY_RAW_FILE);
    if let Ok(bytes) = fs::read(&legacy) {
        let bytes = Zeroizing::new(bytes);
        let mut raw = Zeroizing::new([0u8; 32]);
        if bytes.len() != raw.len() {
            return Err("The stored phone key is damaged.".into());
        }
        raw.copy_from_slice(&bytes);
        let pair = StaticKeypair::from_private(raw).map_err(|e| e.to_string())?;
        store(dir, &pair, wrapper)?;
        let _ = fs::remove_file(&legacy);
        return Ok(pair);
    }
    let pair = StaticKeypair::generate().map_err(|e| e.to_string())?;
    store(dir, &pair, wrapper)?;
    Ok(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the Keystore: XOR with a fixed pad, so the blob differs from the key.
    struct Xor;

    impl KeyWrapper for Xor {
        fn wrap(&self, raw: &[u8; 32]) -> Result<String, String> {
            let sealed: Vec<u8> = raw.iter().map(|b| b ^ 0x5a).collect();
            Ok(STANDARD.encode(sealed))
        }
        fn unwrap(&self, blob: &str) -> Result<Zeroizing<[u8; 32]>, String> {
            let mut raw = decode_raw(blob)?;
            raw.iter_mut().for_each(|b| *b ^= 0x5a);
            Ok(raw)
        }
    }

    #[test]
    fn created_once_then_reloaded_and_never_stored_raw() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path(), &Xor).unwrap();
        let again = load_or_create(dir.path(), &Xor).unwrap();
        assert_eq!(first.public(), again.public());
        let blob = fs::read_to_string(dir.path().join(WRAPPED_FILE)).unwrap();
        assert_ne!(
            blob,
            *encode_raw(&first.private_bytes()),
            "the raw key is not on disk"
        );
        assert!(!dir.path().join(LEGACY_RAW_FILE).exists());
    }

    #[test]
    fn a_spike_raw_key_is_migrated_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let old = StaticKeypair::generate().unwrap();
        fs::write(dir.path().join(LEGACY_RAW_FILE), *old.private_bytes()).unwrap();
        let loaded = load_or_create(dir.path(), &Xor).unwrap();
        assert_eq!(
            loaded.public(),
            old.public(),
            "same identity, PCs stay paired"
        );
        assert!(!dir.path().join(LEGACY_RAW_FILE).exists());
        assert!(dir.path().join(WRAPPED_FILE).exists());
    }

    #[test]
    fn a_damaged_blob_is_an_error_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(WRAPPED_FILE), "not base64!").unwrap();
        assert!(load_or_create(dir.path(), &Xor).is_err());
        assert!(load_or_create(dir.path(), &PassThrough).is_err());
    }

    #[test]
    fn base64_round_trip() {
        let raw = [7u8; 32];
        assert_eq!(*decode_raw(&encode_raw(&raw)).unwrap(), raw);
        assert!(decode_raw("AAAA").is_err());
    }
}
