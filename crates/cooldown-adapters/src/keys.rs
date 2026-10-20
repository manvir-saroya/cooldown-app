//! Loading an API key for a poll, and the notes that explain why there is none.

use std::sync::Arc;

use crate::secrets::{SecretError, SecretStore, SecretString};

/// Why a poll has no key to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyProblem {
    /// Nothing is saved under the secret's name.
    Missing,
    /// The saved value cannot be an HTTP bearer token.
    Invalid,
    /// The credential store could not be read.
    StoreUnavailable,
}

impl KeyProblem {
    /// A note for the UI. Never contains the key.
    pub(crate) fn reason(self, what: &str) -> String {
        match self {
            KeyProblem::Missing => format!("No {what} saved. Add one in Settings"),
            KeyProblem::Invalid => format!("The saved {what} is not valid. Replace it in Settings"),
            KeyProblem::StoreUnavailable => {
                format!("Could not read the {what} from the system credential store")
            }
        }
    }
}

/// Read the secret called `name` from the store (on a blocking thread: credential stores may talk
/// to the OS) and check it is usable as a bearer token.
pub(crate) async fn load_key(
    store: &Arc<dyn SecretStore>,
    name: &str,
) -> Result<SecretString, KeyProblem> {
    let store = Arc::clone(store);
    let name = name.to_owned();
    let loaded = tokio::task::spawn_blocking(move || store.get(&name))
        .await
        .map_err(|_| KeyProblem::StoreUnavailable)?;
    match loaded {
        Ok(Some(key)) if key.is_valid_api_key() => Ok(key),
        Ok(Some(_)) => Err(KeyProblem::Invalid),
        Ok(None) => Err(KeyProblem::Missing),
        Err(SecretError::Invalid) => Err(KeyProblem::Invalid),
        Err(SecretError::Unavailable(_)) => Err(KeyProblem::StoreUnavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::InMemorySecretStore;

    #[tokio::test]
    async fn keys_are_checked_before_use() {
        let store: Arc<dyn SecretStore> = Arc::new(InMemorySecretStore::new());
        assert_eq!(load_key(&store, "a:b").await, Err(KeyProblem::Missing));
        store.set("a:b", &SecretString::new("two words")).unwrap();
        assert_eq!(load_key(&store, "a:b").await, Err(KeyProblem::Invalid));
        store.set("a:b", &SecretString::new("sk-good")).unwrap();
        assert_eq!(load_key(&store, "a:b").await.unwrap().expose(), "sk-good");
        assert_eq!(load_key(&store, "").await, Err(KeyProblem::Invalid));
    }

    #[test]
    fn reasons_never_mention_a_key_value() {
        for problem in [
            KeyProblem::Missing,
            KeyProblem::Invalid,
            KeyProblem::StoreUnavailable,
        ] {
            let text = problem.reason("OpenRouter API key");
            assert!(text.contains("OpenRouter API key"));
        }
    }
}
