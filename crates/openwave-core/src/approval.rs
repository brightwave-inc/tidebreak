//! Durable approval registration and park-and-resume for Sensitive tools.
//!
//! When the agent hits a tool whose [`ApprovalClass`](crate::tool::ApprovalClass)
//! requires a human decision, it registers an [`ApprovalGate`] durably, emits
//! `ApprovalRequired`, then awaits the decision. Tests inject an auto-approve
//! or refuse gate instead.
//!
//! **Ordering invariant:** the registration future must make the call durable
//! before it resolves, so a client that sees
//! `ApprovalRequired` can never race a 404 against a not-yet-parked call.

use std::future::Future;
use std::pin::Pin;

use crate::event::SequencedEvent;
use crate::id::{CallId, ChatId, TurnId};
use crate::tool::ApprovalClass;
use chrono::{DateTime, Utc};

/// The human's decision on a parked tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    /// Run the tool.
    Approve,
    /// Skip the tool; feed an error result back to the model.
    Reject {
        /// Why it was rejected (shown to the model / UI).
        reason: String,
    },
}

impl ApprovalDecision {
    /// Durable status corresponding to this decision.
    #[must_use]
    pub const fn status(&self) -> ToolApprovalStatus {
        match self {
            Self::Approve => ToolApprovalStatus::Approved,
            Self::Reject { .. } => ToolApprovalStatus::Rejected,
        }
    }

    /// Stable reject reason, if this is a rejection.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Approve => None,
            Self::Reject { reason } => Some(reason),
        }
    }
}

/// Durable lifecycle for one Sensitive tool approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolApprovalStatus {
    /// The canonical tool call is waiting for a human decision.
    Pending,
    /// The exact call was approved once.
    Approved,
    /// The exact call was rejected.
    Rejected,
}

/// Closed immutable consent semantics stored with each approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalKind {
    SearchMayShareQueryAndExcerpts,
    Unsupported,
}

impl ToolApprovalKind {
    #[must_use]
    pub fn for_tool_name(name: &str) -> Self {
        match name {
            "search" => Self::SearchMayShareQueryAndExcerpts,
            _ => Self::Unsupported,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SearchMayShareQueryAndExcerpts => "search_may_share_query_and_excerpts",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn is_approvable(self) -> bool {
        matches!(self, Self::SearchMayShareQueryAndExcerpts)
    }
}

impl ToolApprovalStatus {
    /// Stable database representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

/// Private durable approval state. Canonical tool arguments and model summaries
/// deliberately remain outside this read model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolApproval {
    pub call_id: CallId,
    pub chat_id: ChatId,
    pub turn_id: TurnId,
    pub tool_name: String,
    pub class: ApprovalClass,
    pub kind: ToolApprovalKind,
    pub status: ToolApprovalStatus,
    pub reason: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl ToolApproval {
    /// Maximum stored reject-reason bytes.
    pub const MAX_REASON_BYTES: usize = 1024;

    /// Maximum stored reject-reason Unicode scalar values.
    pub const MAX_REASON_CHARS: usize = 512;

    /// Whether a reject reason is safe to persist and feed back to the model.
    #[must_use]
    pub fn valid_reason(reason: &str) -> bool {
        !reason.is_empty()
            && reason.len() <= Self::MAX_REASON_BYTES
            && reason.chars().count() <= Self::MAX_REASON_CHARS
            && !reason.chars().any(char::is_control)
    }

    /// Recover a terminal decision from durable state.
    #[must_use]
    pub fn decision(&self) -> Option<ApprovalDecision> {
        match self.status {
            ToolApprovalStatus::Pending => None,
            ToolApprovalStatus::Approved => Some(ApprovalDecision::Approve),
            ToolApprovalStatus::Rejected => Some(ApprovalDecision::Reject {
                reason: self
                    .reason
                    .clone()
                    .unwrap_or_else(|| "user denied approval".into()),
            }),
        }
    }
}

/// What the agent asks the gate to decide.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Correlates with `ApprovalRequired` / the HTTP path.
    pub call_id: CallId,
    /// Chat the turn belongs to.
    pub chat_id: ChatId,
    /// Turn that is parked.
    pub turn_id: TurnId,
    /// Tool name the model invoked.
    pub tool_name: String,
    /// Approval class that triggered the park.
    pub class: ApprovalClass,
    /// Closed server-owned consent semantics, frozen at first registration.
    pub kind: ToolApprovalKind,
    /// Short human-readable summary of what will happen.
    pub summary: String,
}

/// A future that resolves to an [`ApprovalDecision`].
pub type ApprovalFuture = Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'static>>;

/// Exact claimed-turn journal slot proposed for `ApprovalRequired`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ApprovalJournalIdentity {
    pub lease_token: uuid::Uuid,
    pub event_ordinal: i32,
}

impl std::fmt::Debug for ApprovalJournalIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApprovalJournalIdentity")
            .field("lease_token", &"[redacted]")
            .field("event_ordinal", &self.event_ordinal)
            .finish()
    }
}

/// How the agent should expose a newly durable approval requirement.
pub enum ApprovalRequiredPublication {
    /// Legacy, non-claimed execution must append the event normally.
    Ordinary,
    /// A claimed execution already committed this exact event and only needs
    /// live publication.
    Committed {
        event_ordinal: i32,
        event: SequencedEvent,
    },
    /// An exact receipt was recovered after the approval had already become
    /// terminal. Consume its ordinal without flashing a stale required card.
    Recovered {
        event_ordinal: i32,
        event: SequencedEvent,
    },
    /// Exact recovery found no new requirement to publish.
    None,
}

/// Durable registration followed by the independently parked decision.
pub struct ApprovalRegistration {
    pub decision: ApprovalFuture,
    pub publication: ApprovalRequiredPublication,
}

/// Registration future. It resolves only after the request is durable and
/// returns the separate wait future used after `ApprovalRequired` is emitted.
pub type ApprovalRegistrationFuture<'a> =
    Pin<Box<dyn Future<Output = ApprovalRegistration> + Send + 'a>>;

/// Decides whether a Sensitive tool call may run.
///
/// Object-safe and held behind `Arc` so the server can park on a channel while
/// tests inject an immediate approve/reject.
pub trait ApprovalGate: Send + Sync {
    /// Register a pending approval before resolving this future. The returned
    /// future independently waits for the human decision.
    fn register(
        &self,
        request: ApprovalRequest,
        journal: Option<ApprovalJournalIdentity>,
    ) -> ApprovalRegistrationFuture<'_>;
}

/// Always rejects — the default when no gate is wired. Keeps fail-closed
/// behavior for Sensitive tools outside the server's park-and-resume path.
pub struct RefuseGate;

impl ApprovalGate for RefuseGate {
    fn register(
        &self,
        request: ApprovalRequest,
        _journal: Option<ApprovalJournalIdentity>,
    ) -> ApprovalRegistrationFuture<'_> {
        Box::pin(async move {
            ApprovalRegistration {
                decision: Box::pin(async move {
                    ApprovalDecision::Reject {
                        reason: format!(
                            "{} requires approval, which is not configured",
                            request.tool_name
                        ),
                    }
                }) as ApprovalFuture,
                publication: ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}

/// Always approves — for tests that exercise Sensitive tools without a UI.
pub struct AutoApproveGate;

impl ApprovalGate for AutoApproveGate {
    fn register(
        &self,
        _request: ApprovalRequest,
        _journal: Option<ApprovalJournalIdentity>,
    ) -> ApprovalRegistrationFuture<'_> {
        Box::pin(async {
            ApprovalRegistration {
                decision: Box::pin(async { ApprovalDecision::Approve }) as ApprovalFuture,
                publication: ApprovalRequiredPublication::Ordinary,
            }
        })
    }
}
