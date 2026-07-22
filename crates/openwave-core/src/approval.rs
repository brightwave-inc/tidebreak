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
use std::sync::RwLock;

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
///
/// Each presentable variant names the egress a human is consenting to, so the
/// renderer can describe the action without ever seeing the model-authored
/// summary or arguments. `Unsupported` is the fail-closed default: a Sensitive
/// action the server can only reject, never approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalKind {
    /// A library search may share the query and matched excerpts with the
    /// provider.
    SearchMayShareQueryAndExcerpts,
    /// A command execution escapes the chat workspace and may reach the network.
    ExecMayRunNetworkedCommand,
    /// A Sensitive action with no presentable consent semantics: rejectable but
    /// not approvable from the renderer.
    Unsupported,
}

impl ToolApprovalKind {
    #[must_use]
    pub fn for_tool_name(name: &str) -> Self {
        match name {
            "search" => Self::SearchMayShareQueryAndExcerpts,
            "exec" => Self::ExecMayRunNetworkedCommand,
            _ => Self::Unsupported,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SearchMayShareQueryAndExcerpts => "search_may_share_query_and_excerpts",
            Self::ExecMayRunNetworkedCommand => "exec_may_run_networked_command",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn is_approvable(self) -> bool {
        matches!(
            self,
            Self::SearchMayShareQueryAndExcerpts | Self::ExecMayRunNetworkedCommand
        )
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

/// A remembered approval that lets a repeated in-scope Sensitive action run
/// without re-prompting.
///
/// Deny-by-default, like the host broker's capability grants: a grant covers
/// exactly one chat and one approvable tool. Resource-level sub-scoping (a path
/// subtree, a single connector) is deliberately deferred until
/// [`ApprovalRequest`] carries a structured resource — see the follow-up on the
/// capability-model issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingGrant {
    chat_id: ChatId,
    tool_name: String,
    kind: ToolApprovalKind,
    granted_at: DateTime<Utc>,
}

impl StandingGrant {
    /// Record consent to stop re-prompting `tool_name` in `chat_id`.
    ///
    /// Returns `None` for a tool whose consent semantics are not standing-
    /// grantable (mirrors [`ToolApprovalKind::is_approvable`]); an unknown or
    /// non-approvable action must keep parking on the gate every call.
    #[must_use]
    pub fn new(
        chat_id: ChatId,
        tool_name: impl Into<String>,
        kind: ToolApprovalKind,
        granted_at: DateTime<Utc>,
    ) -> Option<Self> {
        if !kind.is_approvable() {
            return None;
        }
        Some(Self {
            chat_id,
            tool_name: tool_name.into(),
            kind,
            granted_at,
        })
    }

    /// Chat the standing consent belongs to.
    #[must_use]
    pub const fn chat_id(&self) -> ChatId {
        self.chat_id
    }

    /// Tool name the consent covers.
    #[must_use]
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Frozen consent semantics the grant was created under.
    #[must_use]
    pub const fn kind(&self) -> ToolApprovalKind {
        self.kind
    }

    /// When the user granted the standing approval.
    #[must_use]
    pub const fn granted_at(&self) -> DateTime<Utc> {
        self.granted_at
    }

    /// Whether this grant covers an exact Sensitive action.
    #[must_use]
    fn covers(&self, chat_id: ChatId, tool_name: &str, kind: ToolApprovalKind) -> bool {
        self.chat_id == chat_id
            && self.tool_name == tool_name
            && self.kind == kind
            && kind.is_approvable()
    }
}

/// A live, deny-by-default set of standing grants consulted before a Sensitive
/// tool call parks on the approval gate.
///
/// Held behind `Arc` and shared with the agent loop. Interior mutability lets a
/// later decision path record a grant mid-turn without re-threading the agent;
/// this slice only seeds and consults the set.
#[derive(Debug, Default)]
pub struct StandingGrants {
    grants: RwLock<Vec<StandingGrant>>,
}

impl StandingGrants {
    /// An empty set — nothing is pre-approved.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a fixed set of grants, e.g. recovered from durable state at turn
    /// start.
    #[must_use]
    pub fn from_grants(grants: Vec<StandingGrant>) -> Self {
        Self {
            grants: RwLock::new(grants),
        }
    }

    /// Remember `grant`, ignoring a duplicate so the set never grows unbounded
    /// on repeated approvals of the same action.
    pub fn record(&self, grant: StandingGrant) {
        let mut grants = self.write();
        if !grants.iter().any(|existing| {
            existing.chat_id == grant.chat_id
                && existing.tool_name == grant.tool_name
                && existing.kind == grant.kind
        }) {
            grants.push(grant);
        }
    }

    /// Whether a live grant covers this exact Sensitive action.
    #[must_use]
    pub fn covers(&self, chat_id: ChatId, tool_name: &str, kind: ToolApprovalKind) -> bool {
        self.read()
            .iter()
            .any(|grant| grant.covers(chat_id, tool_name, kind))
    }

    /// Drop every grant, e.g. on explicit revocation.
    pub fn clear(&self) {
        self.write().clear();
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Vec<StandingGrant>> {
        self.grants
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Vec<StandingGrant>> {
        self.grants
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

#[cfg(test)]
mod standing_grant_tests {
    use super::*;

    fn grant(chat_id: ChatId, tool: &str) -> StandingGrant {
        StandingGrant::new(
            chat_id,
            tool,
            ToolApprovalKind::for_tool_name(tool),
            Utc::now(),
        )
        .expect("approvable tool is grantable")
    }

    #[test]
    fn escaping_exec_is_a_presentable_approvable_kind() {
        let kind = ToolApprovalKind::for_tool_name("exec");
        assert_eq!(kind, ToolApprovalKind::ExecMayRunNetworkedCommand);
        assert!(kind.is_approvable());
        // The string round-trips through durable storage without collapsing to
        // `Unsupported`, so an approved escaping action stays approvable on
        // recovery.
        assert_eq!(kind.as_str(), "exec_may_run_networked_command");
    }

    #[test]
    fn escaping_exec_is_standing_grantable() {
        let chat = ChatId::new();
        let grants = StandingGrants::from_grants(vec![grant(chat, "exec")]);
        let kind = ToolApprovalKind::for_tool_name("exec");
        assert!(grants.covers(chat, "exec", kind));
        // Deny-by-default still holds for a different chat.
        assert!(!grants.covers(ChatId::new(), "exec", kind));
    }

    #[test]
    fn non_approvable_tools_cannot_be_granted() {
        assert!(StandingGrant::new(
            ChatId::new(),
            "third_party_sensitive",
            ToolApprovalKind::for_tool_name("third_party_sensitive"),
            Utc::now(),
        )
        .is_none());
    }

    #[test]
    fn empty_set_covers_nothing() {
        let chat = ChatId::new();
        let grants = StandingGrants::new();
        assert!(!grants.covers(chat, "search", ToolApprovalKind::for_tool_name("search")));
    }

    #[test]
    fn grant_covers_only_its_exact_chat_and_tool() {
        let chat = ChatId::new();
        let other_chat = ChatId::new();
        let grants = StandingGrants::from_grants(vec![grant(chat, "search")]);
        let kind = ToolApprovalKind::for_tool_name("search");

        assert!(grants.covers(chat, "search", kind));
        assert!(!grants.covers(other_chat, "search", kind));
        assert!(!grants.covers(
            chat,
            "third_party_sensitive",
            ToolApprovalKind::for_tool_name("third_party_sensitive"),
        ));
    }

    #[test]
    fn recording_is_idempotent_and_revocable() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("search");
        let grants = StandingGrants::new();
        grants.record(grant(chat, "search"));
        grants.record(grant(chat, "search"));
        assert_eq!(grants.read().len(), 1);
        assert!(grants.covers(chat, "search", kind));

        grants.clear();
        assert!(!grants.covers(chat, "search", kind));
    }
}
