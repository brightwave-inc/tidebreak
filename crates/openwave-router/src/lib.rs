//! OpenWave model routing.
//!
//! Home of the concrete [`ModelProvider`](openwave_core::ModelProvider) adapters
//! (and, later, the composite `Router` that does config-gated selection and
//! health-based failover). Each adapter's job is to normalize one provider's
//! streaming quirks into the shared
//! [`ProviderEvent`](openwave_core::ProviderEvent) vocabulary.
//!
//! The `ModelProvider` contract itself lives in `openwave-core`; only the
//! implementations live here.

#[cfg(feature = "anthropic")]
pub mod anthropic;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
