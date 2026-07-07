//! The crate-wide error type.
//!
//! [`AgentError`] is intentionally lean; variants are added as the code that
//! produces them lands (provider, tool, store, agent-loop). It is
//! `#[non_exhaustive]` so growing it is never a breaking change for downstream
//! matches.

use thiserror::Error;

/// Shorthand for results across the core crate.
pub type Result<T, E = AgentError> = std::result::Result<T, E>;

/// Anything that can go wrong inside the core.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentError {
    /// A configuration or setup problem (bad path, missing setting).
    #[error("configuration error: {0}")]
    Config(String),

    /// A catch-all for contexts that do not yet warrant their own variant.
    #[error("{0}")]
    Message(String),

    /// JSON (de)serialization failure.
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl AgentError {
    /// Build an [`AgentError::Message`] from anything string-like.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }

    /// Build an [`AgentError::Config`] from anything string-like.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }
}
