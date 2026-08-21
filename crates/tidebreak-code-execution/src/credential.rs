use std::sync::Arc;

use sha2::{Digest, Sha256};
use tidebreak_core::SecretProvider;

use crate::ExecError;

/// Shared storage and redaction primitive for managed-provider credentials.
#[derive(Clone)]
pub(crate) struct SecretCredential(Arc<str>);

impl std::fmt::Debug for SecretCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SecretCredential")
            .field(&"***")
            .finish()
    }
}

impl SecretCredential {
    pub(crate) fn parse(provider: &str, value: impl Into<String>) -> Result<Self, ExecError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ExecError::InvalidRequest(format!(
                "{provider} API key must not be empty"
            )));
        }
        Ok(Self(Arc::from(trimmed)))
    }

    pub(crate) async fn load(
        secrets: &dyn SecretProvider,
        key: &str,
        provider: &str,
    ) -> Result<Option<Self>, ExecError> {
        let value = secrets.get_secret(key).await.map_err(|_| {
            ExecError::Unavailable(format!("{provider} credential storage is unavailable"))
        })?;
        value
            .filter(|value| !value.trim().is_empty())
            .map(|value| Self::parse(provider, value))
            .transpose()
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}
