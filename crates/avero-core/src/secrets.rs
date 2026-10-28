//! Password storage. Secrets live in the OS keyring, never on disk in clear.

use crate::model::AccountId;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(#[from] keyring::Error),
}

/// Where account passwords are kept.
pub trait SecretStore: Send + Sync {
    /// Returns `Ok(None)` when no secret is stored for this account.
    fn get(&self, account: AccountId) -> Result<Option<String>, SecretError>;
    fn set(&self, account: AccountId, secret: &str) -> Result<(), SecretError>;
    /// Deleting a missing secret is not an error.
    fn delete(&self, account: AccountId) -> Result<(), SecretError>;
}

/// Secrets stored in the operating system keyring (service "avero").
pub struct KeyringSecrets;

impl KeyringSecrets {
    fn entry(account: AccountId) -> Result<keyring::Entry, SecretError> {
        Ok(keyring::Entry::new(
            "avero",
            &format!("account-{}", account.0),
        )?)
    }
}

impl SecretStore for KeyringSecrets {
    fn get(&self, account: AccountId) -> Result<Option<String>, SecretError> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, account: AccountId, secret: &str) -> Result<(), SecretError> {
        Ok(Self::entry(account)?.set_password(secret)?)
    }

    fn delete(&self, account: AccountId) -> Result<(), SecretError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// In-memory secrets, for tests.
#[derive(Default)]
pub struct MemorySecrets {
    secrets: Mutex<HashMap<AccountId, String>>,
}

impl MemorySecrets {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecrets {
    fn get(&self, account: AccountId) -> Result<Option<String>, SecretError> {
        Ok(self.secrets.lock().unwrap().get(&account).cloned())
    }

    fn set(&self, account: AccountId, secret: &str) -> Result<(), SecretError> {
        self.secrets
            .lock()
            .unwrap()
            .insert(account, secret.to_string());
        Ok(())
    }

    fn delete(&self, account: AccountId) -> Result<(), SecretError> {
        self.secrets.lock().unwrap().remove(&account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_set_get_delete() {
        let store = MemorySecrets::new();
        let id = AccountId(1);
        assert_eq!(store.get(id).unwrap(), None);
        store.set(id, "hunter2").unwrap();
        assert_eq!(store.get(id).unwrap().as_deref(), Some("hunter2"));
        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
        store.delete(id).unwrap();
    }

    /// Touches the real OS keyring: run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let store = KeyringSecrets;
        let id = AccountId(i64::MAX - 1);
        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
        store.set(id, "s3cret").unwrap();
        assert_eq!(store.get(id).unwrap().as_deref(), Some("s3cret"));
        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
    }
}
