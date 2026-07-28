//! The default [`SecretProvider`], backed by the OS keychain.
//!
//! Secrets (model API keys, connection tokens) live in the platform credential
//! store — Keychain on macOS, Credential Manager on Windows, the Secret Service
//! on Linux — keyed by a stable reference string, and never touch the `Store`.
//! Enabled by the `keychain` feature.
//!
//! The blocking `keyring` calls run on a blocking thread (`spawn_blocking`) so
//! they don't stall the async runtime.

use std::sync::Once;

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

static MOCK_INIT: Once = Once::new();

impl KeychainSecretProvider {
    /// Use the default service name (`openwave`).
    ///
    /// If [`use_mock`](Self::use_mock) was called earlier in the process, all
    /// operations go to an in-memory store instead of the OS keychain. The
    /// `OPENWAVE_KEYCHAIN_MOCK` env var (any non-empty value) has the same
    /// effect — useful for subprocess-based integration tests where you can't
    /// call `use_mock` before `main`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_service(DEFAULT_SERVICE)
    }

    /// Use a custom service name (useful to isolate profiles or tests).
    /// Honors [`use_mock`](Self::new) and `OPENWAVE_KEYCHAIN_MOCK` the same
    /// way [`new`](Self::new) does.
    pub fn with_service(service: impl Into<String>) -> Self {
        if std::env::var("OPENWAVE_KEYCHAIN_MOCK").is_ok_and(|v| !v.is_empty()) {
            Self::use_mock();
        }
        Self {
            service: service.into(),
        }
    }

    /// Route all keyring operations to an in-memory mock. Call this before any
    /// `KeychainSecretProvider` is used. Safe to call from multiple threads;
    /// only the first call has effect.
    pub fn use_mock() {
        MOCK_INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
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

    #[tokio::test]
    async fn operations_succeed_and_missing_reads_as_none() {
        KeychainSecretProvider::use_mock();
        let secrets = KeychainSecretProvider::with_service("openwave-test");
        let key = "provider.anthropic.api_key";

        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        secrets.set_secret(key, "sk-123").await.unwrap();
        // Deleting is a no-op when nothing is persisted (mock is per-entry).
        secrets.delete_secret(key).await.unwrap();
    }
}
