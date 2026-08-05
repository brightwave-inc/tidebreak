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

use crate::id::OutputId;
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

    /// No credential is configured for the selected model provider.
    #[error("provider credential missing: {0}")]
    MissingCredential(String),

    /// A persistence failure from the `Store` / `BlobStore`.
    #[error("store error: {0}")]
    Store(String),

    /// A project-scoped atomic write lost a concurrent project deletion race.
    #[error("project {0} not found")]
    ProjectNotFound(ProjectId),

    /// An output creation lost the race for a filename: another live output in
    /// the same conversation already carries it. The winner's id is carried so
    /// the loser can address the same document instead of forking it.
    #[error("output filename `{filename}` is already live in this conversation as {output_id}")]
    OutputFilenameTaken {
        /// The contested display filename.
        filename: String,
        /// The live output that already holds the name.
        output_id: OutputId,
    },

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
            Self::MissingCredential(_) => "missing_credential",
            Self::Store(_) => "store",
            Self::ProjectNotFound(_) => "not_found",
            Self::OutputFilenameTaken { .. } => "conflict",
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

/// The stream-carrying form of a classified provider failure.
///
/// [`AgentError`] is not `Serialize`, but [`crate::provider::ProviderEvent`]
/// is, and its `Failed` variant must carry the classification through the
/// event stream so a mid-stream failure reaches the client under the same kind
/// as the equivalent HTTP-status failure. This is that projection: the `kind`
/// the client ultimately branches on, plus the client-safe message *without*
/// the variant's `Display` prefix, so [`ProviderErrorInfo::into_agent_error`]
/// rebuilds the classified error instead of doubling the prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderErrorInfo {
    /// Machine-readable category — one of the provider-failure
    /// [`AgentError::kind`] values.
    pub kind: String,
    /// Client-safe description, free of provider payload text and URLs.
    pub message: String,
}

impl ProviderErrorInfo {
    /// Project a provider failure into its stream-carrying form.
    pub fn from_error(error: &AgentError) -> Self {
        let message = match error {
            AgentError::Provider(message)
            | AgentError::Authentication(message)
            | AgentError::InvalidRequest(message)
            | AgentError::Refusal(message)
            | AgentError::PromptTooLong(message) => message.clone(),
            AgentError::RateLimited(failure) | AgentError::Overloaded(failure) => {
                failure.to_string()
            }
            other => other.to_string(),
        };
        Self {
            kind: error.kind().to_string(),
            message,
        }
    }

    /// A plain `provider`-kind failure with a client-safe message.
    pub fn provider(message: impl Into<String>) -> Self {
        Self {
            kind: "provider".to_string(),
            message: message.into(),
        }
    }

    /// Rebuild the classified error for the consumer that fails the turn.
    ///
    /// A mid-stream failure carries no `Retry-After` header, so the throttling
    /// variants rebuild without a hint and the retry schedule falls back to
    /// its own backoff.
    pub fn into_agent_error(self) -> AgentError {
        match self.kind.as_str() {
            "authentication" => AgentError::Authentication(self.message),
            "rate_limited" => AgentError::RateLimited(self.message.into()),
            "overloaded" => AgentError::Overloaded(self.message.into()),
            "invalid_request" => AgentError::InvalidRequest(self.message),
            "refusal" => AgentError::Refusal(self.message),
            "prompt_too_long" => AgentError::PromptTooLong(self.message),
            _ => AgentError::Provider(self.message),
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

    #[test]
    fn provider_error_info_roundtrips_the_classification() {
        for (error, kind) in [
            (AgentError::Provider("p".into()), "provider"),
            (AgentError::Authentication("a".into()), "authentication"),
            (AgentError::RateLimited("r".into()), "rate_limited"),
            (AgentError::Overloaded("o".into()), "overloaded"),
            (AgentError::InvalidRequest("i".into()), "invalid_request"),
            (AgentError::Refusal("no".into()), "refusal"),
            (AgentError::PromptTooLong("long".into()), "prompt_too_long"),
        ] {
            let info = ProviderErrorInfo::from_error(&error);
            assert_eq!(info.kind, kind);
            let rebuilt = info.into_agent_error();
            assert_eq!(rebuilt.kind(), kind);
            assert_eq!(rebuilt.to_string(), error.to_string());
        }
        // The throttling variants rebuild without a retry hint: a mid-stream
        // failure never saw the response's headers.
        assert!(
            AgentError::RateLimited(ProviderFailure::new("r", Some(Duration::from_secs(5))))
                .retry_after()
                .is_some()
        );
        assert!(
            ProviderErrorInfo::from_error(&AgentError::RateLimited(ProviderFailure::new(
                "r",
                Some(Duration::from_secs(5))
            )))
            .into_agent_error()
            .retry_after()
            .is_none()
        );
    }
}
