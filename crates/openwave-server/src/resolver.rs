//! Resolving the model provider for a turn from configured credentials.
//!
//! The provider is built *per turn* rather than once at boot, so setting an API
//! key at runtime (`PUT /settings/api-key`) takes effect on the next turn without
//! a restart. The [`ProviderResolver`] seam also lets tests inject a provider
//! directly instead of standing up a real backend.

use std::sync::Arc;

use async_trait::async_trait;

use openwave_core::{ModelProvider, SecretProvider};
use openwave_router::AnthropicProvider;

use crate::provider::UnconfiguredProvider;

/// Secret key under which the Anthropic API key is stored.
pub const ANTHROPIC_API_KEY: &str = "provider.anthropic.api_key";

/// Builds the model provider for the next turn.
#[async_trait]
pub trait ProviderResolver: Send + Sync {
    /// Resolve the provider. Infallible: with no credentials it returns a
    /// fail-closed provider, so a turn surfaces `TurnFailed` instead of egressing.
    async fn resolve(&self) -> Arc<dyn ModelProvider>;
}

/// Resolves an Anthropic provider from the stored API key — falling back to the
/// `ANTHROPIC_API_KEY` environment variable — or a fail-closed provider if none.
pub struct KeyedResolver {
    secrets: Arc<dyn SecretProvider>,
}

impl KeyedResolver {
    /// A resolver that reads the API key from `secrets`.
    pub fn new(secrets: Arc<dyn SecretProvider>) -> Self {
        Self { secrets }
    }
}

#[async_trait]
impl ProviderResolver for KeyedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        let key = self
            .secrets
            .get_secret(ANTHROPIC_API_KEY)
            .await
            .ok()
            .flatten()
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .filter(|key| !key.is_empty());
        match key {
            Some(key) => Arc::new(AnthropicProvider::new(key)),
            // Fail closed: no key ⇒ a provider that refuses without any network call.
            None => Arc::new(UnconfiguredProvider),
        }
    }
}
