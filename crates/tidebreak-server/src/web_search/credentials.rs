use tidebreak_core::SecretProvider;

use crate::web_search::{WebSearchError, WebSearchProviderKind};

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

/// Whether one provider's credential requirement is satisfied.
///
/// "No key stored" and "no key needed" are separate states on purpose. Folding
/// them into one `Option` would make a provider that *requires* a key look
/// usable the moment a credential-free one existed, and failing closed on a
/// missing key is the whole point of resolving credentials at all.
#[derive(Debug)]
pub enum WebSearchCredentialState {
    /// The provider's fixed key is stored and non-empty.
    Present(WebSearchCredential),
    /// The provider requires a key and none is stored. Callers must fail
    /// closed and make no request.
    Missing,
    /// The provider takes no credential — a self-hosted instance the operator
    /// runs. There is nothing here to fail closed on.
    NotRequired,
}

/// Reads provider keys from the application's existing secret boundary.
#[derive(Debug, Default, Clone, Copy)]
pub struct WebSearchCredentials;

impl WebSearchCredentials {
    /// Resolve one provider's credential state. Missing or whitespace-only
    /// entries are [`WebSearchCredentialState::Missing`], never a usable
    /// credential.
    pub async fn resolve(
        secrets: &dyn SecretProvider,
        kind: WebSearchProviderKind,
    ) -> Result<WebSearchCredentialState, WebSearchError> {
        let Some(key) = kind.credential_key() else {
            return Ok(WebSearchCredentialState::NotRequired);
        };
        let value = secrets
            .get_secret(key)
            .await
            .map_err(|error| WebSearchError::Transport(error.to_string()))?;
        Ok(value
            .filter(|value| !value.trim().is_empty())
            .map_or(WebSearchCredentialState::Missing, |api_key| {
                WebSearchCredentialState::Present(WebSearchCredential { kind, api_key })
            }))
    }
}
