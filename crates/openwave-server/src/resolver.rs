//! Resolving the model provider for a turn from configured credentials.
//!
//! The provider is built *per turn* rather than once at boot, so setting an API
//! key at runtime (`PUT /settings/api-key`) takes effect on the next turn without
//! a restart. The [`ProviderResolver`] seam also lets tests inject a provider
//! directly instead of standing up a real backend.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use openwave_core::{ModelProvider, SecretProvider};
use openwave_router::AnthropicProvider;

use crate::provider::UnconfiguredProvider;

/// Secret key under which the Anthropic API key is stored.
pub const ANTHROPIC_API_KEY: &str = "provider.anthropic.api_key";

/// The provider last built by a [`KeyedResolver`], tagged with the key it was
/// built from so it can be reused until the key changes.
type CachedProvider = Option<(Option<String>, Arc<dyn ModelProvider>)>;

/// Builds the model provider for the next turn.
#[async_trait]
pub trait ProviderResolver: Send + Sync {
    /// Resolve the provider. Infallible: with no credentials it returns a
    /// fail-closed provider, so a turn surfaces `TurnFailed` instead of egressing.
    async fn resolve(&self) -> Arc<dyn ModelProvider>;
}

/// Resolves an Anthropic provider from the stored API key — falling back to the
/// `ANTHROPIC_API_KEY` environment variable — or a fail-closed provider if none.
///
/// The built provider is cached and reused while the key is unchanged, so a
/// provider (and its `reqwest` connection pool) isn't rebuilt every turn; a key
/// change (via `PUT /settings/api-key`) rebuilds it on the next turn.
pub struct KeyedResolver {
    secrets: Arc<dyn SecretProvider>,
    cached: Mutex<CachedProvider>,
}

impl KeyedResolver {
    /// A resolver that reads the API key from `secrets`.
    pub fn new(secrets: Arc<dyn SecretProvider>) -> Self {
        Self {
            secrets,
            cached: Mutex::new(None),
        }
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

        // Reuse the cached provider while the key is unchanged. The lock is held
        // only across the cheap, synchronous build below — never over an await.
        let mut cached = self.cached.lock().unwrap();
        if let Some((cached_key, provider)) = cached.as_ref() {
            if *cached_key == key {
                return provider.clone();
            }
        }
        let provider: Arc<dyn ModelProvider> = match &key {
            Some(key) => Arc::new(AnthropicProvider::new(key.clone())),
            // Fail closed: no key ⇒ a provider that refuses without any network call.
            None => Arc::new(UnconfiguredProvider),
        };
        *cached = Some((key, provider.clone()));
        provider
    }
}
