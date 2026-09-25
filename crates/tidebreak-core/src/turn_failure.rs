//! Why a turn failed, in the one vocabulary chat and code turns share.
//!
//! A failure's internal kind ([`crate::AgentError::kind`], and the codes the
//! turn worker and the engine adapters record) is a diagnostic vocabulary. It
//! grows with the server, and its message can carry provider payloads and
//! host paths. A reader needs something narrower: what went wrong, what to do
//! about it, and whether running the same turn again could plausibly help.
//! [`TurnFailureCategory`] is exactly that. Nothing earns a variant unless a
//! client would say or do something different for it.
//!
//! Whether a retry can help is part of each category's meaning, not a separate
//! flag ([`TurnFailureCategory::retries_may_succeed`]). The turn worker reads
//! the same answer to decide whether to reschedule a failed turn, so the
//! category a client sees and the one the scheduler acted on cannot drift
//! apart.
//!
//! [`TurnFailure`] pairs a category with the facts its copy needs: the
//! engine, the model, and when a usage limit resets. They are fields, never
//! prose, so the renderer owns every sentence it shows.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use ts_rs::TS;

use crate::code::HarnessKind;

/// Why a turn failed, closed and coarse enough to be stable.
///
/// Clients built before the full vocabulary read only `rate_limited`, `auth`,
/// `provider_access`, `transient`, and `unknown`, and nothing else is ever
/// sent to them; [`Self::legacy`] maps every category onto that set.
///
/// The vocabulary may grow. A reader built from this definition reads a
/// category it does not know as [`Self::Unknown`] instead of failing, so a
/// failure from a newer server still arrives as a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureCategory {
    /// The provider or engine throttled this account's requests. Waiting and
    /// asking again is the remedy.
    RateLimited,
    /// The provider is overloaded or briefly unavailable for everyone.
    /// Waiting and asking again is the remedy.
    Overloaded,
    /// The provider rejected Tidebreak's credential, or none is configured.
    /// Only fixing the credential changes the outcome.
    Auth,
    /// The provider account or organization denied the request: credits,
    /// billing, policy, entitlement, or key permissions.
    ProviderAccess,
    /// The model is retired, unknown to the provider, or not served to this
    /// account, and the provider said so. Choosing another model is the
    /// remedy.
    ModelUnavailable,
    /// The provider's address answered that nothing is there, without naming
    /// a model: the configured base URL, or a proxy in front of it, is
    /// wrong. Fixing the provider's configuration is the remedy.
    EndpointNotFound,
    /// The conversation no longer fits the model's context window, after
    /// Tidebreak's own reductions ran out.
    ContextOverflow,
    /// The provider or engine rejected the request itself as invalid or
    /// against policy. Sending it again gets the same answer.
    RequestRejected,
    /// Tidebreak could not read its own data on this computer: its database,
    /// its saved credentials, or the disk.
    Local,
    /// A network or upstream fault that may clear on its own.
    Transient,
    /// The coding engine is not signed in, or its sign-in no longer works.
    EngineAuth,
    /// The coding engine's plan or credit limit is spent. It lifts at the
    /// reset time when the engine reported one.
    UsageLimit,
    /// Everything else: budgets the turn exceeded, malformed agent output,
    /// internal invariants, and any category this build does not know. A
    /// client should not promise that a retry helps.
    #[serde(other)]
    Unknown,
}

impl TurnFailureCategory {
    /// Every category, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::RateLimited,
        Self::Overloaded,
        Self::Auth,
        Self::ProviderAccess,
        Self::ModelUnavailable,
        Self::EndpointNotFound,
        Self::ContextOverflow,
        Self::RequestRejected,
        Self::Local,
        Self::Transient,
        Self::EngineAuth,
        Self::UsageLimit,
        Self::Unknown,
    ];

    /// Classify a failure kind from the internal vocabulary.
    ///
    /// Unrecognized kinds fall to [`Self::Unknown`], so a new internal
    /// failure code reads as coarse rather than wrong. A store or secret
    /// failure reads as [`Self::Transient`] here, because only its message
    /// can say whether it was a definite local fault; prefer
    /// [`Self::from_failure`] wherever the message is at hand.
    #[must_use]
    pub fn from_kind(kind: &str) -> Self {
        match kind {
            "rate_limited" => Self::RateLimited,
            "overloaded" => Self::Overloaded,
            "authentication" | "missing_credential" => Self::Auth,
            "access_denied" => Self::ProviderAccess,
            // `unknown_model`: the turn's model left the registry.
            // `model_provider_unavailable`: a managed gateway cannot run an
            // unpinned model alias.
            "model_unavailable" | "unknown_model" | "model_provider_unavailable" => {
                Self::ModelUnavailable
            }
            "endpoint_not_found" => Self::EndpointNotFound,
            // The agent loop surfaces this only after its own context
            // reductions ran out.
            "prompt_too_long" => Self::ContextOverflow,
            "invalid_request" | "refusal" => Self::RequestRejected,
            "store" | "secret" | "provider" | "empty_model_response" => Self::Transient,
            "engine_auth" => Self::EngineAuth,
            "usage_limit" => Self::UsageLimit,
            _ => Self::Unknown,
        }
    }

    /// Classify a failure from its kind and its message.
    ///
    /// The message matters only for Tidebreak's own store and secret
    /// failures. A timeout, a busy or locked database, or an unreachable
    /// secret service may clear on its own, so those stay
    /// [`Self::Transient`] and the turn worker retries them. Only a definite
    /// local fault — a full disk, a damaged or unopenable database, a
    /// credential store the platform refused — is [`Self::Local`], which no
    /// retry fixes.
    #[must_use]
    pub fn from_failure(kind: &str, message: &str) -> Self {
        match kind {
            "store" | "secret" if definite_local_fault(message) => Self::Local,
            _ => Self::from_kind(kind),
        }
    }

    /// Whether running the same turn again, unchanged, could plausibly
    /// succeed.
    ///
    /// The turn worker retries only these, and a client offers a plain retry
    /// only for these. Every other category needs something to change first:
    /// a credential, a model, the conversation, or time a retry cannot wait
    /// out.
    #[must_use]
    pub const fn retries_may_succeed(self) -> bool {
        matches!(self, Self::RateLimited | Self::Overloaded | Self::Transient)
    }

    /// The category a client built before the full vocabulary reads.
    ///
    /// Those clients know five categories and cannot render any other value,
    /// so a field they read never carries anything else. Each category maps
    /// to the nearest one whose advice still holds, and to `unknown` where
    /// none does.
    #[must_use]
    pub const fn legacy(self) -> Self {
        match self {
            Self::RateLimited | Self::Overloaded | Self::UsageLimit => Self::RateLimited,
            Self::Auth | Self::EngineAuth => Self::Auth,
            Self::ProviderAccess => Self::ProviderAccess,
            Self::Transient => Self::Transient,
            Self::ModelUnavailable
            | Self::EndpointNotFound
            | Self::ContextOverflow
            | Self::RequestRejected
            | Self::Local
            | Self::Unknown => Self::Unknown,
        }
    }

    /// The wire spelling, for a client that prints the category as text.
    /// Pinned to the serde rendering by a test.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Overloaded => "overloaded",
            Self::Auth => "auth",
            Self::ProviderAccess => "provider_access",
            Self::ModelUnavailable => "model_unavailable",
            Self::EndpointNotFound => "endpoint_not_found",
            Self::ContextOverflow => "context_overflow",
            Self::RequestRejected => "request_rejected",
            Self::Local => "local",
            Self::Transient => "transient",
            Self::EngineAuth => "engine_auth",
            Self::UsageLimit => "usage_limit",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether a store or secret failure's words name a fault on the machine
/// Tidebreak runs on that waiting will not clear.
///
/// Each phrase is the source's own wording: SQLite's messages for a full
/// disk, a damaged file, a file that is not a database, a read-only
/// database, and one it cannot open; the operating system's for a full
/// device; and the `keyring` crate's for a credential store the platform
/// refused (a denied keychain prompt reads as a platform failure).
fn definite_local_fault(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "database or disk is full",
        "no space left on device",
        "database disk image is malformed",
        "file is not a database",
        "attempt to write a readonly database",
        "unable to open database file",
        "couldn't access platform secure storage",
        "platform secure storage failure",
    ]
    .iter()
    .any(|phrase| message.contains(phrase))
}

/// Whether a provider's or engine's words say the requested model is what
/// was not found, as opposed to the address the request went to.
///
/// A 404 alone cannot tell a retired model from a wrong base URL or a proxy
/// that answers for nothing. Each phrase here is one a provider or engine
/// sends about the model itself: Anthropic's `model: <name>`, the Model
/// Gateway's and OpenAI's "model … does not exist", the gateway's "not
/// granted", opencode's "Model not found", and Grok's "unknown model".
#[must_use]
pub fn names_missing_model(message: &str) -> bool {
    let message = message.trim().to_ascii_lowercase();
    message.starts_with("model:")
        || message.contains("model not found")
        || message.contains("unknown model")
        || (message.contains("model")
            && (message.contains("does not exist") || message.contains("not granted")))
}

/// A turn failure's category with the facts its copy needs.
///
/// Every field but the category is optional: a source that cannot state a
/// fact leaves it out, and the renderer words around its absence.
//
// Read tolerantly: a key a newer server adds must not break a client a
// release behind, and neither may a value this build cannot read in one of
// the optional facts, which then reads as absent. A plain comment, so the
// generated `wire.ts` does not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TurnFailure {
    /// Why the turn failed.
    pub category: TurnFailureCategory,
    /// The coding engine that failed the turn. Absent on a chat turn and on
    /// a code turn that ran on Tidebreak's own engine, where the failure is
    /// the model provider's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    #[ts(optional)]
    pub engine: Option<HarnessKind>,
    /// The model the failure is about. A code turn's failure about its
    /// model names the model the turn asked for; a chat turn's model rides
    /// beside the failure instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    #[ts(optional)]
    pub model: Option<String>,
    /// When a usage or rate limit lifts, when the engine or provider said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "lenient")]
    #[ts(optional)]
    pub resets_at: Option<DateTime<Utc>>,
}

/// Read an optional fact, or nothing when the value is one this build
/// cannot read, such as an engine a newer server knows.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Lenient<T> {
        Read(T),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(match Option::<Lenient<T>>::deserialize(deserializer)? {
        Some(Lenient::Read(value)) => Some(value),
        Some(Lenient::Unreadable(_)) | None => None,
    })
}

impl TurnFailure {
    /// A failure with only its category known.
    #[must_use]
    pub const fn new(category: TurnFailureCategory) -> Self {
        Self {
            category,
            engine: None,
            model: None,
            resets_at: None,
        }
    }

    /// Classify a failure kind from the internal vocabulary.
    #[must_use]
    pub fn from_kind(kind: &str) -> Self {
        Self::new(TurnFailureCategory::from_kind(kind))
    }

    /// Classify a failure from its kind and its message; see
    /// [`TurnFailureCategory::from_failure`].
    #[must_use]
    pub fn from_failure(kind: &str, message: &str) -> Self {
        Self::new(TurnFailureCategory::from_failure(kind, message))
    }

    /// Name the engine that failed the turn.
    #[must_use]
    pub fn with_engine(mut self, engine: HarnessKind) -> Self {
        self.engine = Some(engine);
        self
    }

    /// Name the model the failure is about.
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Say when the limit lifts.
    #[must_use]
    pub fn with_resets_at(mut self, resets_at: Option<DateTime<Utc>>) -> Self {
        self.resets_at = resets_at;
        self
    }

    /// Say when the limit lifts, from the Unix time in seconds engines
    /// report. A value no calendar holds leaves the time unknown.
    #[must_use]
    pub fn with_reset_timestamp(self, seconds: Option<i64>) -> Self {
        self.with_resets_at(seconds.and_then(|seconds| DateTime::from_timestamp(seconds, 0)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_name(category: TurnFailureCategory) -> String {
        serde_json::to_value(category)
            .expect("a category serializes")
            .as_str()
            .expect("a category is a string")
            .to_owned()
    }

    #[test]
    fn category_names_match_the_wire() {
        for category in TurnFailureCategory::ALL {
            assert_eq!(category.as_str(), wire_name(category));
        }
    }

    /// The five categories an older client can render are the only values
    /// `legacy` produces, and each of those maps to itself.
    #[test]
    fn legacy_categories_stay_inside_what_older_clients_read() {
        let older = [
            TurnFailureCategory::RateLimited,
            TurnFailureCategory::Auth,
            TurnFailureCategory::ProviderAccess,
            TurnFailureCategory::Transient,
            TurnFailureCategory::Unknown,
        ];
        for category in TurnFailureCategory::ALL {
            assert!(older.contains(&category.legacy()), "{category:?}");
        }
        for category in older {
            assert_eq!(category.legacy(), category);
        }
    }

    /// The audit's cases: a retired model, an oversized conversation, a
    /// rejected request, and a local fault are not blind-retryable; throttles
    /// and network faults are.
    #[test]
    fn retryability_is_part_of_each_category() {
        for (kind, category, retryable) in [
            ("rate_limited", TurnFailureCategory::RateLimited, true),
            ("overloaded", TurnFailureCategory::Overloaded, true),
            ("provider", TurnFailureCategory::Transient, true),
            ("empty_model_response", TurnFailureCategory::Transient, true),
            ("authentication", TurnFailureCategory::Auth, false),
            ("missing_credential", TurnFailureCategory::Auth, false),
            ("access_denied", TurnFailureCategory::ProviderAccess, false),
            (
                "model_unavailable",
                TurnFailureCategory::ModelUnavailable,
                false,
            ),
            (
                "unknown_model",
                TurnFailureCategory::ModelUnavailable,
                false,
            ),
            (
                "prompt_too_long",
                TurnFailureCategory::ContextOverflow,
                false,
            ),
            (
                "invalid_request",
                TurnFailureCategory::RequestRejected,
                false,
            ),
            ("refusal", TurnFailureCategory::RequestRejected, false),
            (
                "endpoint_not_found",
                TurnFailureCategory::EndpointNotFound,
                false,
            ),
            // Without its message a store or secret failure may be one that
            // clears, so it stays retryable.
            ("store", TurnFailureCategory::Transient, true),
            ("secret", TurnFailureCategory::Transient, true),
            ("engine_auth", TurnFailureCategory::EngineAuth, false),
            ("usage_limit", TurnFailureCategory::UsageLimit, false),
            ("max_steps_exceeded", TurnFailureCategory::Unknown, false),
            ("a_code_from_next_year", TurnFailureCategory::Unknown, false),
        ] {
            let classified = TurnFailureCategory::from_kind(kind);
            assert_eq!(classified, category, "{kind}");
            assert_eq!(classified.retries_may_succeed(), retryable, "{kind}");
        }
    }

    /// A timeout or a busy store may clear, so the worker retries it; only a
    /// definite fault on the machine Tidebreak runs on is `local`. The
    /// messages are the sources' own: SeaORM's pool timeout, SQLite's busy,
    /// full, and damaged-file errors, Tidebreak's Vault timeout, and the
    /// `keyring` crate's platform failures, each behind the prefix
    /// `AgentError` gives it.
    #[test]
    fn store_and_secret_failures_are_local_only_when_waiting_cannot_help() {
        use TurnFailureCategory::{Local, Transient};
        for (kind, message, expected) in [
            (
                "store",
                "store error: Failed to acquire connection from pool: Connection pool timed out",
                Transient,
            ),
            (
                "store",
                "store error: Execution Error: error returned from database: (code: 5) database is locked",
                Transient,
            ),
            ("secret", "secret error: the Vault request timed out", Transient),
            ("secret", "secret error: the Vault request failed", Transient),
            (
                "store",
                "store error: Execution Error: error returned from database: (code: 13) database or disk is full",
                Local,
            ),
            (
                "store",
                "store error: Execution Error: error returned from database: (code: 11) database disk image is malformed",
                Local,
            ),
            (
                "store",
                "store error: could not write the blob: No space left on device (os error 28)",
                Local,
            ),
            (
                "secret",
                "secret error: Platform secure storage failure: The user name or passphrase you entered is not correct.",
                Local,
            ),
            (
                "secret",
                "secret error: Couldn't access platform secure storage: The specified keychain could not be found.",
                Local,
            ),
        ] {
            let category = TurnFailureCategory::from_failure(kind, message);
            assert_eq!(category, expected, "{message}");
            assert_eq!(category.retries_may_succeed(), expected == Transient);
        }
        // The message refines only Tidebreak's own failures.
        assert_eq!(
            TurnFailureCategory::from_failure("provider", "database or disk is full"),
            Transient
        );
    }

    /// A category or engine from a newer server reads as `unknown` or as
    /// absent, and the failure around it still reads. Before this, one new
    /// category made a whole frame unreadable.
    #[test]
    fn values_from_a_newer_server_degrade_instead_of_failing() {
        assert_eq!(
            serde_json::from_str::<TurnFailureCategory>("\"quota_exceeded\"").unwrap(),
            TurnFailureCategory::Unknown
        );
        let failure: TurnFailure = serde_json::from_value(serde_json::json!({
            "category": "quota_exceeded",
            "engine": "an_engine_from_next_year",
            "model": 7,
            "resets_at": "not a time",
            "added_later": true,
        }))
        .unwrap();
        assert_eq!(failure, TurnFailure::new(TurnFailureCategory::Unknown));
        // Known values still read exactly.
        let known: TurnFailure = serde_json::from_value(serde_json::json!({
            "category": "usage_limit",
            "engine": "codex",
            "model": "gpt-5.5",
            "resets_at": "2026-08-20T15:05:54Z",
        }))
        .unwrap();
        assert_eq!(known.engine, Some(HarnessKind::Codex));
        assert_eq!(known.model.as_deref(), Some("gpt-5.5"));
        assert!(known.resets_at.is_some());
        // Unknown never serializes as anything but itself.
        assert_eq!(wire_name(TurnFailureCategory::Unknown), "unknown");
    }

    /// Only the provider's own words about the model make a 404 a missing
    /// model. Each message is one the router and adapter tests already carry.
    #[test]
    fn a_missing_model_is_named_by_the_provider_not_by_the_status() {
        for message in [
            "model: claude-3-opus-20240229",
            "The requested model does not exist or is not granted to you on this gateway.",
            "Model not found: opencode/this-model-does-not-exist.",
            "Couldn't set model 'definitely-not-a-real-model-xyz': Invalid params: \"unknown model id\".",
        ] {
            assert!(names_missing_model(message), "{message}");
        }
        for message in [
            "",
            "Not Found",
            "<html><head><title>404 Not Found</title></head><body><center><h1>404 Not Found</h1></center><hr><center>nginx</center></body></html>",
            "Unknown error",
        ] {
            assert!(!names_missing_model(message), "{message}");
        }
    }

    #[test]
    fn absent_facts_stay_off_the_wire() {
        let bare = serde_json::to_value(TurnFailure::new(TurnFailureCategory::Local)).unwrap();
        assert_eq!(bare, serde_json::json!({ "category": "local" }));
        let full = TurnFailure::new(TurnFailureCategory::UsageLimit)
            .with_engine(HarnessKind::Codex)
            .with_model("gpt-5.6")
            .with_resets_at(Some(DateTime::from_timestamp(1_787_238_354, 0).unwrap()));
        let encoded = serde_json::to_value(&full).unwrap();
        assert_eq!(encoded["engine"], "codex");
        assert_eq!(encoded["resets_at"], "2026-08-20T15:05:54Z");
        assert_eq!(
            serde_json::from_value::<TurnFailure>(encoded).unwrap(),
            full
        );
    }
}
