//! The default [`SecretProvider`], backed by the OS keychain.
//!
//! Secrets (model API keys, connection tokens) live in the platform credential
//! store — Keychain on macOS, Credential Manager on Windows, the Secret Service
//! on Linux — keyed by a stable reference string, and never touch the `Store`.
//! Enabled by the `keychain` feature.
//!
//! The blocking `keyring` calls run on a blocking thread (`spawn_blocking`) so
//! they don't stall the async runtime.

use std::collections::HashMap;
use std::sync::{Mutex, Once, OnceLock};

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

/// The mock's credential store, keyed by (service, key).
///
/// `keyring`'s own mock keeps a password on the entry object and has no store
/// behind it, so a value written through one `Entry` is invisible to the next
/// one — a mocked process could store a credential and never read it back.
/// Everything that writes a secret then uses it (a headless run configuring a
/// provider and then taking a turn with it) depends on the two agreeing, so
/// the mock keeps its own map.
fn mock_store() -> &'static Mutex<HashMap<(String, String), String>> {
    static STORE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    STORE.get_or_init(Mutex::default)
}

/// Whether this process routes secrets to [`mock_store`] instead of the OS.
fn mocked() -> bool {
    MOCK_INIT.is_completed()
}

impl KeychainSecretProvider {
    /// Use the default service name (`openwave`).
    ///
    /// If [`use_mock`](Self::use_mock) was called earlier in the process, all
    /// operations go to an in-memory store instead of the OS keychain. In
    /// **debug builds only**, the `OPENWAVE_KEYCHAIN_MOCK` env var (any
    /// non-empty value) has the same effect — useful for subprocess-based
    /// integration tests where you can't call `use_mock` before `main`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_service(DEFAULT_SERVICE)
    }

    /// Use a custom service name (useful to isolate profiles or tests).
    /// Honors [`use_mock`](Self::use_mock) and `OPENWAVE_KEYCHAIN_MOCK` the
    /// same way [`new`](Self::new) does.
    pub fn with_service(service: impl Into<String>) -> Self {
        // Debug-only on purpose. In a shipped build this env var would let
        // anything that can set the app's environment silently swap the OS
        // keychain for a process-local store: secrets written during the run
        // are lost when it exits, and a credential the user stored earlier
        // reads as absent. Only debug-binary tests consume it, so a release
        // binary has no reason to honor it and every reason not to.
        #[cfg(debug_assertions)]
        if std::env::var("OPENWAVE_KEYCHAIN_MOCK").is_ok_and(|v| !v.is_empty()) {
            Self::use_mock();
        }
        Self {
            service: service.into(),
        }
    }

    /// Route every secret operation to an in-memory store for the rest of the
    /// process. Call this before any `KeychainSecretProvider` is used. Safe to
    /// call from multiple threads; only the first call has effect.
    ///
    /// Values written this way behave like stored credentials — they read back
    /// under the same (service, key) — and are gone when the process exits.
    pub fn use_mock() {
        MOCK_INIT.call_once(|| {
            // Also point `keyring` itself away from the OS credential store,
            // so nothing that builds an `Entry` some other way can reach a
            // developer's real keychain from a mocked process.
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
        if mocked() {
            return Ok(mock_store().lock().unwrap().get(&(service, key)).cloned());
        }
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
        if mocked() {
            mock_store().lock().unwrap().insert((service, key), value);
            return Ok(());
        }
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
        if mocked() {
            mock_store().lock().unwrap().remove(&(service, key));
            return Ok(());
        }
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

    /// The mock has to behave like a credential store, not just accept writes:
    /// code that stores a secret and then uses it — a headless run configuring
    /// a provider before taking a turn — only works if a written value reads
    /// back.
    #[tokio::test]
    async fn the_mock_stores_what_it_is_given() {
        KeychainSecretProvider::use_mock();
        let secrets = KeychainSecretProvider::with_service("openwave-test");
        let key = "provider.anthropic.api_key";

        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        secrets.set_secret(key, "sk-123").await.unwrap();
        assert_eq!(
            secrets.get_secret(key).await.unwrap().as_deref(),
            Some("sk-123")
        );
        // A second service name is a separate namespace, as on the real thing.
        let other = KeychainSecretProvider::with_service("openwave-test-other");
        assert_eq!(other.get_secret(key).await.unwrap(), None);

        secrets.delete_secret(key).await.unwrap();
        assert_eq!(secrets.get_secret(key).await.unwrap(), None);
        // Deleting an absent entry is a no-op, not an error.
        secrets.delete_secret(key).await.unwrap();
    }

    /// Round-trips a secret through the real OS credential store — Credential
    /// Manager on Windows, Keychain on macOS — via the platform `keyring`
    /// backend, the boot-critical path every stored API key takes. Ignored by
    /// default so ordinary test runs never write to a developer's credential
    /// store; the Windows CI lane runs it explicitly with `-- --ignored`,
    /// which also keeps the process-global in-memory mock (set by the test
    /// above) out of this process.
    #[tokio::test]
    #[ignore = "writes to the real OS credential store; the Windows CI lane runs it explicitly"]
    async fn native_credential_store_round_trip() {
        assert!(
            !MOCK_INIT.is_completed(),
            "the in-memory keyring mock is active in this process; \
             run this test alone (`-- --ignored`) so it reaches the real backend"
        );
        let secrets = KeychainSecretProvider::with_service("openwave-ci-probe");
        let key = format!("credential-round-trip-{}", uuid::Uuid::new_v4());

        assert_eq!(secrets.get_secret(&key).await.unwrap(), None);
        secrets.set_secret(&key, "first").await.unwrap();
        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("first")
        );
        secrets.set_secret(&key, "second").await.unwrap();
        assert_eq!(
            secrets.get_secret(&key).await.unwrap().as_deref(),
            Some("second")
        );
        secrets.delete_secret(&key).await.unwrap();
        assert_eq!(secrets.get_secret(&key).await.unwrap(), None);
        // Deleting an absent entry is a no-op, not an error.
        secrets.delete_secret(&key).await.unwrap();
    }
}
