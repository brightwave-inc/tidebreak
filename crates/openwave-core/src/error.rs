//! The crate-wide error type.
//!
//! [`AgentError`] is intentionally lean; variants are added as the code that
//! produces them lands (provider, tool, store, agent-loop). It is
//! `#[non_exhaustive]` so growing it is never a breaking change for downstream
//! matches.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ProjectId;

/// Shorthand for results across the core crate.
pub type Result<T, E = AgentError> = std::result::Result<T, E>;

/// A client-safe provider failure plus the wait the provider asked for.
///
/// `retry_after` carries the response's `Retry-After` value when one was
/// present. It is a hint from the provider about when the condition clears —
/// the retry scheduler prefers it over its own blind backoff, because guessing
/// shorter turns a rate limit into a wasted attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    message: String,
    retry_after: Option<Duration>,
}

impl ProviderFailure {
    /// Build a failure carrying the provider's own retry hint.
    #[must_use]
    pub fn new(message: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self {
            message: message.into(),
            retry_after,
        }
    }

    /// The wait the provider asked for, when the response carried one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl<T: Into<String>> From<T> for ProviderFailure {
    fn from(message: T) -> Self {
        Self::new(message, None)
    }
}

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

    /// A project-scoped atomic write lost a concurrent project deletion race.
    #[error("project {0} not found")]
    ProjectNotFound(ProjectId),

    /// A failure reaching the secret store (keychain / KMS).
    #[error("secret error: {0}")]
    Secret(String),

    /// A failure from a model provider (network, HTTP status, malformed stream).
    #[error("provider error: {0}")]
    Provider(String),

    /// The provider rejected the configured credential.
    #[error("provider authentication failed: {0}")]
    Authentication(String),

    /// The provider is rate limiting requests.
    #[error("provider rate limited the request: {0}")]
    RateLimited(ProviderFailure),

    /// The provider is temporarily overloaded or unavailable.
    #[error("provider overloaded: {0}")]
    Overloaded(ProviderFailure),

    /// The provider rejected a request that is invalid for the selected model.
    #[error("invalid provider request: {0}")]
    InvalidRequest(String),

    /// The provider refused the requested content.
    #[error("provider refused the request: {0}")]
    Refusal(String),

    /// The prompt exceeded the model's context window. The agent loop retries
    /// with a tighter context budget before giving up.
    #[error("prompt too long: {0}")]
    PromptTooLong(String),

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
            Self::ProjectNotFound(_) => "not_found",
            Self::Secret(_) => "secret",
            Self::Provider(_) => "provider",
            Self::Authentication(_) => "authentication",
            Self::RateLimited(_) => "rate_limited",
            Self::Overloaded(_) => "overloaded",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Refusal(_) => "refusal",
            Self::PromptTooLong(_) => "prompt_too_long",
            Self::Message(_) => "message",
            Self::Serde(_) => "serde",
        }
    }

    /// The wait the provider asked for, when this failure carried one.
    ///
    /// Only the throttling variants can carry a hint; every other failure
    /// leaves the schedule to the caller's own backoff.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited(failure) | Self::Overloaded(failure) => failure.retry_after(),
            _ => None,
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
