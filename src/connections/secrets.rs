//! Credential storage (design.md §20.2). Secrets are keyed by connection id.

use std::collections::HashMap;
use std::sync::Mutex;

use super::CredentialRef;
use crate::core::Secret;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("credential store: {0}")]
    Store(String),
    #[error("environment variable {0} is not set")]
    EnvNotSet(String),
    #[error("no stored credential for this connection")]
    Missing,
}

pub trait SecretStore: Send + Sync {
    fn set(&self, connection_id: &str, secret: &Secret) -> Result<(), SecretError>;
    fn get(&self, connection_id: &str) -> Result<Option<Secret>, SecretError>;
    /// Deleting a secret that does not exist succeeds.
    fn delete(&self, connection_id: &str) -> Result<(), SecretError>;
}

/// The OS credential store: Keychain on macOS, Credential Manager on
/// Windows, Secret Service on Linux.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyringStore;

impl KeyringStore {
    const SERVICE: &'static str = "paperwire";

    fn entry(connection_id: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(Self::SERVICE, connection_id).map_err(store_error)
    }
}

// keyring errors describe the store, not the secret, so they are safe to show.
fn store_error(e: keyring::Error) -> SecretError {
    SecretError::Store(e.to_string())
}

impl SecretStore for KeyringStore {
    fn set(&self, connection_id: &str, secret: &Secret) -> Result<(), SecretError> {
        Self::entry(connection_id)?
            .set_password(secret.expose())
            .map_err(store_error)
    }

    fn get(&self, connection_id: &str) -> Result<Option<Secret>, SecretError> {
        match Self::entry(connection_id)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(store_error(e)),
        }
    }

    fn delete(&self, connection_id: &str) -> Result<(), SecretError> {
        match Self::entry(connection_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(store_error(e)),
        }
    }
}

/// An in-process store, for tests.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<String, Secret>>);

impl SecretStore for MemoryStore {
    fn set(&self, connection_id: &str, secret: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(connection_id.to_owned(), secret.clone());
        Ok(())
    }

    fn get(&self, connection_id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.0.lock().unwrap().get(connection_id).cloned())
    }

    fn delete(&self, connection_id: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(connection_id);
        Ok(())
    }
}

/// The credential a connection refers to, or `None` if it needs none.
pub fn resolve(
    credential: &CredentialRef,
    connection_id: &str,
    store: &dyn SecretStore,
) -> Result<Option<Secret>, SecretError> {
    match credential {
        CredentialRef::None => Ok(None),
        CredentialRef::Keyring => store
            .get(connection_id)?
            .map(Some)
            .ok_or(SecretError::Missing),
        CredentialRef::Env(name) => match std::env::var(name) {
            Ok(value) if !value.is_empty() => Ok(Some(Secret::new(value))),
            _ => Err(SecretError::EnvNotSet(name.clone())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_follows_the_reference() {
        let store = MemoryStore::default();
        assert_eq!(resolve(&CredentialRef::None, "c", &store).unwrap(), None);
        assert!(matches!(
            resolve(&CredentialRef::Keyring, "c", &store),
            Err(SecretError::Missing)
        ));
        store.set("c", &Secret::new("k")).unwrap();
        let secret = resolve(&CredentialRef::Keyring, "c", &store).unwrap();
        assert_eq!(secret.unwrap().expose(), "k");
        assert!(matches!(
            resolve(
                &CredentialRef::Env("PAPERWIRE_TEST_UNSET_VAR".into()),
                "c",
                &store
            ),
            Err(SecretError::EnvNotSet(_))
        ));
    }

    /// Touches the real OS credential store, so it only runs on request:
    /// `cargo test -- --ignored keyring_round_trip`.
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let id = "conn_test_keyring_round_trip";
        let store = KeyringStore;
        store.set(id, &Secret::new("not-a-real-key")).unwrap();
        assert_eq!(store.get(id).unwrap().unwrap().expose(), "not-a-real-key");
        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
        store.delete(id).unwrap();
    }
}
