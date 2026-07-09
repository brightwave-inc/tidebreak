//! Resolving the model provider for a turn from configured credentials.
//!
//! The provider is built *per turn* rather than once at boot, so setting an API
//! key at runtime (`PUT /providers/anthropic` or the legacy
//! `PUT /settings/api-key`) takes effect on the next turn without a restart.
//! The [`ProviderResolver`] seam also lets tests inject a provider directly
//! instead of standing up a real backend.
//!
//! Today this still resolves Anthropic only — the composite router (model →
//! provider selection across configured kinds) is the next slice. Credentials
//! are read through the providers module so the new typed blob and the legacy
//! plain-string key both work.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use openwave_core::{ModelProvider, SecretProvider, Store};
use openwave_router::AnthropicProvider;

use crate::provider::UnconfiguredProvider;
use crate::providers::{self, ProviderKind};

/// Cache key: whether Anthropic is enabled + the resolved API key. Either
/// changing rebuilds the provider.
type CacheKey = (bool, Option<String>);
type CachedProvider = Option<(CacheKey, Arc<dyn ModelProvider>)>;

/// Builds the model provider for the next turn.
#[async_trait]
pub trait ProviderResolver: Send + Sync {
    /// Resolve the provider. Infallible: with no credentials it returns a
    /// fail-closed provider, so a turn surfaces `TurnFailed` instead of egressing.
    async fn resolve(&self) -> Arc<dyn ModelProvider>;
}

/// Resolves an Anthropic provider when the Anthropic provider is enabled and an
/// API key is available (stored credential or `ANTHROPIC_API_KEY` env) — or a
/// fail-closed provider otherwise.
///
/// The built provider is cached and reused while the key and enabled flag are
/// unchanged, so a provider (and its `reqwest` connection pool) isn't rebuilt
/// every turn; a key or enabled change rebuilds it on the next turn.
///
/// OpenAI / openai_compatible credentials are accepted by the `/providers` API
/// but not yet selected here — the composite router is the next slice.
pub struct KeyedResolver {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    cached: Mutex<CachedProvider>,
}

impl KeyedResolver {
    /// A resolver that reads Anthropic config from `store` and the API key from
    /// `secrets`.
    pub fn new(store: Arc<dyn Store>, secrets: Arc<dyn SecretProvider>) -> Self {
        Self {
            store,
            secrets,
            cached: Mutex::new(None),
        }
    }
}

#[async_trait]
impl ProviderResolver for KeyedResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        let enabled = providers::read_config(&*self.store, ProviderKind::Anthropic)
            .await
            .map(|c| c.enabled)
            // Store read failure → treat as disabled (fail closed).
            .unwrap_or(false);
        let key = if enabled {
            providers::resolve_anthropic_api_key(&*self.secrets).await
        } else {
            None
        };
        let cache_key = (enabled, key.clone());

        // Reuse the cached provider while the key/enabled are unchanged. The
        // lock is held only across the cheap, synchronous build below — never
        // over an await.
        let mut cached = self.cached.lock().unwrap();
        if let Some((cached_key, provider)) = cached.as_ref() {
            if *cached_key == cache_key {
                return provider.clone();
            }
        }
        let provider: Arc<dyn ModelProvider> = match &key {
            Some(key) => Arc::new(AnthropicProvider::new(key.clone())),
            // Fail closed: disabled or no key ⇒ refuse without any network call.
            None => Arc::new(UnconfiguredProvider),
        };
        *cached = Some((cache_key, provider.clone()));
        provider
    }
}
