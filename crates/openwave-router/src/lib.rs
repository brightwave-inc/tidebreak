//! OpenWave model routing.
//!
//! Home of the concrete [`ModelProvider`](openwave_core::ModelProvider) adapters
//! and the composite [`Router`] that does config-gated model→provider selection.
//! Each adapter's job is to normalize one provider's streaming quirks into the
//! shared [`ProviderEvent`](openwave_core::ProviderEvent) vocabulary.
//!
//! The `ModelProvider` contract itself lives in `openwave-core`; only the
//! implementations live here. Health-based failover is a later slice.

pub mod router;
mod sse;

#[cfg(feature = "anthropic")]
pub mod anthropic;

#[cfg(feature = "openai-compat")]
pub mod openai_compat;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "gemini")]
pub mod google_auth;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "gemini")]
pub use gemini::GeminiProvider;
#[cfg(feature = "gemini")]
pub use google_auth::{
    valid_resource_segment as valid_google_resource_segment, valid_vertex_location,
    GoogleServiceAccount, GoogleServiceAccountTokenSource,
};
#[cfg(feature = "openai-compat")]
pub use openai_compat::OpenAiCompatProvider;
pub use router::{BearerTokenSource, Route, RouteKind, Router, VertexRoute};
