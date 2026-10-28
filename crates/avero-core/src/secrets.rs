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
///
/// The Windows credential manager caps a secret at 2560 bytes (1280 UTF-16
/// units) and OAuth refresh tokens can be longer, so long secrets are split:
/// the main entry `account-N` then holds a header `chunks:K` and the parts
/// live in `account-N-part-1` .. `account-N-part-K`.
pub struct KeyringSecrets;

/// Longest part stored in a single keyring entry, in UTF-16 units (the
/// Windows limit is 1280).
const CHUNK_UNITS: usize = 1000;
const CHUNK_HEADER: &str = "avero-chunks:";

/// What the main entry holds, and the parts to store next to it.
fn split_secret(secret: &str) -> (String, Vec<String>) {
    if secret.encode_utf16().count() <= CHUNK_UNITS && !secret.starts_with(CHUNK_HEADER) {
        return (secret.to_string(), Vec::new());
    }
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for c in secret.chars() {
        if units + c.len_utf16() > CHUNK_UNITS {
            parts.push(std::mem::take(&mut current));
            units = 0;
        }
        units += c.len_utf16();
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    (format!("{CHUNK_HEADER}{}", parts.len()), parts)
}

/// Number of parts announced by a main entry, `None` for a plain secret.
fn chunk_count(head: &str) -> Option<usize> {
    head.strip_prefix(CHUNK_HEADER)?.parse().ok()
}

/// Reassembles parts read back in order.
fn join_parts(parts: &[String]) -> String {
    parts.concat()
}

impl KeyringSecrets {
    fn entry(account: AccountId) -> Result<keyring::Entry, SecretError> {
        Ok(keyring::Entry::new(
            "avero",
            &format!("account-{}", account.0),
        )?)
    }

    fn part(account: AccountId, index: usize) -> Result<keyring::Entry, SecretError> {
        Ok(keyring::Entry::new(
            "avero",
            &format!("account-{}-part-{index}", account.0),
        )?)
    }

    fn read(entry: &keyring::Entry) -> Result<Option<String>, SecretError> {
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn remove(entry: &keyring::Entry) -> Result<(), SecretError> {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes parts from `from` upward until one is missing.
    fn remove_parts(account: AccountId, from: usize) -> Result<(), SecretError> {
        let mut index = from;
        loop {
            let entry = Self::part(account, index)?;
            if Self::read(&entry)?.is_none() {
                return Ok(());
            }
            Self::remove(&entry)?;
            index += 1;
        }
    }
}

impl SecretStore for KeyringSecrets {
    fn get(&self, account: AccountId) -> Result<Option<String>, SecretError> {
        let Some(head) = Self::read(&Self::entry(account)?)? else {
            return Ok(None);
        };
        let Some(count) = chunk_count(&head) else {
            return Ok(Some(head));
        };
        let mut parts = Vec::with_capacity(count);
        for index in 1..=count {
            match Self::read(&Self::part(account, index)?)? {
                Some(part) => parts.push(part),
                // A half-written secret is as good as none.
                None => return Ok(None),
            }
        }
        Ok(Some(join_parts(&parts)))
    }

    fn set(&self, account: AccountId, secret: &str) -> Result<(), SecretError> {
        let (head, parts) = split_secret(secret);
        for (i, part) in parts.iter().enumerate() {
            Self::part(account, i + 1)?.set_password(part)?;
        }
        Self::entry(account)?.set_password(&head)?;
        // Leftovers of a longer previous secret.
        Self::remove_parts(account, parts.len() + 1)
    }

    fn delete(&self, account: AccountId) -> Result<(), SecretError> {
        Self::remove(&Self::entry(account)?)?;
        Self::remove_parts(account, 1)
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

    #[test]
    fn short_secret_is_stored_as_is() {
        let (head, parts) = split_secret("s3cret");
        assert_eq!(head, "s3cret");
        assert!(parts.is_empty());
        assert_eq!(chunk_count(&head), None);
    }

    #[test]
    fn long_secret_is_split_and_joined() {
        let secret: String = (0..5000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let (head, parts) = split_secret(&secret);
        assert_eq!(chunk_count(&head), Some(5));
        assert_eq!(parts.len(), 5);
        assert!(
            parts
                .iter()
                .all(|p| p.encode_utf16().count() <= CHUNK_UNITS)
        );
        assert_eq!(join_parts(&parts), secret);
    }

    #[test]
    fn split_counts_utf16_units() {
        let secret = "é".repeat(1500);
        let (head, parts) = split_secret(&secret);
        assert_eq!(chunk_count(&head), Some(2));
        assert_eq!(parts[0].chars().count(), 1000);
        assert_eq!(join_parts(&parts), secret);
        let wide = "😀".repeat(600);
        let (_, parts) = split_secret(&wide);
        assert_eq!(parts.len(), 2);
        assert!(
            parts
                .iter()
                .all(|p| p.encode_utf16().count() <= CHUNK_UNITS)
        );
        assert_eq!(join_parts(&parts), wide);
    }

    #[test]
    fn secret_looking_like_a_header_is_chunked() {
        let (head, parts) = split_secret("avero-chunks:3");
        assert_eq!(chunk_count(&head), Some(1));
        assert_eq!(join_parts(&parts), "avero-chunks:3");
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

        let long: String = (0..5000)
            .map(|i| char::from(b'A' + (i % 26) as u8))
            .collect();
        store.set(id, &long).unwrap();
        assert_eq!(store.get(id).unwrap().as_deref(), Some(long.as_str()));
        // Overwriting with a short secret removes the parts.
        store.set(id, "short").unwrap();
        assert_eq!(store.get(id).unwrap().as_deref(), Some("short"));
        assert!(
            KeyringSecrets::read(&KeyringSecrets::part(id, 1).unwrap())
                .unwrap()
                .is_none()
        );
        store.set(id, &long).unwrap();
        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
        assert!(
            KeyringSecrets::read(&KeyringSecrets::part(id, 1).unwrap())
                .unwrap()
                .is_none()
        );
    }
}
