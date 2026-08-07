//! First-class Google Vertex AI provider.
//!
//! Vertex serves two native protocol families from one Google Cloud identity:
//! Gemini uses GenerateContent under the `google` publisher, while Claude uses
//! Anthropic Messages through `streamRawPredict`. This adapter keeps that
//! distinction explicit and refuses every model that the host did not curate.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

use openwave_core::error::{AgentError, Result};
use openwave_core::provider::{ChatRequest, ModelProvider, ProviderEvent, ProviderId};

use crate::{AnthropicProvider, BearerTokenSource, GeminiProvider, VertexModelFamily};

/// A provider that dispatches only its curated model ids to the matching
/// native Vertex protocol adapter.
#[derive(Clone)]
pub struct VertexProvider {
    models: HashMap<String, VertexModelFamily>,
    gemini: GeminiProvider,
    anthropic: AnthropicProvider,
}

impl VertexProvider {
    /// Build a global Vertex provider from validated resource identity and an
    /// explicit model-family map. The bearer source is shared so both
    /// protocols use the same serialized, expiry-aware Google token cache.
    pub fn new(
        project_id: impl Into<String>,
        location: impl Into<String>,
        token_source: Arc<dyn BearerTokenSource>,
        models: impl IntoIterator<Item = (String, VertexModelFamily)>,
    ) -> Result<Self> {
        let project_id = project_id.into();
        let location = location.into();
        if location != "global" {
            return Err(AgentError::config(
                "first-class Vertex AI routes require the global location",
            ));
        }
        let mut mapped = HashMap::new();
        for (model, family) in models {
            if mapped.insert(model, family).is_some() {
                return Err(AgentError::config("duplicate model id in Vertex AI route"));
            }
        }
        if mapped.is_empty() {
            return Err(AgentError::config("Vertex AI route has no curated models"));
        }
        Ok(Self {
            models: mapped,
            gemini: GeminiProvider::vertex(
                project_id.clone(),
                location.clone(),
                token_source.clone(),
            )?,
            anthropic: AnthropicProvider::vertex(project_id, location, token_source)?,
        })
    }

    /// Override both derived hosts for a controlled local fixture server.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        self.gemini = self.gemini.with_base_url(base_url.clone());
        self.anthropic = self.anthropic.with_base_url(base_url);
        self
    }
}

#[async_trait]
impl ModelProvider for VertexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("vertex")
    }

    async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        match self.models.get(&req.model) {
            Some(VertexModelFamily::Gemini) => self.gemini.stream(req).await,
            Some(VertexModelFamily::Anthropic) => self.anthropic.stream(req).await,
            None => Err(AgentError::config(format!(
                "Vertex AI cannot serve uncurated model `{}`",
                req.model
            ))),
        }
    }
}
