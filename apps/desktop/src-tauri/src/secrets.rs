//! Where the desktop app keeps API keys: the OS credential store, with Windows credentials pinned
//! to this machine.
//!
//! `cooldown_adapters::KeyringSecretStore` goes through the `keyring` v1 wrapper, which on Windows
//! creates credentials as `CRED_PERSIST_ENTERPRISE`: Microsoft documents that level as visible to
//! this user's logon sessions on other computers (roaming profiles). This store uses `keyring-core`
//! and the native store directly, like `cooldown-sync`'s `KeyringIdentityStore`, so every entry
//! is built with `persistence=local`, and a key found at another level is rewritten when read.
//! Service and entry names are the same as `KeyringSecretStore`'s, so keys saved by either are found.
//!
//! If the native store cannot be opened, Windows does **not** fall back to the `keyring` wrapper
//! (that would silently save keys at the roaming level): it refuses to save keys and says so
//! ([`open_secret_store`]). Other platforms keep the fallback and log a warning.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use cooldown_adapters::secrets::{SecretError, KEYRING_SERVICE};
use cooldown_adapters::{KeyringSecretStore, SecretStore, SecretString};
use keyring_core::api::CredentialStore;
use keyring_core::Error as KeyringError;

/// API keys in the OS credential store.
pub struct NativeSecretStore {
    store: Arc<CredentialStore>,
    service: String,
    modifiers: Vec<(String, String)>,
}

impl fmt::Debug for NativeSecretStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeSecretStore")
            .field("service", &self.service)
            .field("modifiers", &self.modifiers)
            .finish_non_exhaustive()
    }
}

/// What the UI shows when keys cannot be saved because Windows Credential Manager did not open.
pub const WINDOWS_STORE_UNAVAILABLE: &str =
    "Cooldown could not open Windows Credential Manager, so it can't save keys on this PC.";

/// A secret store that holds nothing and saves nothing, because the real one could not be opened.
/// Reads, writes and deletes all fail with the same sentence, which the UI shows as is.
#[derive(Debug)]
pub struct UnavailableSecretStore {
    message: String,
}

impl UnavailableSecretStore {
    /// A store whose every operation fails with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        UnavailableSecretStore {
            message: message.into(),
        }
    }

    fn error(&self) -> SecretError {
        SecretError::Unavailable(self.message.clone())
    }
}

impl SecretStore for UnavailableSecretStore {
    fn get(&self, _name: &str) -> Result<Option<SecretString>, SecretError> {
        Err(self.error())
    }

    fn set(&self, _name: &str, _value: &SecretString) -> Result<(), SecretError> {
        Err(self.error())
    }

    fn delete(&self, _name: &str) -> Result<(), SecretError> {
        Err(self.error())
    }
}

/// What to use when [`NativeSecretStore::native`] fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The `keyring` wrapper (works, but on Windows it saves at the roaming level).
    Keyring,
    /// Nothing: keys cannot be saved.
    Refuse,
}

/// Windows fails closed because the wrapper's Windows credentials can roam to other computers;
/// elsewhere the wrapper is as safe as the native store.
pub const fn fallback_for(windows: bool) -> Fallback {
    if windows {
        Fallback::Refuse
    } else {
        Fallback::Keyring
    }
}

/// The store for API keys: the native one, or the platform's fallback (with a warning in the log)
/// when it cannot be opened.
pub fn open_secret_store() -> Arc<dyn SecretStore> {
    store_from(NativeSecretStore::native(), cfg!(windows))
}

/// [`open_secret_store`] with the native result and the platform passed in (tests).
pub fn store_from(
    native: Result<NativeSecretStore, SecretError>,
    windows: bool,
) -> Arc<dyn SecretStore> {
    match native {
        Ok(store) => Arc::new(store),
        Err(error) => match fallback_for(windows) {
            Fallback::Refuse => {
                log::warn!(
                    "could not open the credential store ({error}); API keys cannot be saved"
                );
                Arc::new(UnavailableSecretStore::new(WINDOWS_STORE_UNAVAILABLE))
            }
            Fallback::Keyring => {
                log::warn!(
                    "the native credential store is unavailable ({error}); using the keyring fallback"
                );
                Arc::new(KeyringSecretStore::new())
            }
        },
    }
}

/// The sentence the UI shows for a failed key operation. A refusal from
/// [`UnavailableSecretStore`] is already a full sentence; other errors keep their usual wording.
pub fn describe_error(error: &SecretError) -> String {
    match error {
        SecretError::Unavailable(message) if message == WINDOWS_STORE_UNAVAILABLE => {
            message.clone()
        }
        other => other.to_string(),
    }
}

fn unavailable(error: KeyringError) -> SecretError {
    SecretError::Unavailable(cooldown_adapters::scrub::scrub(&error.to_string(), &[]))
}

impl NativeSecretStore {
    /// The platform store: Windows Credential Manager (persistence `local`) or the macOS Keychain.
    pub fn native() -> Result<Self, SecretError> {
        #[cfg(target_os = "windows")]
        {
            let store = windows_native_keyring_store::Store::new().map_err(unavailable)?;
            Ok(Self::with_store(
                store,
                KEYRING_SERVICE,
                &[("persistence", "local")],
            ))
        }
        #[cfg(target_os = "macos")]
        {
            let store = apple_native_keyring_store::keychain::Store::new().map_err(unavailable)?;
            Ok(Self::with_store(store, KEYRING_SERVICE, &[]))
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            Err(SecretError::Unavailable(
                "no credential store on this platform".into(),
            ))
        }
    }

    /// Any `keyring-core` store (tests pass the mock). `modifiers` are applied to every entry.
    pub fn with_store(
        store: Arc<CredentialStore>,
        service: &str,
        modifiers: &[(&str, &str)],
    ) -> Self {
        NativeSecretStore {
            store,
            service: service.to_owned(),
            modifiers: modifiers
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    fn entry(&self, name: &str) -> Result<keyring_core::Entry, SecretError> {
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(SecretError::Invalid);
        }
        let modifiers: HashMap<&str, &str> = self
            .modifiers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let modifiers = (!modifiers.is_empty()).then_some(&modifiers);
        self.store
            .build(&self.service, name, modifiers)
            .map_err(unavailable)
    }

    fn desired_persistence(&self) -> Option<&str> {
        self.modifiers
            .iter()
            .find(|(k, _)| k == "persistence")
            .map(|(_, v)| v.as_str())
    }
}

impl SecretStore for NativeSecretStore {
    fn get(&self, name: &str) -> Result<Option<SecretString>, SecretError> {
        let entry = self.entry(name)?;
        let secret = match entry.get_password() {
            Ok(text) => SecretString::new(text),
            Err(KeyringError::NoEntry) => return Ok(None),
            Err(KeyringError::BadEncoding(_)) => return Err(SecretError::Invalid),
            Err(other) => return Err(unavailable(other)),
        };
        // A key saved at another level (older build, the keyring wrapper) is rewritten locally.
        if let Some(want) = self.desired_persistence() {
            if let Ok(attributes) = entry.get_attributes() {
                let has = attributes.get("persistence").map(String::as_str);
                if has.is_some_and(|has| !has.eq_ignore_ascii_case(want)) {
                    self.set(name, &secret)?;
                }
            }
        }
        Ok(Some(secret))
    }

    fn set(&self, name: &str, value: &SecretString) -> Result<(), SecretError> {
        if value.expose().is_empty() {
            return Err(SecretError::Invalid);
        }
        self.entry(name)?
            .set_password(value.expose())
            .map_err(unavailable)
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(other) => Err(unavailable(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> NativeSecretStore {
        NativeSecretStore::with_store(keyring_core::mock::Store::new().unwrap(), "svc", &[])
    }

    #[test]
    fn round_trip() {
        let secrets = store();
        assert!(secrets.get("openrouter:default").unwrap().is_none());
        secrets
            .set("openrouter:default", &SecretString::new("sk-1"))
            .unwrap();
        assert_eq!(
            secrets.get("openrouter:default").unwrap().unwrap().expose(),
            "sk-1"
        );
        secrets.delete("openrouter:default").unwrap();
        assert!(secrets.get("openrouter:default").unwrap().is_none());
        secrets.delete("openrouter:default").unwrap();
    }

    #[test]
    fn bad_names_and_empty_values_are_refused() {
        let secrets = store();
        assert_eq!(secrets.get(""), Err(SecretError::Invalid));
        assert_eq!(secrets.get("a\nb"), Err(SecretError::Invalid));
        assert_eq!(
            secrets.set("x", &SecretString::new("  ")),
            Err(SecretError::Invalid)
        );
    }

    #[test]
    fn windows_fails_closed_and_other_platforms_keep_the_fallback() {
        assert_eq!(fallback_for(true), Fallback::Refuse);
        assert_eq!(fallback_for(false), Fallback::Keyring);
    }

    #[test]
    fn on_windows_a_failed_native_store_saves_nothing_and_says_why() {
        let native = Err(SecretError::Unavailable("no access".into()));
        let secrets = store_from(native, true);
        let failure = secrets
            .set("openrouter:default", &SecretString::new("sk-or-v1-abc"))
            .unwrap_err();
        assert_eq!(
            describe_error(&failure),
            "Cooldown could not open Windows Credential Manager, so it can't save keys on this PC."
        );
        // Nothing is readable or deletable either, and the reason stays the same sentence.
        assert!(secrets.get("openrouter:default").is_err());
        assert!(secrets.delete("openrouter:default").is_err());
        assert!(!format!("{secrets:?}").contains("Keyring"));
    }

    #[test]
    fn elsewhere_a_failed_native_store_falls_back_to_the_keyring_wrapper() {
        // Only built here, never called: the wrapper would reach the real OS credential store.
        let secrets = store_from(Err(SecretError::Unavailable("no access".into())), false);
        assert!(format!("{secrets:?}").contains("KeyringSecretStore"));
    }

    #[test]
    fn a_working_native_store_is_used_on_every_platform() {
        for windows in [true, false] {
            let secrets = store_from(Ok(store()), windows);
            secrets.set("x", &SecretString::new("sk-1")).unwrap();
            assert_eq!(secrets.get("x").unwrap().unwrap().expose(), "sk-1");
        }
    }

    #[test]
    fn other_errors_keep_their_usual_wording() {
        assert_eq!(
            describe_error(&SecretError::Unavailable("locked".into())),
            "credential store unavailable: locked"
        );
        assert_eq!(
            describe_error(&SecretError::Invalid),
            "invalid secret name or value"
        );
    }

    #[test]
    fn debug_never_shows_a_key() {
        let secrets = store();
        secrets.set("x", &SecretString::new("sk-secret")).unwrap();
        assert!(!format!("{secrets:?}").contains("sk-secret"));
    }
}
