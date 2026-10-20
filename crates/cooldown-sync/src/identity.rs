//! Private identity keys.
//!
//! The static X25519 private key never goes through the [`TrustStore`]. It goes
//! through an [`IdentityStore`], so each platform can use its strongest vault:
//!
//! * PC: [`KeyringIdentityStore`] (Windows Credential Manager, macOS Keychain), with the Windows
//!   persistence level forced to *local machine* so the key cannot roam to other computers.
//! * Android: the app implements the trait in Kotlin, wrapping the raw key with a non-exportable
//!   Keystore key (the Keystore cannot perform the X25519 operation itself).
//! * Tests: [`MemoryIdentityStore`]. Tests never touch the real OS keychain.

use std::sync::Mutex;

use zeroize::Zeroizing;

use crate::error::{IdentityError, ServerError};
use crate::keys::StaticKeypair;
use crate::trust::TrustStore;

/// Where a device keeps its static private key.
pub trait IdentityStore: Send + Sync + 'static {
    /// The stored key pair, or `None` if there is none.
    fn load(&self) -> Result<Option<StaticKeypair>, IdentityError>;

    /// Store (or replace) the key pair.
    fn store(&self, keypair: &StaticKeypair) -> Result<(), IdentityError>;

    /// Delete the key pair (identity reset). Deleting nothing is not an error.
    fn delete(&self) -> Result<(), IdentityError>;
}

/// An identity store that keeps the key in process memory only.
#[derive(Default)]
pub struct MemoryIdentityStore {
    key: Mutex<Option<Zeroizing<[u8; 32]>>>,
}

impl MemoryIdentityStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdentityStore for MemoryIdentityStore {
    fn load(&self) -> Result<Option<StaticKeypair>, IdentityError> {
        let guard = self.key.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(private) => StaticKeypair::from_private(Zeroizing::new(**private)).map(Some),
            None => Ok(None),
        }
    }

    fn store(&self, keypair: &StaticKeypair) -> Result<(), IdentityError> {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = Some(keypair.private_bytes());
        Ok(())
    }

    fn delete(&self) -> Result<(), IdentityError> {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

/// How [`load_or_create_pc_identity`] obtained the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityOutcome {
    /// The stored key matched the public key recorded in the trust store.
    Loaded,
    /// First run: a new identity was created.
    Created,
    /// The identity had to be replaced and **every paired phone was dropped**: tell the user that
    /// all phones must pair again. The reason distinguishes a lost credential from a foreign one.
    Regenerated(RegenerationReason),
}

/// Why a PC identity was replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegenerationReason {
    /// The trust store remembers an identity but the credential store has none (lost entry,
    /// reinstall).
    CredentialMissing,
    /// The credential store holds a key whose public half differs from the recorded one.
    PublicKeyMismatch,
    /// The credential store holds a key but the local trust store never recorded its public half.
    /// This is what a credential that roamed or was cloned from another machine looks like (the
    /// trust store is local; the credential might not be), so it is not trusted.
    NoRecordedPublicKey,
}

/// Load the PC's identity, creating it on first run.
///
/// The public half is kept in the (machine-local) trust store. That makes three failure modes
/// detectable: a lost credential, a replaced one, and a credential that appeared from elsewhere
/// (for example one that roamed with a Windows enterprise profile). In each case a fresh identity
/// is created and all peers are dropped, because phones paired to the old key cannot authenticate
/// to the new one anyway.
pub fn load_or_create_pc_identity(
    identity: &dyn IdentityStore,
    trust: &dyn TrustStore,
) -> Result<(StaticKeypair, IdentityOutcome), ServerError> {
    let stored = identity.load()?;
    let recorded = trust.own_public()?;
    let regenerate = |reason| -> Result<_, ServerError> {
        let fresh = StaticKeypair::generate()?;
        identity.store(&fresh)?;
        trust.clear_peers()?;
        trust.set_own_public(Some(*fresh.public()))?;
        Ok((fresh, IdentityOutcome::Regenerated(reason)))
    };
    match (stored, recorded) {
        (None, None) => {
            let fresh = StaticKeypair::generate()?;
            identity.store(&fresh)?;
            trust.set_own_public(Some(*fresh.public()))?;
            Ok((fresh, IdentityOutcome::Created))
        }
        (Some(pair), Some(public)) if *pair.public() == public => {
            Ok((pair, IdentityOutcome::Loaded))
        }
        (Some(_), Some(_)) => regenerate(RegenerationReason::PublicKeyMismatch),
        (Some(_), None) => regenerate(RegenerationReason::NoRecordedPublicKey),
        (None, Some(_)) => regenerate(RegenerationReason::CredentialMissing),
    }
}

/// "Reset pairing identity": make a new PC identity and drop every peer, so all phones must pair
/// again. The old private key is deleted from the credential store first.
pub fn reset_pc_identity(
    identity: &dyn IdentityStore,
    trust: &dyn TrustStore,
) -> Result<StaticKeypair, ServerError> {
    identity.delete()?;
    trust.clear_peers()?;
    trust.set_own_public(None)?;
    let fresh = StaticKeypair::generate()?;
    identity.store(&fresh)?;
    trust.set_own_public(Some(*fresh.public()))?;
    Ok(fresh)
}

#[cfg(feature = "keyring-store")]
pub use keyring_backed::KeyringIdentityStore;

#[cfg(feature = "keyring-store")]
mod keyring_backed {
    use std::collections::HashMap;
    use std::sync::Arc;

    use keyring_core::api::CredentialStore;
    use keyring_core::Error as KeyringError;
    use zeroize::Zeroizing;

    use super::{IdentityError, IdentityStore};
    use crate::codec::{b64_decode_fixed, b64_encode};
    use crate::keys::StaticKeypair;

    /// An [`IdentityStore`] on the operating system's credential store, through `keyring-core`.
    ///
    /// The private key is stored as base64url text under service `cooldown`, user
    /// `noise-static-v1`.
    ///
    /// **Windows persistence.** The Windows store (`windows-native-keyring-store`) creates
    /// credentials with `CRED_PERSIST_ENTERPRISE` unless the entry is built with the
    /// `persistence` modifier, and Microsoft documents that level as visible to this user's logon
    /// sessions *on other computers* (roaming profiles). [`KeyringIdentityStore::native`] therefore
    /// builds every entry with `persistence=local` (`CRED_PERSIST_LOCAL_MACHINE`), and
    /// [`load`](IdentityStore::load) rewrites a credential found with any other level. The
    /// `keyring` v1 wrapper crate cannot pass that modifier, which is why this type does not use
    /// it. See the README for what was and was not verified.
    pub struct KeyringIdentityStore {
        store: Arc<CredentialStore>,
        service: String,
        user: String,
        modifiers: Vec<(String, String)>,
    }

    impl KeyringIdentityStore {
        /// The default service name.
        pub const SERVICE: &'static str = "cooldown";
        /// The default user name.
        pub const USER: &'static str = "noise-static-v1";

        /// The platform credential store with this crate's settings (Windows: persistence
        /// `local`). Touches the real credential store on use, so tests must not call it.
        pub fn native() -> Result<Self, IdentityError> {
            #[cfg(target_os = "windows")]
            {
                let store = windows_native_keyring_store::Store::new().map_err(backend)?;
                Ok(Self::with_store(
                    store,
                    Self::SERVICE,
                    Self::USER,
                    &[("persistence", "local")],
                ))
            }
            #[cfg(target_os = "macos")]
            {
                let store = apple_native_keyring_store::keychain::Store::new().map_err(backend)?;
                Ok(Self::with_store(store, Self::SERVICE, Self::USER, &[]))
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                Err(IdentityError::Unsupported)
            }
        }

        /// Use any `keyring-core` store (tests pass a mock). `modifiers` are handed to the store
        /// when each entry is built.
        pub fn with_store(
            store: Arc<CredentialStore>,
            service: &str,
            user: &str,
            modifiers: &[(&str, &str)],
        ) -> Self {
            KeyringIdentityStore {
                store,
                service: service.to_owned(),
                user: user.to_owned(),
                modifiers: modifiers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            }
        }

        fn entry(&self) -> Result<keyring_core::Entry, IdentityError> {
            let modifiers: HashMap<&str, &str> = self
                .modifiers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let modifiers = (!modifiers.is_empty()).then_some(&modifiers);
            self.store
                .build(&self.service, &self.user, modifiers)
                .map_err(backend)
        }

        fn desired_persistence(&self) -> Option<&str> {
            self.modifiers
                .iter()
                .find(|(k, _)| k == "persistence")
                .map(|(_, v)| v.as_str())
        }
    }

    fn backend(error: KeyringError) -> IdentityError {
        IdentityError::Backend(error.to_string())
    }

    impl IdentityStore for KeyringIdentityStore {
        fn load(&self) -> Result<Option<StaticKeypair>, IdentityError> {
            let entry = self.entry()?;
            let text = match entry.get_password() {
                Ok(text) => Zeroizing::new(text),
                Err(KeyringError::NoEntry) => return Ok(None),
                Err(KeyringError::BadEncoding(mut bytes)) => {
                    zeroize::Zeroize::zeroize(&mut bytes);
                    return Err(IdentityError::Malformed);
                }
                Err(other) => return Err(backend(other)),
            };
            let private = b64_decode_fixed::<32>(text.trim())
                .map(Zeroizing::new)
                .ok_or(IdentityError::Malformed)?;
            let pair = StaticKeypair::from_private(private)?;
            // A credential written at a different persistence level (for example by an older
            // build or another tool) is rewritten at the level we want.
            if let Some(want) = self.desired_persistence() {
                if let Ok(attributes) = entry.get_attributes() {
                    let has = attributes.get("persistence").map(String::as_str);
                    if has.is_some_and(|has| !has.eq_ignore_ascii_case(want)) {
                        self.store(&pair)?;
                    }
                }
            }
            Ok(Some(pair))
        }

        fn store(&self, keypair: &StaticKeypair) -> Result<(), IdentityError> {
            let encoded = Zeroizing::new(b64_encode(&*keypair.private_bytes()));
            self.entry()?.set_password(&encoded).map_err(backend)
        }

        fn delete(&self) -> Result<(), IdentityError> {
            match self.entry()?.delete_credential() {
                Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
                Err(other) => Err(backend(other)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{MemoryTrustStore, PeerRecord};

    fn pc_setup() -> (MemoryIdentityStore, MemoryTrustStore) {
        (MemoryIdentityStore::new(), MemoryTrustStore::new())
    }

    fn add_peer(trust: &MemoryTrustStore) {
        let phone = StaticKeypair::generate().unwrap();
        trust
            .upsert(PeerRecord::new(
                *phone.public(),
                "Pixel".into(),
                "android".into(),
                0,
                0,
                1,
            ))
            .unwrap();
        trust.set_beacon_key(Some([1; 32])).unwrap();
    }

    #[test]
    fn first_run_creates_then_reloads_the_same_identity() {
        let (identity, trust) = pc_setup();
        let (first, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(outcome, IdentityOutcome::Created);
        assert_eq!(trust.own_public().unwrap(), Some(*first.public()));
        add_peer(&trust);
        let (second, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(outcome, IdentityOutcome::Loaded);
        assert_eq!(first.public(), second.public());
        assert_eq!(trust.list().unwrap().len(), 1);
    }

    #[test]
    fn a_lost_credential_regenerates_and_drops_peers() {
        let (identity, trust) = pc_setup();
        let (first, _) = load_or_create_pc_identity(&identity, &trust).unwrap();
        add_peer(&trust);
        identity.delete().unwrap();
        let (second, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(
            outcome,
            IdentityOutcome::Regenerated(RegenerationReason::CredentialMissing)
        );
        assert_ne!(first.public(), second.public());
        assert!(trust.list().unwrap().is_empty());
        assert_eq!(trust.own_public().unwrap(), Some(*second.public()));
    }

    #[test]
    fn a_replaced_credential_is_not_trusted() {
        let (identity, trust) = pc_setup();
        load_or_create_pc_identity(&identity, &trust).unwrap();
        add_peer(&trust);
        identity.store(&StaticKeypair::generate().unwrap()).unwrap();
        let (_, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(
            outcome,
            IdentityOutcome::Regenerated(RegenerationReason::PublicKeyMismatch)
        );
        assert!(trust.list().unwrap().is_empty());
    }

    #[test]
    fn a_credential_with_no_local_record_is_treated_as_foreign() {
        // What a roamed or cloned credential looks like on a machine whose trust store is new.
        let (identity, trust) = pc_setup();
        let foreign = StaticKeypair::generate().unwrap();
        identity.store(&foreign).unwrap();
        let (pair, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(
            outcome,
            IdentityOutcome::Regenerated(RegenerationReason::NoRecordedPublicKey)
        );
        assert_ne!(pair.public(), foreign.public());
    }

    #[test]
    fn reset_replaces_the_identity_and_drops_every_peer() {
        let (identity, trust) = pc_setup();
        let (first, _) = load_or_create_pc_identity(&identity, &trust).unwrap();
        add_peer(&trust);
        let second = reset_pc_identity(&identity, &trust).unwrap();
        assert_ne!(first.public(), second.public());
        assert!(trust.list().unwrap().is_empty());
        assert_eq!(trust.beacon_key().unwrap(), None);
        let (loaded, outcome) = load_or_create_pc_identity(&identity, &trust).unwrap();
        assert_eq!(outcome, IdentityOutcome::Loaded);
        assert_eq!(loaded.public(), second.public());
    }

    #[cfg(feature = "keyring-store")]
    mod keyring_backed {
        use std::any::Any;
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use keyring_core::api::{CredentialApi, CredentialStore, CredentialStoreApi};
        use keyring_core::{Credential, Entry, Error, Result};

        use super::*;
        use crate::identity::KeyringIdentityStore;

        /// A stand-in for the Windows store: credentials default to `Enterprise` persistence,
        /// the `persistence` modifier on an entry selects the level applied when it writes, and
        /// the level is readable as an attribute. It mirrors the behaviour read from
        /// `windows-native-keyring-store` 1.1.0.
        type Blob = Arc<Mutex<Option<(Vec<u8>, String)>>>;

        #[derive(Default)]
        struct FakeWindowsStore {
            blob: Blob,
        }

        struct FakeCred {
            blob: Blob,
            persistence: String,
        }

        impl std::fmt::Debug for FakeCred {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("FakeCred")
            }
        }

        impl CredentialApi for FakeCred {
            fn set_secret(&self, secret: &[u8]) -> Result<()> {
                *self.blob.lock().unwrap() = Some((secret.to_vec(), self.persistence.clone()));
                Ok(())
            }
            fn get_secret(&self) -> Result<Vec<u8>> {
                match &*self.blob.lock().unwrap() {
                    Some((secret, _)) => Ok(secret.clone()),
                    None => Err(Error::NoEntry),
                }
            }
            fn get_attributes(&self) -> Result<HashMap<String, String>> {
                match &*self.blob.lock().unwrap() {
                    Some((_, level)) => Ok(HashMap::from([("persistence".into(), level.clone())])),
                    None => Err(Error::NoEntry),
                }
            }
            fn delete_credential(&self) -> Result<()> {
                match self.blob.lock().unwrap().take() {
                    Some(_) => Ok(()),
                    None => Err(Error::NoEntry),
                }
            }
            fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
                Ok(None)
            }
            fn get_specifiers(&self) -> Option<(String, String)> {
                None
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        impl CredentialStoreApi for FakeWindowsStore {
            fn vendor(&self) -> String {
                "fake windows".into()
            }
            fn id(&self) -> String {
                "fake".into()
            }
            fn build(
                &self,
                _service: &str,
                _user: &str,
                modifiers: Option<&HashMap<&str, &str>>,
            ) -> Result<Entry> {
                let persistence = modifiers.and_then(|m| m.get("persistence")).map_or(
                    "Enterprise".to_owned(),
                    |level| {
                        let mut level = level.to_lowercase();
                        level[..1].make_ascii_uppercase();
                        level
                    },
                );
                Ok(Entry::new_with_credential(Arc::new(FakeCred {
                    blob: self.blob.clone(),
                    persistence,
                })))
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
        }

        fn level(store: &FakeWindowsStore) -> String {
            store.blob.lock().unwrap().as_ref().unwrap().1.clone()
        }

        #[test]
        fn mock_store_round_trip() {
            let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
            let ids = KeyringIdentityStore::with_store(store, "svc", "usr", &[]);
            assert!(ids.load().unwrap().is_none());
            let pair = StaticKeypair::generate().unwrap();
            ids.store(&pair).unwrap();
            assert_eq!(ids.load().unwrap().unwrap().public(), pair.public());
            ids.delete().unwrap();
            assert!(ids.load().unwrap().is_none());
            ids.delete().unwrap();
        }

        #[test]
        fn malformed_stored_text_is_an_error_not_a_new_identity() {
            let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
            store
                .build("svc", "usr", None)
                .unwrap()
                .set_password("not a key")
                .unwrap();
            let ids = KeyringIdentityStore::with_store(store, "svc", "usr", &[]);
            assert!(matches!(ids.load(), Err(IdentityError::Malformed)));
        }

        #[test]
        fn local_persistence_is_applied_on_every_write() {
            let fake = Arc::new(FakeWindowsStore::default());
            let ids = KeyringIdentityStore::with_store(
                fake.clone(),
                "cooldown",
                "noise-static-v1",
                &[("persistence", "local")],
            );
            ids.store(&StaticKeypair::generate().unwrap()).unwrap();
            assert_eq!(level(&fake), "Local");
        }

        #[test]
        fn an_enterprise_credential_is_rewritten_as_local_on_load() {
            let fake = Arc::new(FakeWindowsStore::default());
            // An older build (or another tool) wrote it with the store's default level.
            let pair = StaticKeypair::generate().unwrap();
            KeyringIdentityStore::with_store(fake.clone(), "cooldown", "noise-static-v1", &[])
                .store(&pair)
                .unwrap();
            assert_eq!(level(&fake), "Enterprise");
            let ids = KeyringIdentityStore::with_store(
                fake.clone(),
                "cooldown",
                "noise-static-v1",
                &[("persistence", "local")],
            );
            let loaded = ids.load().unwrap().unwrap();
            assert_eq!(loaded.public(), pair.public());
            assert_eq!(level(&fake), "Local");
        }
    }
}
