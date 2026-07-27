//! Resolving the model provider for a turn from configured credentials.
//!
//! Builds a composite [`openwave_router::Router`] from every enabled,
//! credentialed provider so the turn's model selects the right adapter
//! (Anthropic / OpenAI / openai_compatible). The router is rebuilt when the
//! route set changes (enable/disable, key, base_url); otherwise the cached
//! instance — and its connection pools — is reused.
//!
//! The [`ProviderResolver`] seam also lets tests inject a provider directly
//! instead of standing up a real backend.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use openwave_core::{ModelProvider, SecretProvider, Store};
use openwave_router::Router;

use crate::provider::UnconfiguredProvider;
use crate::providers;

/// Cache: fingerprint of the route set + the built provider.
type CachedProvider = Option<(String, Arc<dyn ModelProvider>)>;

/// Builds the model provider for the next turn.
#[async_trait]
pub trait ProviderResolver: Send + Sync {
    /// Resolve the provider. Infallible: with no credentials it returns a
    /// fail-closed provider, so a turn surfaces `TurnFailed` instead of egressing.
    async fn resolve(&self) -> Arc<dyn ModelProvider>;

    /// Whether public model selections must resolve through the host registry.
    ///
    /// Production configured routing returns true. Test/custom embedders that
    /// inject one provider keep their existing free-form model contract.
    fn enforces_model_registry(&self) -> bool {
        false
    }
}

/// Resolves a composite [`Router`] from enabled, credentialed providers — or a
/// fail-closed provider when none are configured.
///
/// Selection happens inside the router from the host-resolved provider hint and
/// raw provider model ID. It never crosses to another provider implicitly. The
/// OpenAI-compatible free-form fallback remains only for legacy stored rows;
/// new public selections must be registered first. No default provider: empty
/// config ⇒ no egress.
pub struct ConfiguredResolver {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
    cached: Mutex<CachedProvider>,
}

impl ConfiguredResolver {
    /// A resolver that reads provider config from `store` and credentials from
    /// `secrets`, with `gateway` supplying the model-gateway token source.
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
    ) -> Self {
        Self {
            store,
            secrets,
            gateway,
            cached: Mutex::new(None),
        }
    }
}

/// Backward-compatible alias — earlier slices called this `KeyedResolver`.
pub type KeyedResolver = ConfiguredResolver;

#[async_trait]
impl ProviderResolver for ConfiguredResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        let gateway_tokens = self.gateway.route_token_source().await;
        let routes = providers::collect_routes(&*self.store, &*self.secrets, gateway_tokens).await;
        let router = Router::build(routes);
        let fingerprint = router.fingerprint().to_string();

        // Reuse the cached provider while the route set is unchanged. The lock
        // is held only across the cheap clone below — never over an await.
        let mut cached = self.cached.lock().unwrap();
        if let Some((cached_fp, provider)) = cached.as_ref() {
            if *cached_fp == fingerprint {
                return provider.clone();
            }
        }

        let provider: Arc<dyn ModelProvider> = if fingerprint.is_empty() {
            // No enabled+credentialed routes ⇒ fail closed without egress.
            Arc::new(UnconfiguredProvider)
        } else {
            Arc::new(router)
        };
        *cached = Some((fingerprint, provider.clone()));
        provider
    }

    fn enforces_model_registry(&self) -> bool {
        true
    }
}
