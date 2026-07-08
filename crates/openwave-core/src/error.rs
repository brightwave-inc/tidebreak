//! The crate-wide error type.
//!
//! [`AgentError`] is intentionally lean; variants are added as the code that
//! produces them lands (provider, tool, store, agent-loop). It is
//! `#[non_exhaustive]` so growing it is never a breaking change for downstream
//! matches.

use serde::{Deserialize, Serialize};
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

    /// A persistence failure from the `Store` / `BlobStore`.
    #[error("store error: {0}")]
    Store(String),

    /// A failure reaching the secret store (keychain / KMS).
    #[error("secret error: {0}")]
    Secret(String),

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

    /// A short, stable machine-readable category for this error.
    ///
    /// Used to tag the serializable [`AgentErrorInfo`] that rides the event
    /// stream, so clients can branch on the kind without parsing the message.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Config(_) => "config",
            Self::Store(_) => "store",
            Self::Secret(_) => "secret",
            Self::Message(_) => "message",
            Self::Serde(_) => "serde",
        }
    }

    /// Convert into the serializable form carried by `AgentEvent::TurnFailed`.
    pub fn to_info(&self) -> AgentErrorInfo {
        AgentErrorInfo {
            kind: self.kind().to_string(),
            message: self.to_string(),
        }
    }
}

/// The wire-facing representation of an error.
///
/// [`AgentError`] itself is not `Serialize` (it wraps non-serializable source
/// errors like [`serde_json::Error`]), but the `AgentEvent` stream that clients
/// consume must serialize. `AgentErrorInfo` is that serializable projection: a
/// stable `kind` plus a human-readable `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentErrorInfo {
    /// Machine-readable category (see [`AgentError::kind`]).
    pub kind: String,
    /// Human-readable description.
    pub message: String,
}

impl From<&AgentError> for AgentErrorInfo {
    fn from(err: &AgentError) -> Self {
        err.to_info()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_carries_kind_and_message() {
        let info = AgentError::config("bad path").to_info();
        assert_eq!(info.kind, "config");
        assert_eq!(info.message, "configuration error: bad path");
    }

    #[test]
    fn info_is_serializable_and_roundtrips() {
        let info: AgentErrorInfo = (&AgentError::msg("boom")).into();
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<AgentErrorInfo>(&json).unwrap(), info);
    }
}
