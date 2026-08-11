//! Composite model router — select a concrete adapter from the request's model.
//!
//! The [`Router`] is itself a [`ModelProvider`]: the agent loop holds one
//! provider and never knows whether it's a single backend or a configured set.
//! Selection is config-gated and fail-closed — no enabled+credentialed candidate
//! ⇒ the stream errors without egress.
//!
//! Health-based failover (circuit breaker, retry) is deliberately out of scope
//! here; this slice is selection only.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{ChatRequest, ModelProvider, ProviderEvent, ProviderId};

#[cfg(feature = "anthropic")]
use crate::AnthropicProvider;
#[cfg(feature = "gemini")]
use crate::GeminiProvider;
#[cfg(feature = "openai-compat")]
use crate::OpenAiCompatProvider;
#[cfg(feature = "openai")]
use crate::OpenAiProvider;
#[cfg(feature = "xai")]
use crate::XaiProvider;

/// Header a model-gateway deployment reads to group inference into one
/// conversation. Gateway adapters opt in explicitly; direct providers never
/// send it.
pub(crate) const GATEWAY_CONVERSATION_HEADER: &str = "x-model-gateway-conversation-id";

/// A per-request credential supplier for routes whose token is short-lived.
///
/// A static key is snapshotted into the adapter for the lifetime of a cached
/// router; a governed endpoint minting ten-minute tokens needs the opposite —
/// the adapter asks this source at each request and the source refreshes
/// behind its own lock. Implementations must never log or echo the token.
#[async_trait]
pub trait BearerTokenSource: Send + Sync {
    /// A currently valid token, refreshing if the cached one is near expiry.
    async fn bearer_token(&self) -> Result<String>;

    /// The token for a request that belongs to `conversation`. Sources that
    /// scope credentials per conversation override this — the model gateway
    /// mints inside a per-conversation attestation context, which is what
    /// lets its attested MCP endpoints match the tool calls this inference
    /// emits. Everything else serves the shared token.
    async fn bearer_token_for(
        &self,
        conversation: Option<openwave_core::id::ChatId>,
    ) -> Result<String> {
        let _ = conversation;
        self.bearer_token().await
    }
}

/// Which concrete adapter a [`Route`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RouteKind {
    /// Anthropic Messages API.
    Anthropic,
    /// Native OpenAI Responses API (api.openai.com).
    Openai,
    /// Native xAI Responses API (api.x.ai).
    Xai,
    /// Fireworks AI over the shared OpenAI-compatible adapter.
    Fireworks,
    /// Together AI over the shared OpenAI-compatible adapter.
    Together,
    /// Any OpenAI-compatible Chat Completions gateway.
    OpenaiCompatible,
    /// Google Gemini Developer API (native GenerateContent protocol).
    Gemini,
    /// A model-gateway deployment's Anthropic-compatible surface, authenticated
    /// with short-lived resource-scoped tokens instead of a static key.
    ModelGateway,
    /// The same model-gateway deployment's OpenAI-compatible Chat Completions
    /// surface. It shares the public `model_gateway` provider namespace with
    /// [`ModelGateway`](Self::ModelGateway); the synced model protocol chooses
    /// which concrete route serves a request.
    ModelGatewayOpenai,
}

impl RouteKind {
    /// Stable concrete-route id. Use [`provider_id`](Self::provider_id) for
    /// the host-facing provider namespace.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Anthropic => "anthropic",
            RouteKind::Openai => "openai",
            RouteKind::Xai => "xai",
            RouteKind::Fireworks => "fireworks",
            RouteKind::Together => "together",
            RouteKind::OpenaiCompatible => "openai_compatible",
            RouteKind::Gemini => "gemini",
            RouteKind::ModelGateway => "model_gateway",
            RouteKind::ModelGatewayOpenai => "model_gateway_openai",
        }
    }

    /// Provider id used in host model selections. The two concrete gateway
    /// adapters deliberately share one public provider namespace.
    fn provider_id(self) -> &'static str {
        match self {
            Self::ModelGatewayOpenai => "model_gateway",
            _ => self.as_str(),
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "xai" => Some(Self::Xai),
            "fireworks" => Some(Self::Fireworks),
            "together" => Some(Self::Together),
            "openai_compatible" => Some(Self::OpenaiCompatible),
            "gemini" => Some(Self::Gemini),
            "model_gateway" => Some(Self::ModelGateway),
            _ => None,
        }
    }
}

/// One enabled, credentialed provider endpoint the router may select.
#[derive(Clone)]
pub struct Route {
    /// Which adapter to build.
    pub kind: RouteKind,
    /// API key / bearer token. Empty when `token_source` supplies credentials.
    pub api_key: String,
    /// Optional base URL override (required in practice for `OpenaiCompatible`).
    pub base_url: Option<String>,
    /// Curated model ids this route claims. Used for preferential selection;
    /// `OpenaiCompatible` typically passes an empty list (free-form fallback).
    pub curated_models: Vec<String>,
    /// Per-request credential supplier for short-lived tokens. Takes
    /// precedence over `api_key` when present.
    pub token_source: Option<Arc<dyn BearerTokenSource>>,
    /// ChatGPT account id for OpenAI routes authenticated via ChatGPT OAuth.
    /// Baked into the adapter (only the bearer rotates).
    pub chatgpt_account_id: Option<String>,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("kind", &self.kind)
            .field("api_key", &"***")
            .field("base_url", &self.base_url)
            .field("curated_models", &self.curated_models)
            .field("token_source", &self.token_source.is_some())
            .field("chatgpt_account_id", &self.chatgpt_account_id)
            .finish()
    }
}

/// A composite [`ModelProvider`] that picks a backend from `ChatRequest.model`.
///
/// Selection order:
/// 1. Prefer a route whose curated catalog contains the model.
/// 2. Else, if an `OpenaiCompatible` route is present, use it (free-form /
///    aggregator / local).
/// 3. Else fail closed — no network call.
pub struct Router {
    adapters: HashMap<RouteKind, Arc<dyn ModelProvider>>,
    /// provider + model claims. The provider dimension is intentional: two
    /// endpoints may expose the same raw model id without becoming ambiguous.
    curated: HashMap<String, RouteKind>,
    has_openai_compat: bool,
    /// Fingerprint of the routes this was built from, for cache invalidation.
    fingerprint: String,
}

impl Router {
    /// Build a router from the given routes. Empty input ⇒ a router that always
    /// fails closed on `stream`.
    pub fn build(routes: Vec<Route>) -> Self {
        let fingerprint = fingerprint_routes(&routes);
        let mut adapters: HashMap<RouteKind, Arc<dyn ModelProvider>> = HashMap::new();
        let mut curated: HashMap<String, RouteKind> = HashMap::new();
        let mut has_openai_compat = false;

        for route in routes {
            if route.kind == RouteKind::OpenaiCompatible {
                has_openai_compat = true;
            }
            for model in &route.curated_models {
                curated.insert(format!("{}::{model}", route.kind.provider_id()), route.kind);
            }
            if let Some(adapter) = build_adapter(&route) {
                adapters.insert(route.kind, adapter);
            }
        }

        Self {
            adapters,
            curated,
            has_openai_compat,
            fingerprint,
        }
    }

    /// Opaque fingerprint of the route set this router was built from. Equal
    /// fingerprints mean the same enabled providers / keys / base URLs.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Select the route kind for `model`, if any candidate can serve it.
    pub fn select(&self, model: &str) -> Option<RouteKind> {
        for kind in [
            RouteKind::Anthropic,
            RouteKind::Openai,
            RouteKind::Xai,
            RouteKind::Gemini,
            RouteKind::Fireworks,
            RouteKind::Together,
            RouteKind::ModelGateway,
            RouteKind::ModelGatewayOpenai,
            RouteKind::OpenaiCompatible,
        ] {
            if self
                .curated
                .get(&format!("{}::{model}", kind.provider_id()))
                == Some(&kind)
                && self.adapters.contains_key(&kind)
            {
                return Some(kind);
            }
        }
        if self.has_openai_compat && self.adapters.contains_key(&RouteKind::OpenaiCompatible) {
            return Some(RouteKind::OpenaiCompatible);
        }
        None
    }

    /// Select the exact provider requested by the host model registry.
    ///
    /// Curated Anthropic/OpenAI/Gemini routes must claim the model. A custom
    /// OpenAI-compatible route accepts a legacy free-form model id so existing
    /// pre-registry settings continue to work; new API writes require an
    /// explicit configured custom entry before they can reach this boundary.
    pub fn select_for(&self, provider: Option<&ProviderId>, model: &str) -> Option<RouteKind> {
        let Some(provider) = provider else {
            return self.select(model);
        };
        if provider.0 == "model_gateway" {
            let kind = *self.curated.get(&format!("model_gateway::{model}"))?;
            return self.adapters.contains_key(&kind).then_some(kind);
        }
        let kind = RouteKind::parse(&provider.0)?;
        if !self.adapters.contains_key(&kind) {
            return None;
        }
        if kind == RouteKind::OpenaiCompatible {
            return Some(kind);
        }
        self.curated
            .contains_key(&format!("{}::{model}", kind.as_str()))
            .then_some(kind)
    }
}

#[async_trait]
impl ModelProvider for Router {
    fn id(&self) -> ProviderId {
        ProviderId::new("router")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let Some(kind) = self.select_for(req.provider.as_ref(), &req.model) else {
            return Err(AgentError::config(format!(
                "provider `{}` is unavailable or cannot serve model `{}`",
                req.provider
                    .as_ref()
                    .map_or("unspecified", |provider| provider.0.as_str()),
                req.model,
            )));
        };
        let Some(adapter) = self.adapters.get(&kind) else {
            return Err(AgentError::config(format!(
                "no enabled provider can serve model `{}`",
                req.model
            )));
        };
        // The route hint is host policy, not provider wire data. Adapters keep
        // it only to gate provider-native reasoning/tool replay; their request
        // builders enumerate wire fields explicitly and never serialize it.
        adapter.stream(req).await
    }
}

fn build_adapter(route: &Route) -> Option<Arc<dyn ModelProvider>> {
    if route.api_key.is_empty() && route.token_source.is_none() {
        return None;
    }
    match route.kind {
        #[cfg(feature = "anthropic")]
        RouteKind::Anthropic => {
            let mut p = AnthropicProvider::new(route.api_key.clone());
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            Some(Arc::new(p))
        }
        #[cfg(not(feature = "anthropic"))]
        RouteKind::Anthropic => None,

        #[cfg(feature = "anthropic")]
        RouteKind::ModelGateway => {
            // A gateway route is only usable with its base URL and a live
            // token source; a static key cannot follow the gateway's
            // ten-minute rotation.
            let base = route.base_url.as_deref()?;
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                return None;
            }
            let source = route.token_source.clone()?;
            Some(Arc::new(
                AnthropicProvider::new(String::new())
                    .with_base_url(base.to_string())
                    .with_token_source(source)
                    .with_conversation_attribution(),
            ))
        }
        #[cfg(not(feature = "anthropic"))]
        RouteKind::ModelGateway => None,

        #[cfg(feature = "openai-compat")]
        RouteKind::ModelGatewayOpenai => {
            // Like the Anthropic gateway route, this surface is unusable
            // without a live token source: static credentials cannot follow
            // the gateway's rotation or conversation attestation context.
            let base = route.base_url.as_deref()?;
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                return None;
            }
            let source = route.token_source.clone()?;
            Some(Arc::new(
                OpenAiCompatProvider::compatible(String::new(), base.to_string())
                    .with_id(route.kind.provider_id())
                    .with_token_source(source)
                    .with_conversation_attribution()
                    .with_streaming_usage(),
            ))
        }
        #[cfg(not(feature = "openai-compat"))]
        RouteKind::ModelGatewayOpenai => None,

        #[cfg(feature = "openai")]
        RouteKind::Openai => {
            let mut p = if let Some(source) = route.token_source.clone() {
                let mut provider = OpenAiProvider::new(String::new()).with_token_source(source);
                if let Some(account_id) = &route.chatgpt_account_id {
                    provider = provider.with_chatgpt_account_id(account_id.clone());
                }
                provider
            } else {
                // A ChatGPT account id without a live token source is not a
                // usable Platform API-key route.
                if route.chatgpt_account_id.is_some() {
                    return None;
                }
                OpenAiProvider::new(route.api_key.clone())
            };
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            Some(Arc::new(p))
        }
        #[cfg(feature = "xai")]
        // xAI credentials are valid only for the fixed first-party endpoint.
        // Ignore even a directly constructed route override so stale config
        // cannot redirect the bearer token around the server's collector.
        RouteKind::Xai => Some(Arc::new(XaiProvider::new(route.api_key.clone()))),
        #[cfg(feature = "openai-compat")]
        RouteKind::Fireworks | RouteKind::Together | RouteKind::OpenaiCompatible => {
            let base = route.base_url.as_deref()?;
            // Refuse non-http(s) schemes so a stored base_url can't point the
            // adapter at an arbitrary scheme handler.
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                return None;
            }
            Some(Arc::new(
                OpenAiCompatProvider::compatible(route.api_key.clone(), base.to_string())
                    .with_id(route.kind.as_str()),
            ))
        }
        #[cfg(feature = "gemini")]
        RouteKind::Gemini => {
            if route.api_key.is_empty() || route.token_source.is_some() {
                return None;
            }
            let mut p = GeminiProvider::new(route.api_key.clone());
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            Some(Arc::new(p))
        }
        #[cfg(not(feature = "gemini"))]
        RouteKind::Gemini => None,
        #[cfg(not(feature = "openai"))]
        RouteKind::Openai => None,
        #[cfg(not(feature = "xai"))]
        RouteKind::Xai => None,
        #[cfg(not(feature = "openai-compat"))]
        RouteKind::Fireworks | RouteKind::Together | RouteKind::OpenaiCompatible => None,
    }
}

fn fingerprint_routes(routes: &[Route]) -> String {
    // Hash key material so the fingerprint (cached on the resolver) never
    // retains a cleartext API key.
    let mut parts: Vec<String> = routes
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}|{}|{}",
                r.kind.as_str(),
                // A rotating token must not thrash the cached router; the
                // fingerprint tracks *whether* a live source exists, and the
                // source itself refreshes per request.
                if r.token_source.is_some() {
                    "oauth".to_string()
                } else {
                    format!("{:x}", fnv1a64(&r.api_key))
                },
                r.base_url.as_deref().unwrap_or(""),
                {
                    let mut models = r.curated_models.clone();
                    models.sort();
                    models.join(",")
                },
                r.chatgpt_account_id.as_deref().unwrap_or("")
            )
        })
        .collect();
    parts.sort();
    parts.join(";")
}

/// Tiny non-crypto FNV-1a 64-bit hash — enough to invalidate the cache when a
/// key changes without storing the key itself.
fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;
    use openwave_core::provider::{ChatMessage, ContentBlock, MessageReasoning, ReasoningOrigin};
    use openwave_core::{ImageAttachments, Role};
    #[cfg(feature = "xai")]
    use serde_json::json;

    fn route(kind: RouteKind, key: &str, models: &[&str], base: Option<&str>) -> Route {
        Route {
            kind,
            api_key: key.into(),
            base_url: base.map(str::to_owned),
            curated_models: models.iter().map(|m| (*m).to_string()).collect(),
            token_source: None,
            chatgpt_account_id: None,
        }
    }

    #[test]
    fn selects_curated_anthropic_over_compat_fallback() {
        let router = Router::build(vec![
            route(RouteKind::Anthropic, "sk-a", &["claude-opus-4-8"], None),
            route(
                RouteKind::OpenaiCompatible,
                "sk-c",
                &[],
                Some("http://127.0.0.1:1234/v1"),
            ),
        ]);
        assert_eq!(router.select("claude-opus-4-8"), Some(RouteKind::Anthropic));
        // Unknown model falls through to openai_compatible.
        assert_eq!(router.select("llama-3"), Some(RouteKind::OpenaiCompatible));
    }

    #[test]
    fn curated_openai_model_selects_openai() {
        let router = Router::build(vec![route(RouteKind::Openai, "sk-o", &["gpt-4o"], None)]);
        assert_eq!(router.select("gpt-4o"), Some(RouteKind::Openai));
        assert_eq!(router.select("unknown"), None);
    }

    #[test]
    fn explicit_xai_model_selects_xai_without_compat_fallback() {
        let router = Router::build(vec![
            route(RouteKind::Xai, "xai-key", &["grok-custom"], None),
            route(
                RouteKind::OpenaiCompatible,
                "compat-key",
                &[],
                Some("http://127.0.0.1:1234/v1"),
            ),
        ]);
        assert_eq!(router.select("grok-custom"), Some(RouteKind::Xai));
        assert_eq!(
            router.select_for(Some(&ProviderId::new("xai")), "grok-custom"),
            Some(RouteKind::Xai)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("openai")), "grok-custom"),
            None
        );
    }

    #[test]
    fn explicit_provider_never_cross_routes_an_ambiguous_model() {
        let router = Router::build(vec![
            route(RouteKind::Anthropic, "sk-a", &["shared"], None),
            route(RouteKind::Openai, "sk-o", &["shared"], None),
            route(
                RouteKind::Fireworks,
                "fw-key",
                &["shared"],
                Some("https://api.fireworks.ai/inference/v1"),
            ),
            route(
                RouteKind::Together,
                "tg-key",
                &["shared"],
                Some("https://api.together.ai/v1"),
            ),
        ]);
        assert_eq!(
            router.select_for(Some(&ProviderId::new("anthropic")), "shared"),
            Some(RouteKind::Anthropic)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("openai")), "shared"),
            Some(RouteKind::Openai)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("fireworks")), "shared"),
            Some(RouteKind::Fireworks)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("together")), "shared"),
            Some(RouteKind::Together)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("anthropic")), "openai-only"),
            None
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("unknown")), "shared"),
            None
        );
    }

    #[test]
    fn empty_router_selects_nothing() {
        let router = Router::build(vec![]);
        assert_eq!(router.select("claude-opus-4-8"), None);
    }

    #[test]
    fn openai_compatible_rejects_non_http_base_url() {
        let router = Router::build(vec![route(
            RouteKind::OpenaiCompatible,
            "sk-c",
            &[],
            Some("file:///etc/passwd"),
        )]);
        assert_eq!(router.select("anything"), None);
    }

    struct StaticSource(&'static str);

    #[async_trait]
    impl BearerTokenSource for StaticSource {
        async fn bearer_token(&self) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn a_model_gateway_route_requires_a_live_token_source() {
        // No source ⇒ no adapter, even with a base URL: a static key cannot
        // follow the gateway's rotation, so there is nothing to build.
        let without = Router::build(vec![route(
            RouteKind::ModelGateway,
            "",
            &["claude-fable-5"],
            Some("http://127.0.0.1:28081/compat/anthropic"),
        )]);
        assert_eq!(without.select("claude-fable-5"), None);

        let mut gateway = route(
            RouteKind::ModelGateway,
            "",
            &["claude-fable-5"],
            Some("http://127.0.0.1:28081/compat/anthropic"),
        );
        gateway.token_source = Some(Arc::new(StaticSource("mg_at_x")));
        let with = Router::build(vec![gateway]);
        assert_eq!(with.select("claude-fable-5"), Some(RouteKind::ModelGateway));
        // Only claimed (synced) models route to the gateway; it is not a
        // free-form fallback.
        assert_eq!(with.select("unknown"), None);
        assert_eq!(
            with.select_for(Some(&ProviderId::new("model_gateway")), "claude-fable-5"),
            Some(RouteKind::ModelGateway)
        );
        assert_eq!(
            with.select_for(Some(&ProviderId::new("model_gateway")), "unknown"),
            None
        );
    }

    #[test]
    fn a_gateway_provider_selects_each_models_synced_protocol() {
        let mut anthropic = route(
            RouteKind::ModelGateway,
            "",
            &["claude-fable-5"],
            Some("http://127.0.0.1:28081/compat/anthropic"),
        );
        anthropic.token_source = Some(Arc::new(StaticSource("mg_at_x")));
        let mut openai = route(
            RouteKind::ModelGatewayOpenai,
            "",
            &["gpt-fable-5"],
            Some("http://127.0.0.1:28081/compat/openai/v1"),
        );
        openai.token_source = Some(Arc::new(StaticSource("mg_at_x")));

        let router = Router::build(vec![anthropic, openai]);
        let gateway = ProviderId::new("model_gateway");
        assert_eq!(
            router.select_for(Some(&gateway), "claude-fable-5"),
            Some(RouteKind::ModelGateway)
        );
        assert_eq!(
            router.select_for(Some(&gateway), "gpt-fable-5"),
            Some(RouteKind::ModelGatewayOpenai)
        );
        assert_eq!(router.select_for(Some(&gateway), "unknown"), None);
    }

    #[test]
    fn fingerprint_is_stable_across_token_rotation() {
        let build = |token: &'static str| {
            let mut r = route(
                RouteKind::ModelGateway,
                "",
                &["m"],
                Some("http://127.0.0.1:1/compat/anthropic"),
            );
            r.token_source = Some(Arc::new(StaticSource(token)));
            Router::build(vec![r])
        };
        // The token value must not enter the fingerprint: a rotation would
        // otherwise rebuild the cached router every ten minutes.
        assert_eq!(
            build("mg_at_one").fingerprint(),
            build("mg_at_two").fingerprint()
        );
    }

    #[test]
    fn fingerprint_changes_with_key() {
        let a = Router::build(vec![route(RouteKind::Anthropic, "sk-1", &[], None)]);
        let b = Router::build(vec![route(RouteKind::Anthropic, "sk-2", &[], None)]);
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn fingerprint_does_not_contain_raw_key() {
        let router = Router::build(vec![route(
            RouteKind::Anthropic,
            "sk-super-secret",
            &[],
            None,
        )]);
        assert!(!router.fingerprint().contains("sk-super-secret"));
    }

    #[test]
    fn route_debug_redacts_api_key() {
        let r = route(RouteKind::Anthropic, "sk-super-secret", &[], None);
        assert!(!format!("{r:?}").contains("sk-super-secret"));
    }

    #[tokio::test]
    async fn stream_fails_closed_with_no_match() {
        let router = Router::build(vec![]);
        let result = router
            .stream(ChatRequest {
                provider: Some(ProviderId::new("anthropic")),
                model: "nope".into(),
                reasoning_model: false,
                system: None,
                messages: vec![],
                tools: vec![],
                max_tokens: None,
                temperature: None,
                reasoning_effort: None,
                images: ImageAttachments::new(),
                ..Default::default()
            })
            .await;
        match result {
            Err(err) => assert!(err.to_string().contains("unavailable")),
            Ok(_) => panic!("expected fail-closed error"),
        }
    }

    #[cfg(feature = "xai")]
    struct XaiShapingProbe {
        body: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<serde_json::Value>>>,
    }

    #[cfg(feature = "xai")]
    #[async_trait]
    impl ModelProvider for XaiShapingProbe {
        fn id(&self) -> ProviderId {
            ProviderId::new("xai")
        }

        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let body =
                crate::openai::build_request_json_for(&req, crate::openai::ResponsesProfile::Xai)?;
            if let Some(tx) = self.body.lock().unwrap().take() {
                let _ = tx.send(body);
            }
            Ok(Box::pin(futures::stream::empty::<ProviderEvent>()))
        }
    }

    #[cfg(feature = "xai")]
    #[tokio::test]
    async fn router_preserves_xai_identity_for_reasoning_replay_without_serializing_it() {
        let reasoning = json!({
            "id": "rs_previous",
            "summary": [],
            "type": "reasoning",
            "status": "completed",
            "encrypted_content": "opaque-previous",
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut adapters: HashMap<RouteKind, Arc<dyn ModelProvider>> = HashMap::new();
        adapters.insert(
            RouteKind::Xai,
            Arc::new(XaiShapingProbe {
                body: std::sync::Mutex::new(Some(tx)),
            }),
        );
        let router = Router {
            adapters,
            curated: HashMap::from([("xai::grok-test".into(), RouteKind::Xai)]),
            has_openai_compat: false,
            fingerprint: String::new(),
        };

        let _stream = router
            .stream(ChatRequest {
                provider: Some(ProviderId::new("xai")),
                model: "grok-test".into(),
                reasoning_model: true,
                messages: vec![ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "first answer".into(),
                    }],
                    reasoning: MessageReasoning::captured(
                        ReasoningOrigin {
                            provider: Some(ProviderId::new("xai")),
                            model: "grok-test".into(),
                        },
                        vec![reasoning.clone()],
                    ),
                }],
                ..Default::default()
            })
            .await
            .unwrap();

        let body = rx.await.unwrap();
        assert_eq!(body["input"][0], reasoning);
        assert!(body.get("provider").is_none());
    }

    #[tokio::test]
    async fn router_preserves_the_route_hint_needed_for_native_reasoning_replay() {
        use axum::body::Body;
        use axum::extract::{Request, State};
        use axum::response::IntoResponse;
        use axum::routing::post;
        use tokio::sync::oneshot;

        async fn capture(
            State(tx): State<Arc<std::sync::Mutex<Option<oneshot::Sender<serde_json::Value>>>>>,
            request: Request,
        ) -> impl IntoResponse {
            let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                .await
                .unwrap();
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(serde_json::from_slice(&body).unwrap());
            }
            (
                [("content-type", "text/event-stream")],
                Body::from(
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
                ),
            )
        }

        let (tx, rx) = oneshot::channel();
        let app = axum::Router::new()
            .fallback(post(capture))
            .with_state(Arc::new(std::sync::Mutex::new(Some(tx))));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let model = "claude-sonnet-5";
        let router = Router::build(vec![route(
            RouteKind::Anthropic,
            "key",
            &[model],
            Some(&format!("http://{address}")),
        )]);
        let mut stream = router
            .stream(ChatRequest {
                provider: Some(ProviderId::new("anthropic")),
                model: model.into(),
                reasoning_model: true,
                messages: vec![ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "answer".into(),
                    }],
                    reasoning: MessageReasoning::captured(
                        ReasoningOrigin {
                            provider: Some(ProviderId::new("anthropic")),
                            model: model.into(),
                        },
                        vec![serde_json::json!({
                            "type": "thinking",
                            "thinking": "plan",
                            "signature": "signed",
                        })],
                    ),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let body = rx.await.unwrap();
        assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");
    }
}
