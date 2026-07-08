//! The default [`SecretProvider`], backed by the OS keychain.
//!
//! Secrets (model API keys, connection tokens) live in the platform credential
//! store — Keychain on macOS, Credential Manager on Windows, the Secret Service
//! on Linux — keyed by a stable reference string, and never touch the `Store`.
//! Enabled by the `keychain` feature.
//!
//! The blocking `keyring` calls run on a blocking thread (`spawn_blocking`) so
//! they don't stall the async runtime.

use async_trait::async_trait;

use crate::error::{AgentError, Result};
use crate::storage::SecretProvider;

/// Default keychain service name; entries are stored under (service, key).
const DEFAULT_SERVICE: &str = "openwave";

fn secret_err(err: impl std::fmt::Display) -> AgentError {
    AgentError::Secret(err.to_string())
}

/// A [`SecretProvider`] backed by the OS keychain.
#[derive(Clone, Debug)]
pub struct KeychainSecretProvider {
    service: String,
}

impl KeychainSecretProvider {
    /// Use the default service name (`openwave`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            service: DEFAULT_SERVICE.to_string(),
        }
    }

    /// Use a custom service name (useful to isolate profiles or tests).
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }
}

impl Default for KeychainSecretProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SecretProvider for KeychainSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        let (service, key) = (self.service.clone(), key.to_string());
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key).map_err(secret_err)?;
            match entry.get_password() {
                Ok(password) => Ok(Some(password)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(err) => Err(secret_err(err)),
            }
        })
        .await
        .map_err(secret_err)?
    }

    async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
        let (service, key, value) = (self.service.clone(), key.to_string(), value.to_string());
        tokio::task::spawn_blocking(move || {
            keyring::Entry::new(&service, &key)
                .map_err(secret_err)?
                .set_password(&value)
                .map_err(secret_err)
        })
        .await
        .map_err(secret_err)?
    }

    async fn delete_secret(&self, key: &str) -> Result<()> {
        let (service, key) = (self.service.clone(), key.to_string());
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key).map_err(secret_err)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(secret_err(err)),
            }
        })
        .await
        .map_err(secret_err)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    // Route all keyring access to the in-memory mock store so tests need no real
    // OS keychain (and pass on headless CI).
    static MOCK: Once = Once::new();
    fn use_mock() {
        MOCK.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    // A smoke test only: keyring's mock credential is per-`Entry`, not a shared
    // store, so it can't verify set-then-get *persistence* (that needs a real OS
    // keychain and is validated manually / on a real desktop). This checks the
    // three operations are wired correctly and that a missing secret reads as
    // `None` and a missing delete is a no-op.
    #[tokio::test]
    async fn operations_succeed_and_missing_reads_as_none() {
        use_mock();
        let secrets = KeychainSecretProvider::with_service("openwave-test");
        let key = "provider.anthropic.api_key";

        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        secrets.set_secret(key, "sk-123").await.unwrap();
        // Deleting is a no-op when nothing is persisted (mock is per-entry).
        secrets.delete_secret(key).await.unwrap();
    }
}
