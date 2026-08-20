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

use tidebreak_core::error::{AgentError, Result};
use tidebreak_core::provider::{ChatRequest, ModelProvider, ProviderEvent, ProviderId};

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
    /// Stable non-secret identity this source is bound to, when its credential
    /// must not follow a replacement session behind the same endpoint.
    fn binding_id(&self) -> Option<&str> {
        None
    }

    /// Whether every HTTP request through this source must carry a live model
    /// route lease. Ordinary rotating credentials return false; managed Model
    /// Gateway sources return true.
    fn requires_model_route_lease(&self) -> bool {
        false
    }

    /// Validate a host-only model selector against live route authority and
    /// lease its provider wire id through one HTTP request setup.
    async fn lease_model_route(&self, _route_model: &str) -> Result<Option<ModelRouteLease>> {
        Ok(None)
    }

    /// Mint a bearer and return the final live route lease for one HTTP leg.
    ///
    /// The default is suitable for ordinary rotating credentials and sources
    /// whose route authority has no local mutation fence. Managed sources
    /// override this to hold one authority read guard across mint, final live
    /// validation, and dispatch — avoiding both a local mutation gap and a
    /// recursive fair-lock read when a writer is queued.
    async fn authorize_model_route(
        &self,
        route_model: &str,
        conversation: Option<tidebreak_core::id::ChatId>,
    ) -> Result<(String, Option<ModelRouteLease>)> {
        let token = self.bearer_token_for(conversation).await?;
        let lease = self.lease_model_route(route_model).await?;
        Ok((token, lease))
    }

    /// A currently valid token, refreshing if the cached one is near expiry.
    async fn bearer_token(&self) -> Result<String>;

    /// The token for a request that belongs to `conversation`. Sources that
    /// scope credentials per conversation override this — the model gateway
    /// mints inside a per-conversation attestation context, which is what
    /// lets its attested MCP endpoints match the tool calls this inference
    /// emits. Everything else serves the shared token.
    async fn bearer_token_for(
        &self,
        conversation: Option<tidebreak_core::id::ChatId>,
    ) -> Result<String> {
        let _ = conversation;
        self.bearer_token().await
    }
}

/// A live claim that one host execution selector still maps to one wire model.
pub struct ModelRouteLease {
    wire_model: String,
    request_shaping_model: String,
    _guard: Box<dyn Send>,
}

impl ModelRouteLease {
    pub fn new(wire_model: impl Into<String>, guard: impl Send + 'static) -> Self {
        let wire_model = wire_model.into();
        Self {
            request_shaping_model: wire_model.clone(),
            wire_model,
            _guard: Box::new(guard),
        }
    }

    pub fn with_request_shaping_model(
        wire_model: impl Into<String>,
        request_shaping_model: impl Into<String>,
        guard: impl Send + 'static,
    ) -> Self {
        Self {
            wire_model: wire_model.into(),
            request_shaping_model: request_shaping_model.into(),
            _guard: Box::new(guard),
        }
    }

    pub fn wire_model(&self) -> &str {
        &self.wire_model
    }

    pub fn request_shaping_model(&self) -> &str {
        &self.request_shaping_model
    }
}

/// Mint one request credential under the same short-lived route claim that
/// validates its host execution selector.
///
/// Adapters call this immediately before every HTTP leg. The returned lease
/// must stay alive until that request has been dispatched; Anthropic therefore
/// repeats the call for each `pause_turn` continuation.
pub(crate) async fn authorize_bearer_request(
    source: &dyn BearerTokenSource,
    route_model: &str,
    wire_model: &str,
    conversation: Option<tidebreak_core::id::ChatId>,
) -> Result<(String, Option<ModelRouteLease>)> {
    // Managed sources perform the final validation after their network-backed
    // mint while retaining the same local authority guard through dispatch.
    // The source owns that ordering because only it can reuse one fair-lock
    // read guard rather than deadlocking on a recursive acquisition.
    let (token, lease) = source
        .authorize_model_route(route_model, conversation)
        .await?;
    if source.requires_model_route_lease() && lease.is_none() {
        return Err(AgentError::config(format!(
            "provider no longer serves model `{route_model}`"
        )));
    }
    if let Some(lease) = lease.as_ref() {
        if lease.wire_model() != wire_model {
            return Err(AgentError::config(format!(
                "provider changed the route for model `{route_model}`"
            )));
        }
    }
    Ok((token, lease))
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
    /// OpenRouter over the shared OpenAI-compatible adapter.
    Openrouter,
    /// A local Ollama daemon over the shared OpenAI-compatible adapter.
    Ollama,
    /// Any OpenAI-compatible Chat Completions gateway.
    OpenaiCompatible,
    /// Google Gemini Developer API (native GenerateContent protocol).
    Gemini,
    /// A model-gateway deployment's Anthropic-compatible surface, authenticated
    /// with short-lived resource-scoped tokens instead of a static key.
    ModelGateway,
    /// The same model-gateway deployment's OpenAI Responses surface
    /// (`/compat/openai/v1/responses`). It shares the public `model_gateway`
    /// provider namespace with [`ModelGateway`](Self::ModelGateway); the
    /// synced model protocol chooses which concrete route serves a request.
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
            RouteKind::Openrouter => "openrouter",
            RouteKind::Ollama => "ollama",
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
            "openrouter" => Some(Self::Openrouter),
            "ollama" => Some(Self::Ollama),
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
    /// Host-only model selectors rewritten to provider wire ids after this
    /// exact route has claimed them.
    ///
    /// Managed gateway turns use this to bind an accepted turn to one catalog
    /// identity without leaking Tidebreak's frozen selector onto the wire.
    /// Every key must also appear in `curated_models`; unclaimed rewrites are
    /// ignored.
    pub model_rewrites: HashMap<String, String>,
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
            .field("model_rewrites", &self.model_rewrites)
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
    /// Provider-qualified host selector to provider wire model id.
    model_rewrites: HashMap<String, String>,
    /// Live authority for mutable catalog-backed model rewrites.
    route_authorities: HashMap<RouteKind, Arc<dyn BearerTokenSource>>,
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
        let mut model_rewrites = HashMap::new();
        let mut route_authorities = HashMap::new();
        let mut has_openai_compat = false;

        for route in routes {
            if route.kind == RouteKind::OpenaiCompatible {
                has_openai_compat = true;
            }
            for model in &route.curated_models {
                let qualified = format!("{}::{model}", route.kind.provider_id());
                curated.insert(qualified.clone(), route.kind);
                if let Some(wire_model) = route.model_rewrites.get(model) {
                    model_rewrites.insert(qualified, wire_model.clone());
                }
            }
            if let Some(adapter) = build_adapter(&route) {
                adapters.insert(route.kind, adapter);
                if let Some(source) = route.token_source.clone() {
                    route_authorities.insert(route.kind, source);
                }
            }
        }

        Self {
            adapters,
            curated,
            model_rewrites,
            route_authorities,
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
            RouteKind::Openrouter,
            RouteKind::Ollama,
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

    async fn stream(&self, mut req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
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
        let qualified = format!("{}::{}", kind.provider_id(), req.model);
        let route_lease = if let Some(expected_wire) = self.model_rewrites.get(&qualified) {
            let source = self.route_authorities.get(&kind).ok_or_else(|| {
                AgentError::config(format!(
                    "provider `{}` has no live authority for model `{}`",
                    kind.provider_id(),
                    req.model
                ))
            })?;
            let lease = source.lease_model_route(&req.model).await?.ok_or_else(|| {
                AgentError::config(format!(
                    "provider `{}` no longer serves model `{}`",
                    kind.provider_id(),
                    req.model
                ))
            })?;
            if lease.wire_model() != expected_wire {
                return Err(AgentError::config(format!(
                    "provider `{}` changed the route for model `{}`",
                    kind.provider_id(),
                    req.model
                )));
            }
            req.wire_model = Some(lease.wire_model().to_owned());
            req.request_shaping_model = Some(lease.request_shaping_model().to_owned());
            Some(lease)
        } else {
            None
        };
        // The route hint is host policy, not provider wire data. Adapters keep
        // it only to gate provider-native reasoning/tool replay; their request
        // builders enumerate wire fields explicitly and never serialize it.
        // The concrete gateway adapter obtains its own request-scoped lease
        // immediately before HTTP dispatch, and repeats that authorization for
        // any provider-managed continuation. Drop this router-level proof first
        // so a non-reentrant authority lock cannot deadlock the adapter.
        drop(route_lease);
        adapter.stream(req).await
    }
}

fn build_adapter(route: &Route) -> Option<Arc<dyn ModelProvider>> {
    let credentialless_ollama =
        route.kind == RouteKind::Ollama && route.api_key.is_empty() && route.token_source.is_none();
    if route.api_key.is_empty() && route.token_source.is_none() && !credentialless_ollama {
        return None;
    }
    match route.kind {
        #[cfg(feature = "anthropic")]
        RouteKind::Anthropic => {
            let mut p = AnthropicProvider::new(route.api_key.clone());
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            // A hosted machine that resolves Anthropic credentials per caller
            // supplies a rotating source instead of a key. The adapter then
            // asks for a bearer at each request, so a token that expires
            // mid-turn is replaced without rebuilding the route.
            if let Some(source) = route.token_source.clone() {
                p = p.with_token_source(source);
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

        #[cfg(feature = "openai")]
        RouteKind::ModelGatewayOpenai => {
            // Like the Anthropic gateway route, this surface is unusable
            // without a live token source: static credentials cannot follow
            // the gateway's rotation or conversation attestation context. The
            // gateway's OpenAI surface is the Responses API — it serves no
            // northbound Chat Completions route.
            let base = route.base_url.as_deref()?;
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                return None;
            }
            let source = route.token_source.clone()?;
            Some(Arc::new(
                OpenAiProvider::new(String::new())
                    .with_base_url(base.to_string())
                    .with_provider_label(route.kind.provider_id())
                    .with_token_source(source)
                    .with_conversation_attribution(),
            ))
        }
        #[cfg(not(feature = "openai"))]
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
        RouteKind::Fireworks
        | RouteKind::Together
        | RouteKind::Openrouter
        | RouteKind::Ollama
        | RouteKind::OpenaiCompatible => {
            let base = route.base_url.as_deref()?;
            let configurable_transport =
                matches!(route.kind, RouteKind::Ollama | RouteKind::OpenaiCompatible);
            if configurable_transport && !base_url_is_allowed(base, credentialless_ollama) {
                return None;
            }
            if !configurable_transport
                && !(base.starts_with("https://") || base.starts_with("http://"))
            {
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
        RouteKind::Fireworks
        | RouteKind::Together
        | RouteKind::Openrouter
        | RouteKind::Ollama
        | RouteKind::OpenaiCompatible => None,
    }
}

fn base_url_is_allowed(base: &str, allow_credentialless_loopback_http: bool) -> bool {
    let Ok(url) = url::Url::parse(base) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" if allow_credentialless_loopback_http => url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }),
        _ => false,
    }
}

fn fingerprint_routes(routes: &[Route]) -> String {
    // Hash key material so the fingerprint (cached on the resolver) never
    // retains a cleartext API key.
    let mut parts: Vec<String> = routes
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}|{}|{}|{}",
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
                {
                    let mut rewrites: Vec<_> = r.model_rewrites.iter().collect();
                    rewrites.sort();
                    rewrites
                        .into_iter()
                        .map(|(selection, wire)| format!("{selection}>{wire}"))
                        .collect::<Vec<_>>()
                        .join(",")
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
    #[cfg(feature = "xai")]
    use serde_json::json;
    use tidebreak_core::provider::{
        ChatMessage, ContentBlock, MessageReasoning, ProviderToolReplay, ReasoningOrigin,
        VendorWebSearch,
    };
    use tidebreak_core::{ImageAttachments, Role};

    fn route(kind: RouteKind, key: &str, models: &[&str], base: Option<&str>) -> Route {
        Route {
            kind,
            api_key: key.into(),
            base_url: base.map(str::to_owned),
            curated_models: models.iter().map(|m| (*m).to_string()).collect(),
            model_rewrites: HashMap::new(),
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
                Some("https://compat.example/v1"),
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
    fn ollama_serves_only_configured_models() {
        let router = Router::build(vec![route(
            RouteKind::Ollama,
            "",
            &["qwen3:0.6b"],
            Some("http://127.0.0.1:11434/v1"),
        )]);
        assert_eq!(
            router.select_for(Some(&ProviderId::new("ollama")), "qwen3:0.6b"),
            Some(RouteKind::Ollama)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("ollama")), "llama3.2:1b"),
            None
        );
        assert_eq!(router.select("qwen3:0.6b"), Some(RouteKind::Ollama));
        assert_eq!(router.select("llama3.2:1b"), None);
    }

    #[test]
    fn openrouter_serves_only_configured_models() {
        let router = Router::build(vec![route(
            RouteKind::Openrouter,
            "sk-or",
            &["anthropic/claude-sonnet-4"],
            Some("https://openrouter.ai/api/v1"),
        )]);
        assert_eq!(
            router.select_for(
                Some(&ProviderId::new("openrouter")),
                "anthropic/claude-sonnet-4"
            ),
            Some(RouteKind::Openrouter)
        );
        assert_eq!(
            router.select_for(Some(&ProviderId::new("openrouter")), "openai/gpt-4o"),
            None
        );
        assert_eq!(
            router.select("anthropic/claude-sonnet-4"),
            Some(RouteKind::Openrouter)
        );
        assert_eq!(router.select("openai/gpt-4o"), None);
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

    #[test]
    fn credentialed_routes_reject_cleartext_http() {
        let router = Router::build(vec![route(
            RouteKind::OpenaiCompatible,
            "sk-c",
            &[],
            Some("http://127.0.0.1:1234/v1"),
        )]);
        assert_eq!(router.select("anything"), None);
    }

    #[test]
    fn credentialless_ollama_allows_only_loopback_cleartext() {
        let loopback = Router::build(vec![route(
            RouteKind::Ollama,
            "",
            &["local"],
            Some("http://[::1]:11434/v1"),
        )]);
        assert_eq!(loopback.select("local"), Some(RouteKind::Ollama));

        let lan = Router::build(vec![route(
            RouteKind::Ollama,
            "",
            &["remote"],
            Some("http://192.168.1.10:11434/v1"),
        )]);
        assert_eq!(lan.select("remote"), None);
    }

    struct StaticSource(&'static str);

    #[async_trait]
    impl BearerTokenSource for StaticSource {
        async fn bearer_token(&self) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[derive(Default)]
    struct LiveRouteSource {
        routes: std::sync::Mutex<HashMap<String, String>>,
        request_shaping_models: std::sync::Mutex<HashMap<String, String>>,
        active_leases: Arc<std::sync::atomic::AtomicUsize>,
        lease_calls: std::sync::atomic::AtomicUsize,
        token_calls: std::sync::atomic::AtomicUsize,
    }

    struct ActiveLease(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for ActiveLease {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl BearerTokenSource for LiveRouteSource {
        fn requires_model_route_lease(&self) -> bool {
            true
        }

        async fn lease_model_route(&self, route_model: &str) -> Result<Option<ModelRouteLease>> {
            self.lease_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .routes
                .lock()
                .unwrap()
                .get(route_model)
                .cloned()
                .map(|wire| {
                    self.active_leases
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let request_shaping_model = self
                        .request_shaping_models
                        .lock()
                        .unwrap()
                        .get(route_model)
                        .cloned()
                        .unwrap_or_else(|| wire.clone());
                    ModelRouteLease::with_request_shaping_model(
                        wire,
                        request_shaping_model,
                        ActiveLease(self.active_leases.clone()),
                    )
                }))
        }

        async fn bearer_token(&self) -> Result<String> {
            let call = self
                .token_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            Ok(format!("mg_at_{call}"))
        }
    }

    #[derive(Default)]
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<ChatRequest>>,
    }

    #[async_trait]
    impl ModelProvider for RecordingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("recording")
        }

        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.requests.lock().unwrap().push(req);
            Ok(Box::pin(futures::stream::empty()))
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

    #[tokio::test]
    async fn frozen_selector_is_rewritten_and_an_old_unclaimed_selector_fails_closed() {
        let old_selector = "__tidebreak_gateway_v1.old-deployment.old-install.old-route";
        let new_selector = "__tidebreak_gateway_v1.new-deployment.new-install.new-route";
        let local_wire_model = "deployment-local-opus";
        let gateway = ProviderId::new("model_gateway");
        let recorder = Arc::new(RecordingProvider::default());
        let authority = Arc::new(LiveRouteSource::default());

        let mut old_route = route(RouteKind::ModelGateway, "", &[old_selector], None);
        old_route
            .model_rewrites
            .insert(old_selector.into(), "retired-local-model".into());
        let mut old_router = Router::build(vec![old_route]);
        old_router
            .adapters
            .insert(RouteKind::ModelGateway, recorder.clone());
        assert_eq!(
            old_router.select_for(Some(&gateway), old_selector),
            Some(RouteKind::ModelGateway)
        );

        let mut current_route = route(
            RouteKind::ModelGateway,
            "",
            &[new_selector],
            Some("http://127.0.0.1:9/compat/anthropic"),
        );
        current_route
            .model_rewrites
            .insert(new_selector.into(), local_wire_model.into());
        authority
            .routes
            .lock()
            .unwrap()
            .insert(new_selector.into(), local_wire_model.into());
        current_route.token_source = Some(authority.clone());
        let mut current_router = Router::build(vec![current_route]);
        current_router
            .adapters
            .insert(RouteKind::ModelGateway, recorder.clone());

        let _stream = current_router
            .stream(ChatRequest {
                provider: Some(gateway.clone()),
                model: new_selector.into(),
                ..Default::default()
            })
            .await
            .unwrap();

        {
            let requests = recorder.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].model, new_selector);
            assert_eq!(requests[0].wire_model.as_deref(), Some(local_wire_model));
            assert_eq!(requests[0].provider.as_ref(), Some(&gateway));
        }

        authority.routes.lock().unwrap().remove(new_selector);
        let stale_error = match current_router
            .stream(ChatRequest {
                provider: Some(gateway.clone()),
                model: new_selector.into(),
                ..Default::default()
            })
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("a retained router must revalidate its frozen selector"),
        };
        assert!(stale_error.to_string().contains("no longer serves"));
        assert_eq!(recorder.requests.lock().unwrap().len(), 1);

        let error = match current_router
            .stream(ChatRequest {
                provider: Some(gateway),
                model: old_selector.into(),
                ..Default::default()
            })
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("an unclaimed frozen selector must fail closed"),
        };
        assert!(error.to_string().contains(old_selector));
        assert_eq!(recorder.requests.lock().unwrap().len(), 1);
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
            model_rewrites: HashMap::new(),
            route_authorities: HashMap::new(),
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

    #[tokio::test]
    async fn frozen_gateway_alias_uses_canonical_anthropic_shaping_and_preserves_replay() {
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

        let frozen = "__tidebreak_gateway_v1.deployment.installation.route";
        let wire = "anthropic-us-claude-opus-5";
        let request_shaping_model = "claude-opus-5";
        let gateway = ProviderId::new("model_gateway");
        let authority = Arc::new(LiveRouteSource::default());
        authority
            .routes
            .lock()
            .unwrap()
            .insert(frozen.into(), wire.into());
        authority
            .request_shaping_models
            .lock()
            .unwrap()
            .insert(frozen.into(), request_shaping_model.into());
        let mut route = route(
            RouteKind::ModelGateway,
            "",
            &[frozen],
            Some(&format!("http://{address}/compat/anthropic")),
        );
        route.token_source = Some(authority);
        route.model_rewrites.insert(frozen.into(), wire.into());
        let router = Router::build(vec![route]);

        let mut stream = router
            .stream(ChatRequest {
                provider: Some(gateway.clone()),
                model: frozen.into(),
                reasoning_model: true,
                reasoning_effort: Some(tidebreak_core::model::ReasoningEffort::XHigh),
                vendor_web_search: Some(VendorWebSearch { max_uses: 2 }),
                messages: vec![ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "answer".into(),
                    }],
                    reasoning: MessageReasoning::captured(
                        ReasoningOrigin {
                            provider: Some(gateway),
                            model: frozen.into(),
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
        assert_eq!(body["model"], wire);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "xhigh");
        assert_eq!(body["tools"][0]["type"], "web_search_20260209");
        assert_eq!(body["messages"][0]["content"][0]["type"], "thinking");
    }

    #[tokio::test]
    async fn a_new_frozen_route_flattens_native_tool_replay_from_a_reused_local_id() {
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

        let old_frozen = "__tidebreak_gateway_v1.deployment.installation.old-route";
        let new_frozen = "__tidebreak_gateway_v1.deployment.installation.new-route";
        let reused_wire = "deployment-local-opus";
        let gateway = ProviderId::new("model_gateway");
        let authority = Arc::new(LiveRouteSource::default());
        authority
            .routes
            .lock()
            .unwrap()
            .insert(new_frozen.into(), reused_wire.into());
        let mut route = route(
            RouteKind::ModelGateway,
            "",
            &[new_frozen],
            Some(&format!("http://{address}/compat/anthropic")),
        );
        route.token_source = Some(authority);
        route
            .model_rewrites
            .insert(new_frozen.into(), reused_wire.into());
        let router = Router::build(vec![route]);

        let native = vec![
            serde_json::json!({
                "type": "server_tool_use",
                "id": "srvtoolu_1",
                "name": "web_search",
                "input": {"query": "private old route query"},
            }),
            serde_json::json!({
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_1",
                "content": [{
                    "type": "web_search_result",
                    "url": "https://old.example",
                    "title": "Old route",
                    "encrypted_content": "opaque-old-route",
                }],
            }),
        ];
        let mut stream = router
            .stream(ChatRequest {
                provider: Some(gateway.clone()),
                model: new_frozen.into(),
                messages: vec![ChatMessage {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ProviderExecutedToolCall {
                        name: "web_search".into(),
                        input: serde_json::json!({"query": "private old route query"}),
                        output: serde_json::json!({
                            "results": [{"title": "Old route", "url": "https://old.example"}]
                        }),
                        is_error: false,
                        replay: Some(ProviderToolReplay::captured(
                            ReasoningOrigin {
                                provider: Some(gateway),
                                model: old_frozen.into(),
                            },
                            native,
                        )),
                    }],
                    reasoning: MessageReasoning::default(),
                }],
                ..Default::default()
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}

        let body = rx.await.unwrap();
        assert_eq!(body["model"], reused_wire);
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[tokio::test]
    async fn anthropic_pause_turn_reauthorizes_every_gateway_leg() {
        use axum::body::Body;
        use axum::extract::State;
        use axum::response::IntoResponse;
        use axum::routing::post;

        async fn respond(
            State(requests): State<Arc<std::sync::atomic::AtomicUsize>>,
        ) -> impl IntoResponse {
            let leg = requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let reason = if leg == 1 { "pause_turn" } else { "end_turn" };
            (
                [("content-type", "text/event-stream")],
                Body::from(format!(
                    "data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{reason}\"}}}}\n\n"
                )),
            )
        }

        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new()
            .fallback(post(respond))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let frozen = "__tidebreak_gateway_v1.deployment.installation.route";
        let wire = "claude-opus-5";
        let authority = Arc::new(LiveRouteSource::default());
        authority
            .routes
            .lock()
            .unwrap()
            .insert(frozen.into(), wire.into());
        let mut route = route(
            RouteKind::ModelGateway,
            "",
            &[frozen],
            Some(&format!("http://{address}/compat/anthropic")),
        );
        route.token_source = Some(authority.clone());
        route.model_rewrites.insert(frozen.into(), wire.into());
        let router = Router::build(vec![route]);

        let stream = router
            .stream(ChatRequest {
                provider: Some(ProviderId::new("model_gateway")),
                model: frozen.into(),
                vendor_web_search: Some(VendorWebSearch { max_uses: 1 }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            authority
                .active_leases
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a completed HTTP setup must not serialize unrelated gateway streams"
        );
        assert_eq!(
            authority
                .token_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        let events = stream.collect::<Vec<_>>().await;
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            authority
                .token_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the continuation must mint a fresh bearer"
        );
        assert_eq!(
            authority
                .lease_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            3,
            "the router and both HTTP legs must validate the frozen selector"
        );
        assert!(matches!(events.last(), Some(ProviderEvent::Stop { .. })));
        assert_eq!(
            authority
                .active_leases
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "each request-scoped lease is released after that HTTP leg is dispatched"
        );
    }

    #[tokio::test]
    async fn anthropic_pause_turn_refuses_a_revoked_gateway_continuation() {
        use axum::body::Body;
        use axum::extract::State;
        use axum::response::IntoResponse;
        use axum::routing::post;

        async fn respond(
            State(requests): State<Arc<std::sync::atomic::AtomicUsize>>,
        ) -> impl IntoResponse {
            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                [("content-type", "text/event-stream")],
                Body::from(
                    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"pause_turn\"}}\n\n",
                ),
            )
        }

        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app = axum::Router::new()
            .fallback(post(respond))
            .with_state(requests.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let frozen = "__tidebreak_gateway_v1.deployment.installation.route";
        let wire = "claude-opus-5";
        let authority = Arc::new(LiveRouteSource::default());
        authority
            .routes
            .lock()
            .unwrap()
            .insert(frozen.into(), wire.into());
        let mut route = route(
            RouteKind::ModelGateway,
            "",
            &[frozen],
            Some(&format!("http://{address}/compat/anthropic")),
        );
        route.token_source = Some(authority.clone());
        route.model_rewrites.insert(frozen.into(), wire.into());
        let router = Router::build(vec![route]);

        let stream = router
            .stream(ChatRequest {
                provider: Some(ProviderId::new("model_gateway")),
                model: frozen.into(),
                vendor_web_search: Some(VendorWebSearch { max_uses: 1 }),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        authority.routes.lock().unwrap().remove(frozen);

        let events = stream.collect::<Vec<_>>().await;
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "revocation before continuation must prevent its HTTP request"
        );
        assert!(matches!(events.last(), Some(ProviderEvent::Failed { .. })));
        assert_eq!(
            authority
                .token_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each continuation mints first, then performs the final live route validation"
        );
    }
}
