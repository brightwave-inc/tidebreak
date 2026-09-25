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
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::code::HarnessKind;

/// Why a turn failed, closed and coarse enough to be stable.
///
/// Clients built before the full vocabulary read only `rate_limited`, `auth`,
/// `provider_access`, `transient`, and `unknown`, and nothing else is ever
/// sent to them; [`Self::legacy`] maps every category onto that set.
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
    /// account. Choosing another model is the remedy.
    ModelUnavailable,
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
    /// internal invariants. A client should not promise that a retry helps.
    Unknown,
}

impl TurnFailureCategory {
    /// Every category, in declaration order.
    pub const ALL: [Self; 12] = [
        Self::RateLimited,
        Self::Overloaded,
        Self::Auth,
        Self::ProviderAccess,
        Self::ModelUnavailable,
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
    /// failure code reads as coarse rather than wrong.
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
            // The agent loop surfaces this only after its own context
            // reductions ran out.
            "prompt_too_long" => Self::ContextOverflow,
            "invalid_request" | "refusal" => Self::RequestRejected,
            // A keychain denial or a database or disk error is Tidebreak's
            // own, never the provider's.
            "store" | "secret" => Self::Local,
            "provider" | "empty_model_response" => Self::Transient,
            "engine_auth" => Self::EngineAuth,
            "usage_limit" => Self::UsageLimit,
            _ => Self::Unknown,
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

/// A turn failure's category with the facts its copy needs.
///
/// Every field but the category is optional: a source that cannot state a
/// fact leaves it out, and the renderer words around its absence.
//
// Read tolerantly: a key a newer server adds must not break a client a
// release behind. A plain comment, so the generated `wire.ts` does not carry
// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct TurnFailure {
    /// Why the turn failed.
    pub category: TurnFailureCategory,
    /// The coding engine that failed the turn. Absent on a chat turn and on
    /// a code turn that ran on Tidebreak's own engine, where the failure is
    /// the model provider's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub engine: Option<HarnessKind>,
    /// The model the failure is about. A code turn's failure about its
    /// model names the model the turn asked for; a chat turn's model rides
    /// beside the failure instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// When a usage or rate limit lifts, when the engine or provider said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resets_at: Option<DateTime<Utc>>,
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
            ("store", TurnFailureCategory::Local, false),
            ("secret", TurnFailureCategory::Local, false),
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
