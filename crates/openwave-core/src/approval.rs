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
use ts_rs::TS;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalKind {
    /// A library search may share the query and matched excerpts with the
    /// provider.
    SearchMayShareQueryAndExcerpts,
    /// A public-web search may share the query and explicit filters with the
    /// host's configured provider.
    WebSearchMayShareQuery,
    /// A public-web page fetch may share the exact URL with the page's site or
    /// the host's configured provider.
    WebExtractMayFetchUrl,
    /// A command execution escapes the chat workspace and may reach the network.
    ExecMayRunNetworkedCommand,
    /// An external MCP process may receive the call and act with its own local
    /// or remote authority. Calls are approvable once but never standing-
    /// grantable because a Settings edit can replace the process behind a
    /// stable tool namespace.
    ExternalMcpMayCallServer,
    /// A `Workspace`-class action may create or modify files inside the chat
    /// workspace. Gated only in [`crate::PermissionMode::Ask`]; approvable
    /// once but not standing-grantable, because "stop asking about workspace
    /// edits here" is exactly what switching the chat to `Auto` says.
    WorkspaceMayModifyFiles,
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
            "web_extract" => Self::WebExtractMayFetchUrl,
            "exec" => Self::ExecMayRunNetworkedCommand,
            "write_file" | "create_deliverable" => Self::WorkspaceMayModifyFiles,
            name if name.starts_with("mcp__") => Self::ExternalMcpMayCallServer,
            _ => Self::Unsupported,
        }
    }

    /// The consent semantics for a call, given the class its tool declares.
    ///
    /// Extends [`Self::for_tool_name`] with the one classification a name
    /// alone cannot make: any `Workspace`-class tool without its own named
    /// kind consents as a workspace edit, so a tool added later gates
    /// correctly in `Ask` mode without registering its name here. Recovery of
    /// a parked call re-derives the kind from the stored name only, so a
    /// workspace tool unknown to [`Self::for_tool_name`] recovers as
    /// `Unsupported` (rejectable-only) after a restart — fail-closed, and the
    /// same degradation the folded database spellings already accept.
    #[must_use]
    pub fn for_call(name: &str, class: crate::tool::ApprovalClass) -> Self {
        match Self::for_tool_name(name) {
            Self::Unsupported if matches!(class, crate::tool::ApprovalClass::Workspace) => {
                Self::WorkspaceMayModifyFiles
            }
            kind => kind,
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
            // Same closed-constraint fold as web search: the exact tool name
            // stored beside it recovers the page-fetch kind on read.
            Self::WebExtractMayFetchUrl => "search_may_share_query_and_excerpts",
            Self::ExecMayRunNetworkedCommand => "exec_may_run_networked_command",
            // Existing databases have a closed approval-kind constraint. The
            // exact namespaced tool name disambiguates this renderer kind when
            // durable state is read, as it already does for web search.
            Self::ExternalMcpMayCallServer => "search_may_share_query_and_excerpts",
            // Same closed-constraint fold: stored as the legacy spelling, and
            // the tool name stored beside it recovers the workspace kind. A
            // workspace tool the name table does not know recovers as a true
            // `Unsupported` — rejectable-only, never silently approvable.
            Self::WorkspaceMayModifyFiles => "unsupported",
            Self::Unsupported => "unsupported",
        }
    }

    /// Stable representation for a standing-grant row.
    ///
    /// This is distinct from [`Self::as_str`], whose legacy database spelling
    /// intentionally folds web search and external MCP into the original
    /// search vocabulary. A remembered grant freezes the consent semantics it
    /// was created under, so its durable key must not make that distinction
    /// depend on the tool name at recovery time.
    #[must_use]
    pub const fn standing_grant_key(self) -> &'static str {
        match self {
            Self::SearchMayShareQueryAndExcerpts => "search_may_share_query_and_excerpts",
            Self::WebSearchMayShareQuery => "web_search_may_share_query",
            Self::WebExtractMayFetchUrl => "web_extract_may_fetch_url",
            Self::ExecMayRunNetworkedCommand => "exec_may_run_networked_command",
            Self::ExternalMcpMayCallServer => "external_mcp_may_call_server",
            // Never stored: the kind is not standing-grantable (the chat's
            // permission mode is the wider consent). Named anyway so the key
            // vocabulary stays total.
            Self::WorkspaceMayModifyFiles => "workspace_may_modify_files",
            Self::Unsupported => "unsupported",
        }
    }

    /// Recover the frozen consent semantics stored beside a standing grant.
    #[must_use]
    pub fn from_standing_grant_key(value: &str) -> Option<Self> {
        match value {
            "search_may_share_query_and_excerpts" => Some(Self::SearchMayShareQueryAndExcerpts),
            "web_search_may_share_query" => Some(Self::WebSearchMayShareQuery),
            "web_extract_may_fetch_url" => Some(Self::WebExtractMayFetchUrl),
            "exec_may_run_networked_command" => Some(Self::ExecMayRunNetworkedCommand),
            "external_mcp_may_call_server" => Some(Self::ExternalMcpMayCallServer),
            "workspace_may_modify_files" => Some(Self::WorkspaceMayModifyFiles),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }

    #[must_use]
    pub const fn is_approvable(self) -> bool {
        matches!(
            self,
            Self::SearchMayShareQueryAndExcerpts
                | Self::WebSearchMayShareQuery
                | Self::WebExtractMayFetchUrl
                | Self::ExecMayRunNetworkedCommand
                | Self::ExternalMcpMayCallServer
                | Self::WorkspaceMayModifyFiles
        )
    }

    /// Whether approval may be remembered for later calls in the same chat.
    ///
    /// MCP is intentionally one-shot: its configured executable can change
    /// while retaining a model-visible namespace, so reusing consent by name
    /// would silently widen authority. Workspace edits are one-shot for the
    /// opposite reason: their standing "yes" already exists as the chat's
    /// `Auto` permission mode, and a second spelling of the same consent
    /// would drift from the first.
    #[must_use]
    pub const fn is_standing_grantable(self) -> bool {
        self.is_approvable()
            && !matches!(
                self,
                Self::ExternalMcpMayCallServer | Self::WorkspaceMayModifyFiles
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
    /// Whether a durable standing grant, rather than a fresh click, authorized
    /// this call. The source is retained so crash recovery does not invent an
    /// approval-card decision for an automatically authorized call.
    pub approved_by_standing_grant: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GrantScope {
    /// Exactly the action the card showed, and nothing else.
    ///
    /// Holding the projection itself rather than one tool's fields is what
    /// keeps this from being an `exec` scope wearing a general name: every tool
    /// that can describe its action exactly gets a rung below the whole tool,
    /// instead of every non-`exec` approval offering "allow all of these" as
    /// its only standing choice.
    ExactAction(ToolActionPreview),
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
        if matches!(self, Self::WholeTool) {
            return true;
        }
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            return false;
        }
        let Some(action) = ToolActionPreview::build(tool_name, arguments) else {
            return false;
        };
        match self {
            Self::WholeTool => true,
            Self::ExactAction(granted) => *granted == action,
            Self::AnyArgsFor { command } => matches!(
                &action,
                ToolActionPreview::Exec { command: called, .. } if called == command
            ),
        }
    }

    /// The scopes offered for a call, narrowest first.
    ///
    /// A call with nothing to describe — or nothing describable *exactly* —
    /// can only be granted wholesale, because there is no narrower thing that
    /// could be named without naming it approximately. Any other call gets the
    /// exact rung: a search that has to keep asking about the query it already
    /// asked about is offered "allow every search in this chat" as its only
    /// alternative, which is the widest thing on the ladder.
    #[must_use]
    pub fn ladder_for(tool_name: &str, arguments: &serde_json::Value) -> Vec<Self> {
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            return vec![Self::WholeTool];
        }
        let Some(action) = ToolActionPreview::build(tool_name, arguments) else {
            return vec![Self::WholeTool];
        };
        let mut ladder = vec![Self::ExactAction(action.clone())];
        // A command is the one action with a rung between "this exact call" and
        // "every call": the executable it runs. With no arguments even that
        // would read as two names for one grant.
        if let ToolActionPreview::Exec { command, args, .. } = &action {
            if !args.is_empty() {
                ladder.push(Self::AnyArgsFor {
                    command: command.clone(),
                });
            }
        }
        ladder.push(Self::WholeTool);
        ladder
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
    pub(crate) fn covers(
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

/// A durable standing grant as the revocation surface reads it: the grant
/// itself plus the approval decision that created it, which is also the row's
/// identity and therefore the handle a revocation names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingGrantRecord {
    /// The call whose approval created this grant (primary key).
    pub source_call_id: CallId,
    pub grant: StandingGrant,
}

/// Explicit in-memory grants for isolated, non-durable callers such as the
/// transport-agnostic MCP server test harness. The foreground server does not
/// use this type: it resolves grants through [`crate::Store`] while holding the
/// approval transaction's chat lock.
#[derive(Debug, Default)]
pub struct StandingGrants {
    grants: RwLock<Vec<StandingGrant>>,
}

impl StandingGrants {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_grants(grants: Vec<StandingGrant>) -> Self {
        Self {
            grants: RwLock::new(grants),
        }
    }

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
    /// A durable standing grant authorized this exact call. No approval card or
    /// decision event is emitted because no new human decision was needed.
    StandingGrant,
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

    /// The exact-action scope for a command run in the default directory.
    fn exact_command(command: &str, args: &[&str]) -> GrantScope {
        GrantScope::ExactAction(ToolActionPreview::Exec {
            command: command.into(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: ".".into(),
        })
    }

    /// The exact-action scope for an unfiltered public web search.
    fn exact_web_search(query: &str) -> GrantScope {
        GrantScope::ExactAction(ToolActionPreview::WebSearch {
            query: query.into(),
            domains: Vec::new(),
            start_published_at: None,
            end_published_at: None,
        })
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
                exact_command("cargo", &["test"]),
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
                exact_command("bash", &["-c", &long]),
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
                exact_command("foo", &["bar"]),
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
                GrantScope::ExactAction(ToolActionPreview::Exec {
                    command: "npm".into(),
                    args: vec!["install".into()],
                    cwd: "./sandbox".into(),
                }),
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
                exact_command("cargo", &["test"]),
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
            vec![exact_command("true", &[]), GrantScope::WholeTool,]
        );
        // Nothing to describe means nothing narrower to offer.
        assert_eq!(
            GrantScope::ladder_for("search", &no_args()),
            vec![GrantScope::WholeTool]
        );
    }

    #[test]
    fn a_search_is_offered_a_grant_narrower_than_every_search() {
        // The ladder used to be `exec`-shaped, so approving a Sensitive search
        // offered "don't ask again about searches" as the only standing choice
        // — the widest rung there is, from a card that had already shown the
        // one query the person was actually deciding about.
        assert_eq!(
            GrantScope::ladder_for(
                "web_search",
                &serde_json::json!({ "query": "quarterly filings" })
            ),
            vec![exact_web_search("quarterly filings"), GrantScope::WholeTool,]
        );
        // A query is one thing, so there is no middle rung between it and the
        // whole tool; only a command has an executable to name.
        assert_eq!(
            GrantScope::ladder_for("search", &serde_json::json!({ "query": "revenue" })),
            vec![
                GrantScope::ExactAction(ToolActionPreview::Search {
                    query: "revenue".into()
                }),
                GrantScope::WholeTool,
            ]
        );
    }

    #[test]
    fn a_query_grant_covers_that_query_and_no_other_search() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("web_search");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                chat,
                "web_search",
                kind,
                exact_web_search("quarterly filings"),
                Utc::now(),
            )
            .unwrap(),
        );

        let query = |query: &str| serde_json::json!({ "query": query });
        assert!(grants.covers(chat, "web_search", kind, &query("quarterly filings")));
        // The tool trims before searching, so padding is not a different search.
        assert!(grants.covers(chat, "web_search", kind, &query("  quarterly filings ")));
        assert!(!grants.covers(chat, "web_search", kind, &query("payroll")));
        // Same query, different tool: a grant is scoped to the tool it was
        // given for, and a private-source search is not a public web search.
        assert!(!grants.covers(
            chat,
            "search",
            ToolApprovalKind::for_tool_name("search"),
            &query("quarterly filings"),
        ));
        // The filters go to the provider too, so the same query with a domain
        // filter is a different disclosure and was never the one granted.
        assert!(!grants.covers(
            chat,
            "web_search",
            kind,
            &serde_json::json!({ "query": "quarterly filings", "domains": ["sec.gov"] }),
        ));
        // Bounding the response is not part of what leaves the machine.
        assert!(grants.covers(
            chat,
            "web_search",
            kind,
            &serde_json::json!({ "query": "quarterly filings", "max_results": 10 }),
        ));
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
