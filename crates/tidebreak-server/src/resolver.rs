//! Resolving the model provider for a turn from configured credentials.
//!
//! Builds a composite [`tidebreak_router::Router`] from every enabled,
//! credentialed provider so the turn's model selects the right adapter
//! (Anthropic / OpenAI / Gemini / hosted or custom OpenAI-compatible). The router is rebuilt when the
//! route set changes (enable/disable, key, base_url); otherwise the cached
//! instance — and its connection pools — is reused.
//!
//! The [`ProviderResolver`] seam also lets tests inject a provider directly
//! instead of standing up a real backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use tidebreak_core::{ModelProvider, OwnerId, SecretProvider, Store};
use tidebreak_router::Router;

use crate::provider::UnconfiguredProvider;
use crate::providers;

/// Cache: per caller, the fingerprint of their route set + the built provider.
///
/// Keyed by caller because a hosted machine can resolve model credentials per
/// caller (decision 51). Two callers' route sets fingerprint identically — the
/// fingerprint records only *that* a live token source exists, never whose —
/// so one shared slot would hand one caller another caller's credential. The
/// key keeps them apart. Deployments that resolve no credential per caller use
/// the single `None` entry and behave exactly as before.
type CachedProviders = HashMap<Option<OwnerId>, (String, Arc<dyn ModelProvider>)>;

/// Builds the model provider for the next turn.
#[async_trait]
pub trait ProviderResolver: Send + Sync {
    /// Resolve the provider. Infallible: with no credentials it returns a
    /// fail-closed provider, so a turn surfaces `TurnFailed` instead of egressing.
    async fn resolve(&self) -> Arc<dyn ModelProvider>;

    /// Resolve the provider for the caller a turn belongs to.
    ///
    /// `None` means the caller cannot be named — a system path with no person
    /// behind it, or a chat whose owner no longer resolves. Implementations
    /// that resolve credentials per caller must then offer none, so an
    /// unattributable turn fails closed rather than borrowing someone's
    /// authority.
    ///
    /// The default ignores the caller, which is right for every deployment
    /// whose credentials are deployment-wide.
    async fn resolve_for(&self, owner: Option<&OwnerId>) -> Arc<dyn ModelProvider> {
        let _ = owner;
        self.resolve().await
    }

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
    chatgpt: Arc<crate::chatgpt_runtime::ChatGptRuntime>,
    provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
    os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    on_behalf_of: Option<Arc<crate::obo_inference::OboInference>>,
    cached: Mutex<CachedProviders>,
}

impl ConfiguredResolver {
    /// A resolver that reads provider config from `store` and credentials from
    /// `secrets`, with `gateway` and `chatgpt` supplying their OAuth token
    /// sources and `provisioned_policy`/`os_policy` the two authorities for
    /// managed-mode resolution.
    ///
    /// Both OAuth runtimes must be the same instances the rest of the process
    /// holds — see [`crate::state::AppState::with_gateway_runtime`].
    pub fn new(
        store: Arc<dyn Store>,
        secrets: Arc<dyn SecretProvider>,
        gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
        chatgpt: Arc<crate::chatgpt_runtime::ChatGptRuntime>,
        provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource>,
        os_policy: Arc<dyn crate::managed_policy::OsPolicySource>,
    ) -> Self {
        Self {
            store,
            secrets,
            gateway,
            chatgpt,
            provisioned_policy,
            os_policy,
            on_behalf_of: None,
            cached: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve model credentials per caller through `on_behalf_of` wherever the
    /// deployment states no other inference path (decision 51).
    ///
    /// Only a gateway-authenticated hosted machine supplies one. Every other
    /// deployment leaves this unset and keeps its configured providers.
    #[must_use]
    pub(crate) fn with_on_behalf_of_inference(
        mut self,
        on_behalf_of: Option<Arc<crate::obo_inference::OboInference>>,
    ) -> Self {
        self.on_behalf_of = on_behalf_of;
        self
    }
}

/// Backward-compatible alias — earlier slices called this `KeyedResolver`.
pub type KeyedResolver = ConfiguredResolver;

#[async_trait]
impl ProviderResolver for ConfiguredResolver {
    async fn resolve(&self) -> Arc<dyn ModelProvider> {
        self.resolve_for(None).await
    }

    async fn resolve_for(&self, owner: Option<&OwnerId>) -> Arc<dyn ModelProvider> {
        // A profile that claims to be managed but whose policy cannot be read
        // fails closed: no egress, rather than quietly reverting to BYOK routes.
        let Ok(policy) =
            crate::managed_policy::resolve(&*self.provisioned_policy, &*self.os_policy)
        else {
            return Arc::new(UnconfiguredProvider);
        };
        let gateway_tokens = self.gateway.route_token_source().await;
        let chatgpt = self.chatgpt.route_auth().await;
        // Per-caller credentials need a caller. An unnamed turn is offered
        // none, which fails it closed instead of running it as somebody else.
        let on_behalf_of = self
            .on_behalf_of
            .as_ref()
            .zip(owner)
            .and_then(|(inference, owner)| {
                inference
                    .token_source_for(owner)
                    .map(|source| (source, inference.inference_base_url().to_owned()))
            });
        let routes = providers::collect_routes(
            &*self.store,
            &*self.secrets,
            gateway_tokens,
            chatgpt,
            on_behalf_of,
            &policy,
        )
        .await;
        let router = Router::build(routes);
        let fingerprint = router.fingerprint().to_string();

        // Reuse this caller's cached provider while their route set is
        // unchanged. The lock is held only across the cheap clone below —
        // never over an await.
        let key = self
            .on_behalf_of
            .is_some()
            .then(|| owner.cloned())
            .flatten();
        let mut cached = self.cached.lock().unwrap();
        if let Some((cached_fp, provider)) = cached.get(&key) {
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
        cached.insert(key, (fingerprint, provider.clone()));
        provider
    }

    fn enforces_model_registry(&self) -> bool {
        true
    }
}
