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
}

/// Vertex-specific route data. The credential fingerprint is already an
/// irreversible digest and exists only to invalidate a cached router after a
/// service-account key rotates.
#[derive(Clone)]
pub struct VertexRoute {
    project_id: String,
    location: String,
    credential_fingerprint: [u8; 32],
}

impl VertexRoute {
    /// Build Vertex routing metadata.
    pub fn new(
        project_id: impl Into<String>,
        location: impl Into<String>,
        credential_fingerprint: [u8; 32],
    ) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            credential_fingerprint,
        }
    }
}

impl std::fmt::Debug for VertexRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VertexRoute")
            .field("project_id", &"***")
            .field("location", &self.location)
            .field("credential_fingerprint", &"***")
            .finish()
    }
}

/// Which concrete adapter a [`Route`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RouteKind {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions (api.openai.com).
    Openai,
    /// Any OpenAI-compatible Chat Completions gateway.
    OpenaiCompatible,
    /// Google Gemini Developer API (native GenerateContent protocol).
    Gemini,
    /// A model-gateway deployment's Anthropic-compatible surface, authenticated
    /// with short-lived resource-scoped tokens instead of a static key.
    ModelGateway,
}

impl RouteKind {
    /// Stable id string matching the server's provider kind wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Anthropic => "anthropic",
            RouteKind::Openai => "openai",
            RouteKind::OpenaiCompatible => "openai_compatible",
            RouteKind::Gemini => "gemini",
            RouteKind::ModelGateway => "model_gateway",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
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
    /// Vertex resource/auth metadata. Present only for a Gemini route backed
    /// by a Google service account.
    pub vertex: Option<VertexRoute>,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("kind", &self.kind)
            .field("api_key", &"***")
            .field("base_url", &self.base_url)
            .field("curated_models", &self.curated_models)
            .field("token_source", &self.token_source.is_some())
            .field("vertex", &self.vertex)
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
                curated.insert(format!("{}::{model}", route.kind.as_str()), route.kind);
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
            RouteKind::Gemini,
            RouteKind::ModelGateway,
            RouteKind::OpenaiCompatible,
        ] {
            if self
                .curated
                .contains_key(&format!("{}::{model}", kind.as_str()))
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
        // The route hint is host policy, not provider wire data.
        req.provider = None;
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
                    .with_token_source(source),
            ))
        }
        #[cfg(not(feature = "anthropic"))]
        RouteKind::ModelGateway => None,

        #[cfg(feature = "openai-compat")]
        RouteKind::Openai => {
            let mut p = OpenAiCompatProvider::new(route.api_key.clone());
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            Some(Arc::new(p))
        }
        #[cfg(feature = "openai-compat")]
        RouteKind::OpenaiCompatible => {
            let base = route.base_url.as_deref()?;
            // Refuse non-http(s) schemes so a stored base_url can't point the
            // adapter at an arbitrary scheme handler.
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                return None;
            }
            Some(Arc::new(OpenAiCompatProvider::compatible(
                route.api_key.clone(),
                base.to_string(),
            )))
        }
        #[cfg(feature = "gemini")]
        RouteKind::Gemini => {
            let mut p = match &route.vertex {
                Some(vertex) => GeminiProvider::vertex(
                    vertex.project_id.clone(),
                    vertex.location.clone(),
                    route.token_source.clone()?,
                )
                .ok()?,
                None => {
                    if route.api_key.is_empty() || route.token_source.is_some() {
                        return None;
                    }
                    GeminiProvider::new(route.api_key.clone())
                }
            };
            if let Some(base) = &route.base_url {
                p = p.with_base_url(base.clone());
            }
            Some(Arc::new(p))
        }
        #[cfg(not(feature = "gemini"))]
        RouteKind::Gemini => None,
        #[cfg(not(feature = "openai-compat"))]
        RouteKind::Openai | RouteKind::OpenaiCompatible => None,
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
                r.vertex.as_ref().map_or_else(String::new, |vertex| {
                    let credential = vertex
                        .credential_fingerprint
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    format!("{}:{credential}", vertex.location)
                })
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
    use openwave_core::ImageAttachments;

    fn route(kind: RouteKind, key: &str, models: &[&str], base: Option<&str>) -> Route {
        Route {
            kind,
            api_key: key.into(),
            base_url: base.map(str::to_owned),
            curated_models: models.iter().map(|m| (*m).to_string()).collect(),
            token_source: None,
            vertex: None,
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
    fn explicit_provider_never_cross_routes_an_ambiguous_model() {
        let router = Router::build(vec![
            route(RouteKind::Anthropic, "sk-a", &["shared"], None),
            route(RouteKind::Openai, "sk-o", &["shared"], None),
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
    fn vertex_fingerprint_changes_with_location_or_service_account() {
        let build = |location: &str, credential: u8| {
            let mut route = route(RouteKind::Gemini, "", &["gemini-3.6-flash"], None);
            route.token_source = Some(Arc::new(StaticSource("vertex-token")));
            route.vertex = Some(VertexRoute::new("test-project", location, [credential; 32]));
            Router::build(vec![route])
        };

        assert_ne!(
            build("global", 1).fingerprint(),
            build("us-central1", 1).fingerprint()
        );
        assert_ne!(
            build("global", 1).fingerprint(),
            build("global", 2).fingerprint()
        );
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
            })
            .await;
        match result {
            Err(err) => assert!(err.to_string().contains("unavailable")),
            Ok(_) => panic!("expected fail-closed error"),
        }
    }
}
