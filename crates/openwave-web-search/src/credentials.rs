use openwave_core::SecretProvider;

use crate::{WebSearchError, WebSearchProviderKind};

/// Non-serializable, redacted API credential for one search provider.
#[derive(Clone)]
pub struct WebSearchCredential {
    kind: WebSearchProviderKind,
    api_key: String,
}

impl std::fmt::Debug for WebSearchCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebSearchCredential")
            .field("kind", &self.kind)
            .field("api_key", &"***")
            .finish()
    }
}

impl WebSearchCredential {
    #[must_use]
    pub fn kind(&self) -> WebSearchProviderKind {
        self.kind
    }

    /// The key is intentionally only exposed to provider adapters that attach
    /// it to an HTTP request. Do not serialize or log this value.
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }
}

/// Reads provider keys from the application's existing secret boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct WebSearchCredentials;

impl WebSearchCredentials {
    /// Resolve one provider's key. Missing or whitespace-only entries mean the
    /// provider is disabled; callers must fail closed and make no request.
    pub async fn load(
        secrets: &dyn SecretProvider,
        kind: WebSearchProviderKind,
    ) -> Result<Option<WebSearchCredential>, WebSearchError> {
        let value = secrets
            .get_secret(kind.credential_key())
            .await
            .map_err(|error| WebSearchError::Transport(error.to_string()))?;
        Ok(value
            .filter(|value| !value.trim().is_empty())
            .map(|api_key| WebSearchCredential { kind, api_key }))
    }
}
