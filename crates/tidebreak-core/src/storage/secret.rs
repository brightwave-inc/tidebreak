use async_trait::async_trait;

use crate::error::Result;

/// Credential custody: secrets keyed by a stable reference string (e.g.
/// `provider.anthropic.credential`). Backed by the OS keychain on desktop. On a
/// self-host server, backed by Vault or by rows encrypted in the deployment's
/// own database (decision 102). Never plaintext rows in the [`Store`].
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Fetch a secret by key, or `None` if unset.
    async fn get_secret(&self, key: &str) -> Result<Option<String>>;

    /// Store (or overwrite) a secret.
    async fn set_secret(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a secret; a no-op if it doesn't exist.
    async fn delete_secret(&self, key: &str) -> Result<()>;
}
