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
use crate::preview::ToolActionPreview;
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
    /// A public-web search may share the query and explicit filters with the
    /// host's configured provider.
    WebSearchMayShareQuery,
    /// A command execution escapes the chat workspace and may reach the network.
    ExecMayRunNetworkedCommand,
    /// An external MCP process may receive the call and act with its own local
    /// or remote authority. Calls are approvable once but never standing-
    /// grantable because a Settings edit can replace the process behind a
    /// stable tool namespace.
    ExternalMcpMayCallServer,
    /// A Sensitive action with no presentable consent semantics: rejectable but
    /// not approvable from the renderer.
    Unsupported,
}

impl ToolApprovalKind {
    #[must_use]
    pub fn for_tool_name(name: &str) -> Self {
        match name {
            "search" => Self::SearchMayShareQueryAndExcerpts,
            "web_search" => Self::WebSearchMayShareQuery,
            "exec" => Self::ExecMayRunNetworkedCommand,
            name if name.starts_with("mcp__") => Self::ExternalMcpMayCallServer,
            _ => Self::Unsupported,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SearchMayShareQueryAndExcerpts => "search_may_share_query_and_excerpts",
            // Existing databases constrain this column to the original closed
            // search vocabulary. The exact tool name stored beside it
            // disambiguates document search from web search on recovery, while
            // serde still exposes the narrower renderer kind.
            Self::WebSearchMayShareQuery => "search_may_share_query_and_excerpts",
            Self::ExecMayRunNetworkedCommand => "exec_may_run_networked_command",
            // Existing databases have a closed approval-kind constraint. The
            // exact namespaced tool name disambiguates this renderer kind when
            // durable state is read, as it already does for web search.
            Self::ExternalMcpMayCallServer => "search_may_share_query_and_excerpts",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn is_approvable(self) -> bool {
        matches!(
            self,
            Self::SearchMayShareQueryAndExcerpts
                | Self::WebSearchMayShareQuery
                | Self::ExecMayRunNetworkedCommand
                | Self::ExternalMcpMayCallServer
        )
    }

    /// Whether approval may be remembered for later calls in the same chat.
    ///
    /// MCP is intentionally one-shot: its configured executable can change
    /// while retaining a model-visible namespace, so reusing consent by name
    /// would silently widen authority.
    #[must_use]
    pub const fn is_standing_grantable(self) -> bool {
        self.is_approvable() && !matches!(self, Self::ExternalMcpMayCallServer)
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
/// deliberately remain outside this read model; only the closed
/// [`ToolActionPreview`] projection of those arguments is presentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolApproval {
    pub call_id: CallId,
    pub chat_id: ChatId,
    pub turn_id: TurnId,
    pub tool_name: String,
    pub class: ApprovalClass,
    pub kind: ToolApprovalKind,
    /// What the call will do, when the tool projects it.
    pub preview: Option<ToolActionPreview>,
    /// Whether [`Self::preview`] reproduces the call's arguments exactly.
    ///
    /// Derived alongside the preview from the arguments the call is parked on,
    /// not stored: the preview is clamped for display, and a scope narrower
    /// than the whole tool may only be built from one that lost nothing. See
    /// [`ToolActionPreview::describes_exactly`].
    pub action_is_exact: bool,
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
    scope: GrantScope,
    granted_at: DateTime<Utc>,
}

/// How much a standing grant covers.
///
/// A grant is easy to widen by accident and hard to notice afterwards, so the
/// scope is stated in the grant itself rather than inferred from the tool. The
/// narrower variants exist because "don't ask me about commands again" is a
/// much larger thing to agree to than "don't ask me about `cargo` again".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantScope {
    /// Exactly this argument vector, in this directory, and nothing else.
    ExactCommand {
        command: String,
        args: Vec<String>,
        cwd: String,
    },
    /// This executable, with any arguments.
    AnyArgsFor { command: String },
    /// Every call to the tool.
    WholeTool,
}

impl GrantScope {
    /// Whether this scope covers the exact call about to run.
    ///
    /// Takes the canonical arguments rather than only the action preview,
    /// because the preview is clamped for display and the clamp is many-to-one:
    /// a 600-character command and that command with `; rm -rf ~` appended
    /// project to the same 512 characters. Keying authorization on the
    /// projection would let the second run under a grant given for the first.
    ///
    /// So a narrow scope requires the call to be exactly describable
    /// ([`ToolActionPreview::describes_exactly`]) *and* to match. An action the
    /// renderer could not describe, or could only describe approximately, keeps
    /// asking.
    #[must_use]
    pub fn covers_call(&self, tool_name: &str, arguments: &serde_json::Value) -> bool {
        use crate::preview::ToolActionPreview;
        if matches!(self, Self::WholeTool) {
            return true;
        }
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            return false;
        }
        let Some(ToolActionPreview::Exec {
            command: call_command,
            args: call_args,
            cwd: call_cwd,
        }) = ToolActionPreview::build(tool_name, arguments)
        else {
            return false;
        };
        match self {
            Self::WholeTool => true,
            Self::ExactCommand { command, args, cwd } => {
                call_command == *command && call_args == *args && call_cwd == *cwd
            }
            Self::AnyArgsFor { command } => call_command == *command,
        }
    }

    /// The scopes offered for a call, narrowest first.
    ///
    /// A call with nothing to describe — or nothing describable *exactly* —
    /// can only be granted wholesale, because there is no narrower thing that
    /// could be named without naming it approximately.
    #[must_use]
    pub fn ladder_for(tool_name: &str, arguments: &serde_json::Value) -> Vec<Self> {
        use crate::preview::ToolActionPreview;
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            return vec![Self::WholeTool];
        }
        match ToolActionPreview::build(tool_name, arguments) {
            Some(ToolActionPreview::Exec { command, args, cwd }) => {
                let mut ladder = vec![Self::ExactCommand {
                    command: command.clone(),
                    args: args.clone(),
                    cwd: cwd.clone(),
                }];
                // With no arguments, "exactly this" and "this executable with
                // any arguments" would read as two names for one grant.
                if !args.is_empty() {
                    ladder.push(Self::AnyArgsFor {
                        command: command.clone(),
                    });
                }
                ladder.push(Self::WholeTool);
                ladder
            }
            _ => vec![Self::WholeTool],
        }
    }
}

impl StandingGrant {
    /// Record consent to stop re-prompting `tool_name` in `chat_id`.
    ///
    /// Returns `None` for a tool whose consent semantics are not
    /// [`ToolApprovalKind::is_standing_grantable`]. An unknown action, an
    /// unapprovable action, or a replaceable MCP namespace must keep parking on
    /// the gate every call.
    #[must_use]
    pub fn new(
        chat_id: ChatId,
        tool_name: impl Into<String>,
        kind: ToolApprovalKind,
        granted_at: DateTime<Utc>,
    ) -> Option<Self> {
        Self::scoped(chat_id, tool_name, kind, GrantScope::WholeTool, granted_at)
    }

    /// Record consent limited to `scope`.
    ///
    /// Returns `None` on the same terms as [`StandingGrant::new`].
    #[must_use]
    pub fn scoped(
        chat_id: ChatId,
        tool_name: impl Into<String>,
        kind: ToolApprovalKind,
        scope: GrantScope,
        granted_at: DateTime<Utc>,
    ) -> Option<Self> {
        if !kind.is_standing_grantable() {
            return None;
        }
        Some(Self {
            chat_id,
            tool_name: tool_name.into(),
            kind,
            scope,
            granted_at,
        })
    }

    /// How much this grant covers.
    #[must_use]
    pub const fn scope(&self) -> &GrantScope {
        &self.scope
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

    /// Whether this grant covers an exact Sensitive call.
    #[must_use]
    fn covers(
        &self,
        chat_id: ChatId,
        tool_name: &str,
        kind: ToolApprovalKind,
        arguments: &serde_json::Value,
    ) -> bool {
        self.chat_id == chat_id
            && self.tool_name == tool_name
            && self.kind == kind
            && kind.is_approvable()
            && self.scope.covers_call(tool_name, arguments)
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
                && existing.scope == grant.scope
        }) {
            grants.push(grant);
        }
    }

    /// Whether a live grant covers this exact Sensitive call.
    ///
    /// Takes the canonical arguments, not the display projection of them: see
    /// [`GrantScope::covers_call`] for why a clamped preview cannot decide
    /// authority.
    #[must_use]
    pub fn covers(
        &self,
        chat_id: ChatId,
        tool_name: &str,
        kind: ToolApprovalKind,
        arguments: &serde_json::Value,
    ) -> bool {
        self.read()
            .iter()
            .any(|grant| grant.covers(chat_id, tool_name, kind, arguments))
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
    /// Closed projection of the arguments this call will run with, when the
    /// tool has one. Registration ignores it; it exists to be presented.
    pub preview: Option<ToolActionPreview>,
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
    fn web_search_has_narrow_presentable_consent_semantics() {
        let kind = ToolApprovalKind::for_tool_name("web_search");
        assert_eq!(kind, ToolApprovalKind::WebSearchMayShareQuery);
        assert!(kind.is_approvable());
        assert_eq!(
            serde_json::to_string(&kind).unwrap(),
            "\"web_search_may_share_query\""
        );
        assert_eq!(
            kind.as_str(),
            ToolApprovalKind::SearchMayShareQueryAndExcerpts.as_str()
        );
    }

    #[test]
    fn escaping_exec_is_standing_grantable() {
        let chat = ChatId::new();
        let grants = StandingGrants::from_grants(vec![grant(chat, "exec")]);
        let kind = ToolApprovalKind::for_tool_name("exec");
        assert!(grants.covers(chat, "exec", kind, &no_args()));
        // Deny-by-default still holds for a different chat.
        assert!(!grants.covers(ChatId::new(), "exec", kind, &no_args()));
    }

    #[test]
    fn external_mcp_is_approvable_once_but_never_standing_grantable() {
        let kind = ToolApprovalKind::for_tool_name("mcp__documents__search");
        assert_eq!(kind, ToolApprovalKind::ExternalMcpMayCallServer);
        assert!(kind.is_approvable());
        assert!(!kind.is_standing_grantable());
        assert!(
            StandingGrant::new(ChatId::new(), "mcp__documents__search", kind, Utc::now(),)
                .is_none()
        );
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
        assert!(!grants.covers(
            chat,
            "search",
            ToolApprovalKind::for_tool_name("search"),
            &no_args()
        ));
    }

    #[test]
    fn grant_covers_only_its_exact_chat_and_tool() {
        let chat = ChatId::new();
        let other_chat = ChatId::new();
        let grants = StandingGrants::from_grants(vec![grant(chat, "search")]);
        let kind = ToolApprovalKind::for_tool_name("search");

        assert!(grants.covers(chat, "search", kind, &no_args()));
        assert!(!grants.covers(other_chat, "search", kind, &no_args()));
        assert!(!grants.covers(
            chat,
            "third_party_sensitive",
            ToolApprovalKind::for_tool_name("third_party_sensitive"),
            &no_args(),
        ));
    }

    /// A tool with no action projection; every scope decision for it turns on
    /// the grant itself rather than on matching an action.
    fn no_args() -> serde_json::Value {
        serde_json::json!({})
    }

    /// Canonical `exec` arguments, which is what a grant is now matched on.
    fn exec_args(command: &str, args: &[&str]) -> serde_json::Value {
        serde_json::json!({ "command": command, "args": args })
    }

    #[test]
    fn an_exact_grant_covers_only_the_vector_it_named() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "exec",
                kind,
                GrantScope::ExactCommand {
                    command: "cargo".into(),
                    args: vec!["test".into()],
                    cwd: ".".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, "exec", kind, &exec_args("cargo", &["test"])));
        assert!(!grants.covers(chat, "exec", kind, &exec_args("cargo", &["publish"])));
        assert!(!grants.covers(chat, "exec", kind, &exec_args("rm", &["test"])));
        // An action the renderer could not describe was never the one granted.
        assert!(!grants.covers(chat, "exec", kind, &no_args()));
    }

    #[test]
    fn a_clamped_call_never_matches_or_creates_a_narrow_grant() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let long = "x".repeat(crate::preview::MAX_ACTION_FIELD_CHARS);
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "exec",
                kind,
                GrantScope::ExactCommand {
                    command: "bash".into(),
                    args: vec!["-c".into(), long.clone()],
                    cwd: ".".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        // Truncation: a longer script sharing the granted prefix projects to
        // the same preview. Keying on the projection would run it unprompted.
        let appended = format!("{long}; rm -rf ~");
        assert!(!grants.covers(chat, "exec", kind, &exec_args("bash", &["-c", &appended])));
        // A value exactly at the bound is not truncated, so it stays faithful
        // and the grant still covers the command it was actually given for.
        assert!(grants.covers(chat, "exec", kind, &exec_args("bash", &["-c", &long])));
        // Nothing at or past the bound can be turned into a narrow grant.
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("bash", &["-c", &appended])),
            vec![GrantScope::WholeTool]
        );
    }

    #[test]
    fn an_exact_grant_is_not_widened_by_a_dropped_or_extra_argument() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "exec",
                kind,
                GrantScope::ExactCommand {
                    command: "foo".into(),
                    args: vec!["bar".into()],
                    cwd: ".".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, "exec", kind, &exec_args("foo", &["bar"])));
        // An empty argument used to clamp away entirely, making these two
        // different calls indistinguishable.
        assert!(!grants.covers(chat, "exec", kind, &exec_args("foo", &["", "bar"])));
        // A control character used to be stripped, so these collapsed too.
        assert!(!grants.covers(chat, "exec", kind, &exec_args("foo", &["b\u{0}ar"])));
        // A non-string argument used to be dropped, changing the call's arity.
        assert!(!grants.covers(
            chat,
            "exec",
            kind,
            &serde_json::json!({ "command": "foo", "args": ["bar", 7] })
        ));
        // Past the argument cap the tail was elided, so a longer vector matched
        // a shorter grant.
        let many: Vec<String> = (0..=crate::preview::MAX_ACTION_ARGS)
            .map(|index| index.to_string())
            .collect();
        assert!(!grants.covers(
            chat,
            "exec",
            kind,
            &serde_json::json!({ "command": "foo", "args": many })
        ));
    }

    #[test]
    fn an_exact_grant_is_scoped_to_the_directory_it_was_given_in() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "exec",
                kind,
                GrantScope::ExactCommand {
                    command: "npm".into(),
                    args: vec!["install".into()],
                    cwd: "./sandbox".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        let in_dir =
            |cwd: &str| serde_json::json!({ "command": "npm", "args": ["install"], "cwd": cwd });
        assert!(grants.covers(chat, "exec", kind, &in_dir("./sandbox")));
        // The card showed the directory, so the grant is about that directory.
        assert!(!grants.covers(chat, "exec", kind, &in_dir("/")));
    }

    #[test]
    fn an_executable_grant_covers_any_arguments_to_it() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "exec",
                kind,
                GrantScope::AnyArgsFor {
                    command: "cargo".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, "exec", kind, &exec_args("cargo", &["test"])));
        assert!(grants.covers(chat, "exec", kind, &exec_args("cargo", &["publish"])));
        assert!(!grants.covers(chat, "exec", kind, &exec_args("rm", &["-rf"])));
    }

    #[test]
    fn a_whole_tool_grant_covers_an_action_it_cannot_read() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(grant(chat, "exec"));

        assert!(grants.covers(chat, "exec", kind, &exec_args("rm", &["-rf"])));
        assert!(grants.covers(chat, "exec", kind, &no_args()));
    }

    #[test]
    fn the_ladder_runs_narrowest_first_and_never_names_one_grant_twice() {
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("cargo", &["test"])),
            vec![
                GrantScope::ExactCommand {
                    command: "cargo".into(),
                    args: vec!["test".into()],
                    cwd: ".".into(),
                },
                GrantScope::AnyArgsFor {
                    command: "cargo".into(),
                },
                GrantScope::WholeTool,
            ]
        );
        // With no arguments, "exactly this" and "any arguments to this" would
        // be two names for the same grant.
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("true", &[])),
            vec![
                GrantScope::ExactCommand {
                    command: "true".into(),
                    args: vec![],
                    cwd: ".".into(),
                },
                GrantScope::WholeTool,
            ]
        );
        // Nothing to describe means nothing narrower to offer.
        assert_eq!(
            GrantScope::ladder_for("search", &no_args()),
            vec![GrantScope::WholeTool]
        );
    }

    #[test]
    fn narrow_grants_for_the_same_tool_accumulate_rather_than_collapse() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        for command in ["cargo", "git"] {
            grants.record(
                StandingGrant::scoped(
                    chat,
                    "exec",
                    kind,
                    GrantScope::AnyArgsFor {
                        command: command.into(),
                    },
                    Utc::now(),
                )
                .unwrap(),
            );
        }
        assert_eq!(grants.read().len(), 2);
        assert!(grants.covers(chat, "exec", kind, &exec_args("git", &["log"])));
        assert!(!grants.covers(chat, "exec", kind, &exec_args("npm", &["i"])));
    }

    #[test]
    fn recording_is_idempotent_and_revocable() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("search");
        let grants = StandingGrants::new();
        grants.record(grant(chat, "search"));
        grants.record(grant(chat, "search"));
        assert_eq!(grants.read().len(), 1);
        assert!(grants.covers(chat, "search", kind, &no_args()));

        grants.clear();
        assert!(!grants.covers(chat, "search", kind, &no_args()));
    }
}
