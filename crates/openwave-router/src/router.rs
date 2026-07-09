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
#[cfg(feature = "openai-compat")]
use crate::OpenAiCompatProvider;

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
}

impl RouteKind {
    /// Stable id string matching the server's provider kind wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Anthropic => "anthropic",
            RouteKind::Openai => "openai",
            RouteKind::OpenaiCompatible => "openai_compatible",
        }
    }
}

/// One enabled, credentialed provider endpoint the router may select.
#[derive(Debug, Clone)]
pub struct Route {
    /// Which adapter to build.
    pub kind: RouteKind,
    /// API key / bearer token.
    pub api_key: String,
    /// Optional base URL override (required in practice for `OpenaiCompatible`).
    pub base_url: Option<String>,
    /// Curated model ids this route claims. Used for preferential selection;
    /// `OpenaiCompatible` typically passes an empty list (free-form fallback).
    pub curated_models: Vec<String>,
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
    /// model id → preferred route kind (first curated claim wins).
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
                curated.entry(model.clone()).or_insert(route.kind);
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
        if let Some(kind) = self.curated.get(model) {
            if self.adapters.contains_key(kind) {
                return Some(*kind);
            }
        }
        if self.has_openai_compat && self.adapters.contains_key(&RouteKind::OpenaiCompatible) {
            return Some(RouteKind::OpenaiCompatible);
        }
        None
    }
}

#[async_trait]
impl ModelProvider for Router {
    fn id(&self) -> ProviderId {
        ProviderId::new("router")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        let Some(kind) = self.select(&req.model) else {
            return Err(AgentError::config(format!(
                "no enabled provider can serve model `{}`",
                req.model
            )));
        };
        let Some(adapter) = self.adapters.get(&kind) else {
            return Err(AgentError::config(format!(
                "no enabled provider can serve model `{}`",
                req.model
            )));
        };
        adapter.stream(req).await
    }
}

fn build_adapter(route: &Route) -> Option<Arc<dyn ModelProvider>> {
    if route.api_key.is_empty() {
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
        #[cfg(not(feature = "openai-compat"))]
        RouteKind::Openai | RouteKind::OpenaiCompatible => None,
    }
}

fn fingerprint_routes(routes: &[Route]) -> String {
    let mut parts: Vec<String> = routes
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}",
                r.kind.as_str(),
                r.api_key,
                r.base_url.as_deref().unwrap_or("")
            )
        })
        .collect();
    parts.sort();
    parts.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(kind: RouteKind, key: &str, models: &[&str], base: Option<&str>) -> Route {
        Route {
            kind,
            api_key: key.into(),
            base_url: base.map(str::to_owned),
            curated_models: models.iter().map(|m| (*m).to_string()).collect(),
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

    #[test]
    fn fingerprint_changes_with_key() {
        let a = Router::build(vec![route(RouteKind::Anthropic, "sk-1", &[], None)]);
        let b = Router::build(vec![route(RouteKind::Anthropic, "sk-2", &[], None)]);
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[tokio::test]
    async fn stream_fails_closed_with_no_match() {
        let router = Router::build(vec![]);
        let result = router
            .stream(ChatRequest {
                model: "nope".into(),
                system: None,
                messages: vec![],
                tools: vec![],
                max_tokens: None,
                temperature: None,
            })
            .await;
        match result {
            Err(err) => assert!(err.to_string().contains("no enabled provider")),
            Ok(_) => panic!("expected fail-closed error"),
        }
    }
}
