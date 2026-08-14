//! Tidebreak model routing.
//!
//! Home of the concrete [`ModelProvider`](tidebreak_core::ModelProvider) adapters
//! and the composite [`Router`] that does config-gated model→provider selection.
//! Each adapter's job is to normalize one provider's streaming quirks into the
//! shared [`ProviderEvent`](tidebreak_core::ProviderEvent) vocabulary.
//!
//! The `ModelProvider` contract itself lives in `tidebreak-core`; only the
//! implementations live here. Health-based failover is a later slice.

pub mod router;
mod sse;

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

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "gemini")]
pub use gemini::GeminiProvider;
#[cfg(feature = "openai")]
pub use openai::OpenAiProvider;
#[cfg(feature = "openai-compat")]
pub use openai_compat::OpenAiCompatProvider;
pub use router::{BearerTokenSource, ModelRouteLease, Route, RouteKind, Router};
#[cfg(feature = "xai")]
pub use xai::XaiProvider;
