//! Where API keys live.
//!
//! Keys are held only in a [`SecretStore`]: the operating system's credential store in production
//! ([`KeyringSecretStore`]: Windows Credential Manager, macOS Keychain, Secret Service) and a
//! plain map in tests ([`InMemorySecretStore`]). They are never written to configuration, never
//! logged, never part of a snapshot and never synced.
//!
//! A [`SecretString`] cannot be printed by accident: `Debug` shows `[redacted]` and there is no
//! `Display` or `Serialize`.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use crate::scrub::scrub;

/// The keyring service name every Cooldown secret is stored under.
pub const KEYRING_SERVICE: &str = "cooldown";

/// Longest API key accepted. Real keys are well under 200 bytes.
const MAX_KEY_LEN: usize = 512;

/// A secret value that refuses to be printed.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a secret. Surrounding whitespace (a pasted newline) is trimmed.
    pub fn new(value: impl AsRef<str>) -> Self {
        SecretString(value.as_ref().trim().to_owned())
    }

    /// The secret itself. Call this only where it goes on the wire.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when the value is usable as an HTTP bearer token: 1 to 512 visible ASCII characters
    /// (no spaces, no control characters, so it can never break out of a header).
    pub fn is_valid_api_key(&self) -> bool {
        !self.0.is_empty()
            && self.0.len() <= MAX_KEY_LEN
            && self.0.bytes().all(|b| (0x21..=0x7e).contains(&b))
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([redacted])")
    }
}

/// Why a secret could not be read or written. The text never contains the secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    /// The credential store is missing, locked or refused access.
    #[error("credential store unavailable: {0}")]
    Unavailable(String),
    /// The name or value is not acceptable (empty, too long, control characters).
    #[error("invalid secret name or value")]
    Invalid,
}

/// The name a secret is stored under: `<adapter id>:<account label>`, for example
/// `openrouter:default`. The Settings screen that saves a key uses the same function, so the
/// adapter finds it.
pub fn secret_name(adapter_id: &str, account: &str) -> String {
    format!("{adapter_id}:{account}")
}

/// A place to keep API keys.
///
/// Implementations are synchronous; adapters call them from `spawn_blocking`.
pub trait SecretStore: Send + Sync + fmt::Debug {
    /// The secret stored under `name`, or `None` when there is none.
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError>;
    /// Store (replace) a secret.
    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError>;
    /// Remove a secret. Removing a missing one succeeds.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

fn check_name(name: &str) -> Result<(), SecretError> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        Err(SecretError::Invalid)
    } else {
        Ok(())
    }
}

/// Secrets in the operating system's credential store, via the `keyring` crate.
///
/// Only entries under Cooldown's own service name are ever touched.
#[derive(Debug, Clone)]
pub struct KeyringSecretStore {
    service: String,
}

impl KeyringSecretStore {
    /// The store under the standard [`KEYRING_SERVICE`] name.
    pub fn new() -> Self {
        Self::with_service(KEYRING_SERVICE)
    }

    /// A store under a custom service name (separate profiles, tests that do not touch the
    /// standard entries).
    pub fn with_service(service: impl Into<String>) -> Self {
        KeyringSecretStore {
            service: service.into(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
        check_name(name)?;
        keyring::Entry::new(&self.service, name).map_err(map_keyring_error)
    }
}

impl Default for KeyringSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringSecretStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        match self.entry(name)?.get_password() {
            Ok(password) => Ok(Some(SecretString::new(password))),
            Err(error) => interpret_get_error(error),
        }
    }

    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
        self.entry(name)?
            .set_password(value.expose())
            .map_err(map_keyring_error)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(map_keyring_error(error)),
        }
    }
}

/// A missing entry is "no secret", anything else is an error.
fn interpret_get_error(error: keyring::Error) -> Result<Option<SecretString>, SecretError> {
    match error {
        keyring::Error::NoEntry => Ok(None),
        other => Err(map_keyring_error(other)),
    }
}

fn map_keyring_error(error: keyring::Error) -> SecretError {
    match error {
        keyring::Error::Invalid(..) | keyring::Error::TooLong(..) => SecretError::Invalid,
        other => SecretError::Unavailable(scrub(&other.to_string(), &[])),
    }
}

/// Secrets in memory, for tests and for runs without a credential store.
#[derive(Default)]
pub struct InMemorySecretStore {
    secrets: Mutex<HashMap<String, SecretString>>,
}

impl InMemorySecretStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store holding one secret.
    pub fn with(name: &str, value: &str) -> Self {
        let store = Self::new();
        store
            .secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), SecretString::new(value));
        store
    }
}

impl fmt::Debug for InMemorySecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.secrets.lock().map(|m| m.len()).unwrap_or(0);
        write!(f, "InMemorySecretStore({count} secrets)")
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        check_name(name)?;
        Ok(self
            .secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned())
    }

    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
        check_name(name)?;
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), value.clone());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        check_name(name)?;
        self.secrets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_print() {
        let secret = SecretString::new("sk-very-secret-value");
        assert!(!format!("{secret:?}").contains("very-secret"));
        let store = InMemorySecretStore::with("openrouter:default", "sk-very-secret-value");
        assert!(!format!("{store:?}").contains("very-secret"));
    }

    #[test]
    fn whitespace_is_trimmed_and_validity_is_strict() {
        assert_eq!(SecretString::new("  abc123\n").expose(), "abc123");
        assert!(SecretString::new("sk-or-v1-abc").is_valid_api_key());
        assert!(!SecretString::new("").is_valid_api_key());
        assert!(!SecretString::new("two words").is_valid_api_key());
        assert!(!SecretString::new("bad\u{7}key").is_valid_api_key());
        assert!(!SecretString::new("ключ").is_valid_api_key());
        assert!(!SecretString::new("a".repeat(513)).is_valid_api_key());
    }

    #[test]
    fn in_memory_store_round_trips() {
        let store = InMemorySecretStore::new();
        let name = secret_name("deepseek", "work");
        assert_eq!(name, "deepseek:work");
        assert_eq!(store.get(&name), Ok(None));
        store.set(&name, &SecretString::new("k-123456")).unwrap();
        assert_eq!(store.get(&name).unwrap().unwrap().expose(), "k-123456");
        store.delete(&name).unwrap();
        assert_eq!(store.get(&name), Ok(None));
        store.delete(&name).unwrap();
    }

    #[test]
    fn bad_names_are_rejected() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get(""), Err(SecretError::Invalid));
        assert_eq!(store.get("a\nb"), Err(SecretError::Invalid));
        assert_eq!(store.get(&"x".repeat(300)), Err(SecretError::Invalid));
    }

    #[test]
    fn keyring_errors_map_without_touching_the_keychain() {
        assert_eq!(interpret_get_error(keyring::Error::NoEntry), Ok(None));
        assert!(matches!(
            interpret_get_error(keyring::Error::NoStorageAccess("locked".into())),
            Err(SecretError::Unavailable(_))
        ));
        assert_eq!(
            map_keyring_error(keyring::Error::Invalid("service".into(), "empty".into())),
            SecretError::Invalid
        );
        // Bad names are caught before the keychain is ever reached.
        let store = KeyringSecretStore::with_service("cooldown-test-never-used");
        assert_eq!(store.get(""), Err(SecretError::Invalid));
    }
}
