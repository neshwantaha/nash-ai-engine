//! Secret storage.
//!
//! Credentials are addressed by an opaque *name* (the `secret_ref` in a
//! provider's configuration). Two backends are provided:
//!
//! * [`MemorySecretStore`] — process-local, for tests and ephemeral runs.
//! * [`KeyringSecretStore`] — the OS credential store (Windows Credential
//!   Manager, macOS Keychain, Secret Service on Linux).
//!
//! A keyring backend keeps its own name index in a well-known entry, because
//! the platform APIs do not offer enumeration.

use nash_core::error::{EngineError, Result};
use nash_core::secrets::SecretResolver;
use parking_lot::RwLock;
use std::collections::BTreeMap;

/// A secret resolver that can also *write* credentials.
pub trait SecretStore: SecretResolver {
    /// Store or replace a secret value.
    fn store(&self, name: &str, value: &str) -> Result<()>;
    /// Remove a secret. Returns whether it existed.
    fn remove(&self, name: &str) -> Result<bool>;
    /// The names currently held. Values are never returned.
    fn names(&self) -> Result<Vec<String>>;
}

/// An in-memory secret store. Not persisted; use for tests.
#[derive(Default)]
pub struct MemorySecretStore {
    map: RwLock<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretResolver for MemorySecretStore {
    fn resolve(&self, name: &str) -> Result<Option<String>> {
        Ok(self.map.read().get(name).cloned().filter(|s| !s.is_empty()))
    }
}

impl SecretStore for MemorySecretStore {
    fn store(&self, name: &str, value: &str) -> Result<()> {
        if name.trim().is_empty() {
            return Err(EngineError::Config("secret name must not be empty".into()));
        }
        self.map.write().insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<bool> {
        Ok(self.map.write().remove(name).is_some())
    }

    fn names(&self) -> Result<Vec<String>> {
        Ok(self.map.read().keys().cloned().collect())
    }
}

#[cfg(feature = "os-keyring")]
mod keyring_backend {
    use super::*;
    use keyring::Entry;

    /// The entry under which the index of stored secret names is kept.
    const INDEX_NAME: &str = "__nash_index__";

    /// OS-keyring backed secret store.
    pub struct KeyringSecretStore {
        service: String,
    }

    impl KeyringSecretStore {
        /// `service` groups all of this app's credentials, e.g. `"nash-ai-engine"`.
        pub fn new(service: impl Into<String>) -> Self {
            Self {
                service: service.into(),
            }
        }

        fn entry(&self, name: &str) -> Result<Entry> {
            Entry::new(&self.service, name)
                .map_err(|e| EngineError::Other(format!("keyring unavailable: {e}")))
        }

        fn read_index(&self) -> Result<Vec<String>> {
            match self.entry(INDEX_NAME)?.get_password() {
                Ok(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
                Err(keyring::Error::NoEntry) => Ok(Vec::new()),
                Err(e) => Err(EngineError::Other(format!("keyring error: {e}"))),
            }
        }

        fn write_index(&self, names: &[String]) -> Result<()> {
            let text = serde_json::to_string(names)?;
            self.entry(INDEX_NAME)?
                .set_password(&text)
                .map_err(|e| EngineError::Other(format!("keyring error: {e}")))
        }
    }

    impl SecretResolver for KeyringSecretStore {
        fn resolve(&self, name: &str) -> Result<Option<String>> {
            match self.entry(name)?.get_password() {
                Ok(v) => Ok(Some(v).filter(|s| !s.is_empty())),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(EngineError::Other(format!("keyring error: {e}"))),
            }
        }
    }

    impl SecretStore for KeyringSecretStore {
        fn store(&self, name: &str, value: &str) -> Result<()> {
            if name.trim().is_empty() || name == INDEX_NAME {
                return Err(EngineError::Config("invalid secret name".into()));
            }
            self.entry(name)?
                .set_password(value)
                .map_err(|e| EngineError::Other(format!("keyring error: {e}")))?;
            let mut names = self.read_index()?;
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
                names.sort();
                self.write_index(&names)?;
            }
            Ok(())
        }

        fn remove(&self, name: &str) -> Result<bool> {
            match self.entry(name)?.delete_credential() {
                Ok(()) => {
                    let mut names = self.read_index()?;
                    names.retain(|n| n != name);
                    self.write_index(&names)?;
                    Ok(true)
                }
                Err(keyring::Error::NoEntry) => Ok(false),
                Err(e) => Err(EngineError::Other(format!("keyring error: {e}"))),
            }
        }

        fn names(&self) -> Result<Vec<String>> {
            self.read_index()
        }
    }
}

#[cfg(feature = "os-keyring")]
pub use keyring_backend::KeyringSecretStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrips() {
        let s = MemorySecretStore::new();
        assert_eq!(s.resolve("k").unwrap(), None);
        s.store("k", "v").unwrap();
        assert_eq!(s.resolve("k").unwrap().as_deref(), Some("v"));
        assert_eq!(s.names().unwrap(), vec!["k".to_string()]);
        assert!(s.remove("k").unwrap());
        assert!(!s.remove("k").unwrap());
        assert_eq!(s.resolve("k").unwrap(), None);
    }

    #[test]
    fn empty_names_rejected() {
        let s = MemorySecretStore::new();
        assert!(s.store("  ", "v").is_err());
    }

    #[test]
    fn empty_values_resolve_to_none() {
        let s = MemorySecretStore::new();
        s.store("k", "").unwrap();
        assert_eq!(s.resolve("k").unwrap(), None);
    }
}
