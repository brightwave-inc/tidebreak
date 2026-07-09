//! A fail-closed provider for when no model credentials are configured.

use async_trait::async_trait;
use futures::stream::BoxStream;

use openwave_core::{AgentError, ChatRequest, ModelProvider, ProviderEvent, ProviderId, Result};

/// Stands in when no provider is configured (e.g. no API key present).
///
/// Its `stream` returns an error immediately, **without opening any connection or
/// sending the request**, so a turn fails closed — the transcript never leaves the
/// machine — instead of egressing to a provider with an empty key and failing only
/// after the round-trip.
pub struct UnconfiguredProvider;

#[async_trait]
impl ModelProvider for UnconfiguredProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("unconfigured")
    }

    async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
        Err(AgentError::config(
            "no model provider is configured (set ANTHROPIC_API_KEY)",
        ))
    }
}
