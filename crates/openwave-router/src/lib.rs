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

#[cfg(feature = "_http")]
mod google;

#[cfg(feature = "_http")]
pub mod http;

#[cfg(feature = "anthropic")]
pub mod anthropic;

#[cfg(feature = "openai-compat")]
pub mod openai_compat;

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "xai")]
pub mod xai;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "gemini")]
pub mod google_auth;

#[cfg(all(feature = "anthropic", feature = "gemini"))]
pub mod vertex;

#[cfg(feature = "bedrock")]
pub mod bedrock;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "bedrock")]
pub use bedrock::{valid_aws_region, BedrockAuth, BedrockProvider};
#[cfg(feature = "gemini")]
pub use gemini::GeminiProvider;
#[cfg(feature = "_http")]
pub use google::{valid_resource_segment as valid_google_resource_segment, valid_vertex_location};
#[cfg(feature = "gemini")]
pub use google_auth::{GoogleServiceAccount, GoogleServiceAccountTokenSource};
#[cfg(feature = "openai")]
pub use openai::OpenAiProvider;
#[cfg(feature = "openai-compat")]
pub use openai_compat::OpenAiCompatProvider;
pub use router::{
    AwsCredentials, BearerTokenSource, BedrockRoute, Route, RouteKind, Router, VertexModelFamily,
    VertexRoute,
};
#[cfg(all(feature = "anthropic", feature = "gemini"))]
pub use vertex::VertexProvider;
#[cfg(feature = "xai")]
pub use xai::XaiProvider;
