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
use crate::id::{CallId, ChatId, ProjectId, TurnId};
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
/// arguments. `Unsupported` is the fail-closed default: a Sensitive
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
    /// A background agent may run commands in its own workspace and reach the
    /// network under this chat's policy, without any of its own calls coming
    /// back for approval.
    ///
    /// Consent is given once, for the whole run: nobody is watching a
    /// background run, so a mid-run card would stall it against its own
    /// deadline. What the reader is deciding is egress — the run's workspace
    /// is keyed by its own id and carries no folder grants, staged host paths,
    /// or chat attachments — so the card names the network policy the child
    /// inherits.
    DelegateMayRunBackgroundAgent,
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
            "write_file" => Self::WorkspaceMayModifyFiles,
            // `create_app` writes app revisions, a `Workspace`-class effect.
            // Recovery of a parked call re-derives the kind from this table
            // alone, so a workspace tool missing here parks as an approvable
            // card the renderer presents and then 409s the approval itself.
            "create_app" => Self::WorkspaceMayModifyFiles,
            crate::SPAWN_SANDBOX_AGENT_TOOL => Self::DelegateMayRunBackgroundAgent,
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
            // Same closed-constraint fold. `approval_from_model` recovers the
            // delegation kind from the exact tool name stored beside it.
            Self::DelegateMayRunBackgroundAgent => "unsupported",
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
            Self::WorkspaceMayModifyFiles => "workspace_may_modify_files",
            Self::DelegateMayRunBackgroundAgent => "delegate_may_run_background_agent",
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
            "delegate_may_run_background_agent" => Some(Self::DelegateMayRunBackgroundAgent),
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
                | Self::DelegateMayRunBackgroundAgent
        )
    }

    /// Whether a call of this kind may ever be offered to the Auto-mode
    /// judge instead of parking straight on the human card.
    ///
    /// A necessary condition, not a sufficient one — see
    /// [`is_auto_judge_candidate`], which additionally requires a command to
    /// have cleared the analyzer. The judge must never be the only thing
    /// standing between a call and its effect; it may shorten the path to
    /// yes for something already known to be structurally benign, and
    /// nothing else.
    ///
    /// MCP stays out on the same replaceable-executable grounds as standing
    /// grants: consent given to a namespace is not consent to whatever
    /// Settings later puts behind it. Delegation stays out because the judge
    /// would be deciding a whole unattended run rather than one call.
    #[must_use]
    pub const fn is_auto_judgeable(self) -> bool {
        matches!(
            self,
            Self::SearchMayShareQueryAndExcerpts
                | Self::WebSearchMayShareQuery
                | Self::WebExtractMayFetchUrl
                | Self::ExecMayRunNetworkedCommand
        )
    }

    /// Whether approval may be remembered for later calls in the same chat.
    ///
    /// MCP is intentionally one-shot: its configured executable can change
    /// while retaining a model-visible namespace, so reusing consent by name
    /// would silently widen authority. Workspace edits are grantable, but only
    /// about a place ([`GrantScope::PathSubtree`], enforced by
    /// [`Self::grantable_at`]): their whole-tool "yes" already exists as the
    /// chat's `Auto` permission mode, and a second spelling of the same
    /// consent would drift from the first.
    #[must_use]
    pub const fn is_standing_grantable(self) -> bool {
        self.is_approvable() && !matches!(self, Self::ExternalMcpMayCallServer)
    }

    /// Whether a standing grant of `scope` may exist for this kind.
    ///
    /// The place rung and the shape rungs answer different questions — where
    /// a write may land versus what a command may look like — so each kind
    /// admits only the rungs that describe its own action. A workspace edit
    /// can be granted about a place and nothing wider; every other grantable
    /// kind keeps the shape rungs and cannot borrow the place one.
    #[must_use]
    pub const fn grantable_at(self, scope: &GrantScope) -> bool {
        if !self.is_standing_grantable() {
            return false;
        }
        match self {
            Self::WorkspaceMayModifyFiles => matches!(scope, GrantScope::PathSubtree { .. }),
            _ => !matches!(scope, GrantScope::PathSubtree { .. }),
        }
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

/// Where the Auto-mode judge stands on one parked call.
///
/// The marker is load-bearing for the renderer: without it, "the judge is
/// still deciding" and "the judge declined, a human is needed" are both just
/// `Pending`, indistinguishable except by waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AutoJudgeStatus {
    /// A judge owns this call; its verdict or failure will move the marker.
    Judging,
    /// The judge approved. Doubles as the "decided automatically" badge and
    /// as the guard that a later human click cannot be mislabeled as one.
    Approved,
    /// The judge declined (or failed — failure is a decline). The card is a
    /// human's to decide; the marker never returns to `Judging`.
    Declined,
}

impl AutoJudgeStatus {
    /// Stable database representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Judging => "judging",
            Self::Approved => "approved",
            Self::Declined => "declined",
        }
    }
}

/// Whether an uncovered call may be handed to the Auto-mode judge.
///
/// Three things have to hold, and the third is the one that matters. The
/// kind must be judgeable at all; the action must be describable *exactly*,
/// so the judge sees the real call rather than a clamped rendering of it;
/// and a command must additionally have cleared the deterministic analyzer
/// under the broadest possible rule.
///
/// That last condition is what makes judging a command defensible, and it is
/// worth stating exactly what it buys. Under the blanket `All` rule the
/// analyzer refuses interpreters, destructive operations, sensitive reads and
/// writes, anything reaching outside the folder — and, because a blanket rule
/// names no program, every script executor and package installer:
/// `python script.py`, `node server.js`, `pip install x` never reach the
/// model, whatever their arguments look like. So what is judged is a call to
/// a named program with ordinary operands, and a model that answers badly can
/// only fail towards asking. It cannot widen what the floor already refused.
///
/// A rule the person actually wrote — "always allow `python`" — still covers
/// those commands. The refusal here is about a rule nobody wrote.
#[must_use]
pub fn is_auto_judge_candidate(
    kind: ToolApprovalKind,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> bool {
    if !kind.is_auto_judgeable() || !ToolActionPreview::describes_exactly(tool_name, arguments) {
        return false;
    }
    let Some(action) = ToolActionPreview::build(tool_name, arguments) else {
        return false;
    };
    match &action {
        ToolActionPreview::Exec { command, args, .. } => {
            let broadest = openwave_shell_policy::ShellRuleSet {
                allow: openwave_shell_policy::CommandRule::new(
                    openwave_shell_policy::CommandRuleKind::All,
                    Vec::new(),
                )
                .into_iter()
                .collect(),
                deny: Vec::new(),
            };
            openwave_shell_policy::analyze_argv(&exec_argv(command, args), &broadest).verdict
                == openwave_shell_policy::ShellVerdict::Allow
        }
        // A query carries no effect of its own; the egress it describes is
        // the whole of what is being judged.
        _ => true,
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
    /// Where the Auto-mode judge stands on this call, when one was engaged.
    pub auto_judge_status: Option<AutoJudgeStatus>,
    pub status: ToolApprovalStatus,
    pub reason: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl ToolApproval {
    /// Reject reason when the reader gave none.
    ///
    /// The model reads this verbatim, so it carries the recovery guidance a
    /// silent reader would otherwise leave implicit: retrying the exact call
    /// just re-asks the question the reader already answered.
    pub const DEFAULT_REJECT_REASON: &'static str = "The user declined to approve this action. \
        Do not retry the same action; propose a different approach, ask the user why, or \
        continue without it.";

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
                    .unwrap_or_else(|| Self::DEFAULT_REJECT_REASON.into()),
            }),
        }
    }
}

/// How far a standing grant reaches.
///
/// A grant used to cover exactly one chat, so "always allow `cargo`" was
/// re-asked in the next conversation and the one after it — the single
/// biggest source of prompting in the model. The level is chosen from where
/// the chat lives rather than put to the reader as a question: a chat in a
/// project grants across that project, and a loose chat has nothing wider to
/// mean, so it grants for itself. The card states which one it is about to
/// write; a grant nobody expected is the failure the ladder exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, TS)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum GrantLevel {
    /// Every later matching call in one chat.
    Chat { chat_id: ChatId },
    /// Every later matching call in any chat filed under one project.
    Project { project_id: ProjectId },
}

impl GrantLevel {
    /// The level a grant made from this chat should be written at.
    #[must_use]
    pub const fn for_chat(chat_id: ChatId, project_id: Option<ProjectId>) -> Self {
        match project_id {
            Some(project_id) => Self::Project { project_id },
            None => Self::Chat { chat_id },
        }
    }

    /// Whether this level reaches a call made in `chat_id` under
    /// `project_id`.
    #[must_use]
    pub fn reaches(self, chat_id: ChatId, project_id: Option<ProjectId>) -> bool {
        match self {
            Self::Chat { chat_id: granted } => granted == chat_id,
            // A project grant covers the project it names, and only a chat
            // that is actually filed under it.
            Self::Project {
                project_id: granted,
            } => project_id == Some(granted),
        }
    }
}

/// A remembered approval that lets a repeated in-scope Sensitive action run
/// without re-prompting.
///
/// Deny-by-default, like the host broker's capability grants: a grant covers
/// exactly one chat (or project) and one approvable tool, narrowed further by
/// its [`GrantScope`] — including [`GrantScope::PathSubtree`], the rung that
/// names a workspace place now that [`ApprovalRequest`] carries a structured
/// resource ([`ToolActionPreview::WriteFile`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingGrant {
    level: GrantLevel,
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
    /// A leading run of argv tokens: the executable plus its subcommand
    /// chain, with any arguments after it.
    ///
    /// The rung people actually reach for — "any `cargo test`", not "any
    /// `cargo`". It is matched token-wise rather than as a string prefix, and
    /// only after the command has cleared the analyzer's floor, so a grant
    /// for `cargo test` cannot be stretched to something that merely starts
    /// with those letters or that smuggles a shell in behind them.
    CommandPrefix { tokens: Vec<String> },
    /// Every write landing in one workspace place: the named path itself, or
    /// anything below it.
    ///
    /// This is the rung that says *where* rather than *what* — "stop asking
    /// about writes under `reports/`", not "stop asking about this document".
    /// The prefix is a run of workspace-relative path segments, matched
    /// segment-wise against the canonical `path` argument so `reports/` can
    /// never be stretched over `reports-old/`, and only against a path the
    /// preview reproduced in full
    /// ([`ToolActionPreview::names_place_exactly`]).
    PathSubtree { prefix: String },
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
        // A place is matched on the canonical path argument, not on the
        // projection: the write's document never reaches the preview, so the
        // whole-call fidelity gate below can never pass for it, and does not
        // need to — consent about a place is indifferent to the document.
        if let Self::PathSubtree { prefix } = self {
            return covers_workspace_place(prefix, tool_name, arguments);
        }
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            // Nothing describable to match, so only the widest scope can
            // apply — and it applies to a call the renderer could not show,
            // which is exactly what granting the whole tool agreed to.
            return matches!(self, Self::WholeTool);
        }
        let Some(action) = ToolActionPreview::build(tool_name, arguments) else {
            return matches!(self, Self::WholeTool);
        };
        // A command is matched against its argv rather than its rendering,
        // and only once the analyzer says the call is safe to auto-run under
        // this very rule. That check is what makes a widened rung honest: a
        // grant for `cargo` stops covering `cargo` the moment the arguments
        // reach somewhere they should not, instead of covering it forever
        // because the executable still matches.
        if let ToolActionPreview::Exec { command, args, .. } = &action {
            return self.covers_argv(&action, command, args);
        }
        match self {
            Self::WholeTool => true,
            Self::ExactAction(granted) => *granted == action,
            // Only a command has an executable or a token run to name.
            Self::AnyArgsFor { .. } | Self::CommandPrefix { .. } => false,
            // Handled on canonical arguments above, before the fidelity gate.
            Self::PathSubtree { .. } => unreachable!("place scopes return early"),
        }
    }

    /// Whether this scope authorizes one command invocation.
    ///
    /// Two independent questions, in order. Does the scope *name* this call —
    /// for the exact rung that still means the whole projection, working
    /// directory included, because "exactly this" was said about an action in
    /// a place. And would the analyzer auto-run it under that rule — the
    /// floor, which no rung escapes, so an interpreter or a path that climbs
    /// out keeps asking however broadly it was granted.
    fn covers_argv(&self, action: &ToolActionPreview, command: &str, args: &[String]) -> bool {
        if let Self::ExactAction(granted) = self {
            if granted != action {
                return false;
            }
        }
        let rule = match self {
            Self::WholeTool => openwave_shell_policy::CommandRule::new(
                openwave_shell_policy::CommandRuleKind::All,
                Vec::new(),
            ),
            Self::AnyArgsFor { command } => openwave_shell_policy::CommandRule::new(
                openwave_shell_policy::CommandRuleKind::Prefix,
                vec![command.clone()],
            ),
            Self::CommandPrefix { tokens } => openwave_shell_policy::CommandRule::new(
                openwave_shell_policy::CommandRuleKind::Prefix,
                tokens.clone(),
            ),
            // The projection already matched above, so the analyzer is asked
            // only whether this invocation clears the floor.
            Self::ExactAction(_) => openwave_shell_policy::CommandRule::new(
                openwave_shell_policy::CommandRuleKind::Exact,
                exec_argv(command, args),
            ),
            // A place names workspace writes, never a command.
            Self::PathSubtree { .. } => unreachable!("place scopes return early"),
        };
        let Ok(rule) = rule else {
            return false;
        };
        let ruleset = openwave_shell_policy::ShellRuleSet {
            allow: vec![rule],
            deny: Vec::new(),
        };
        openwave_shell_policy::analyze_argv(&exec_argv(command, args), &ruleset).verdict
            == openwave_shell_policy::ShellVerdict::Allow
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
        // A workspace write never describes itself exactly — the document is
        // not projected — so its ladder is built from the place instead, and
        // only when the path arrived intact. Falling through to the whole-tool
        // default would offer the one workspace rung that must not exist: the
        // chat's `Auto` mode already spells that consent.
        if tool_name == "write_file" {
            if !ToolActionPreview::names_place_exactly(tool_name, arguments) {
                return Vec::new();
            }
            return match ToolActionPreview::build(tool_name, arguments) {
                Some(action) => Self::ladder_for_action(&action),
                None => Vec::new(),
            };
        }
        if !ToolActionPreview::describes_exactly(tool_name, arguments) {
            return vec![Self::WholeTool];
        }
        let Some(action) = ToolActionPreview::build(tool_name, arguments) else {
            return vec![Self::WholeTool];
        };
        Self::ladder_for_action(&action)
    }

    /// The rungs of [`Self::ladder_for`] a grant of `kind` may actually hold,
    /// per [`ToolApprovalKind::grantable_at`]. This is the ladder a card may
    /// offer: a rung appears only when granting it would mint.
    #[must_use]
    pub fn mintable_ladder_for(
        kind: ToolApprovalKind,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Vec<Self> {
        let mut ladder = Self::ladder_for(tool_name, arguments);
        ladder.retain(|scope| kind.grantable_at(scope));
        ladder
    }

    /// The ladder for an action already projected from a parked call.
    ///
    /// The same rungs [`Self::ladder_for`] offers, reachable from the durable
    /// action so a decision arriving later is rebuilt against exactly what
    /// the card showed rather than against anything the client sent.
    #[must_use]
    pub fn ladder_for_action(action: &ToolActionPreview) -> Vec<Self> {
        let action = action.clone();
        // A command's ladder is built by the analyzer rather than assembled
        // here, and every rung on it is verified: a rung appears only when
        // granting exactly that rule would in fact stop the asking. That is
        // what stops the card offering "always allow any `timeout`", which
        // the gate would then refuse to honor.
        if let ToolActionPreview::Exec { command, args, .. } = &action {
            let argv = exec_argv(command, args);
            let mut ladder: Vec<Self> = openwave_shell_policy::suggested_rungs_for_argv(&argv)
                .into_iter()
                .map(|rule| match rule.kind {
                    openwave_shell_policy::CommandRuleKind::Exact => {
                        Self::ExactAction(action.clone())
                    }
                    openwave_shell_policy::CommandRuleKind::Prefix => Self::CommandPrefix {
                        tokens: rule.tokens,
                    },
                    openwave_shell_policy::CommandRuleKind::All => Self::WholeTool,
                })
                .collect();
            // A command the analyzer will not auto-run under any rule has no
            // ladder at all, and offering one anyway would promise something
            // the gate refuses. Approving once stays available.
            ladder.dedup();
            return ladder;
        }
        // A write's ladder names places, narrowest first: exactly this path,
        // then the directory that holds it. There is deliberately no
        // whole-workspace rung — that consent already exists as `Auto` mode.
        if let ToolActionPreview::WriteFile { path } = &action {
            return place_ladder(path);
        }
        // Delegation is consented to as delegation. A task is model-authored
        // prose, so an exact-action grant would never match a second time and
        // would only be a rung that looks narrower than it is.
        if matches!(action, ToolActionPreview::DelegateAgent { .. }) {
            return vec![Self::WholeTool];
        }
        vec![Self::ExactAction(action), Self::WholeTool]
    }
}

/// One command invocation as the analyzer reads it.
fn exec_argv(command: &str, args: &[String]) -> Vec<String> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(command.to_owned());
    argv.extend(args.iter().cloned());
    argv
}

/// The canonical segments of a workspace-relative path, or `None` for a path
/// no place grant should reason about.
///
/// Canonical means what the workspace itself would resolve: empty and `.`
/// segments dropped, so a grant for `reports` covers `./reports/q1.md` rather
/// than being dodged by a cosmetic respelling. Anything that could point
/// outside the workspace — an absolute path, a `..` — yields `None`, and a
/// call carrying one keeps asking instead of matching anything.
fn place_segments(path: &str) -> Option<Vec<&str>> {
    if path.starts_with('/') {
        return None;
    }
    let segments: Vec<&str> = path
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if segments.is_empty() || segments.contains(&"..") {
        return None;
    }
    Some(segments)
}

/// Whether a place grant's prefix covers the workspace write about to run.
///
/// Matched on the canonical `path` argument, segment-wise — `reports` never
/// covers `reports-old/q1.md` — and only when the path reached the preview
/// intact, so a grant given for the place the card showed cannot be stretched
/// over paths that merely clamp to it.
fn covers_workspace_place(prefix: &str, tool_name: &str, arguments: &serde_json::Value) -> bool {
    if !ToolActionPreview::names_place_exactly(tool_name, arguments) {
        return false;
    }
    let Some(path) = arguments.get("path").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let (Some(prefix), Some(path)) = (place_segments(prefix), place_segments(path)) else {
        return false;
    };
    path.len() >= prefix.len()
        && prefix
            .iter()
            .zip(path.iter())
            .all(|(granted, requested)| granted == requested)
}

/// The place rungs for one workspace write, narrowest first.
///
/// Rebuilt from the (possibly clamped) preview on recovery, which cannot say
/// whether a path exactly at the clamp bound was truncated — so a path at the
/// bound gets no ladder, and no rung is ever offered for an approximate place.
fn place_ladder(path: &str) -> Vec<GrantScope> {
    if path.chars().count() >= crate::preview::MAX_ACTION_FIELD_CHARS {
        return Vec::new();
    }
    let Some(segments) = place_segments(path) else {
        return Vec::new();
    };
    let mut ladder = vec![GrantScope::PathSubtree {
        prefix: segments.join("/"),
    }];
    if segments.len() > 1 {
        ladder.push(GrantScope::PathSubtree {
            prefix: segments[..segments.len() - 1].join("/"),
        });
    }
    ladder
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
        level: GrantLevel,
        tool_name: impl Into<String>,
        kind: ToolApprovalKind,
        granted_at: DateTime<Utc>,
    ) -> Option<Self> {
        Self::scoped(level, tool_name, kind, GrantScope::WholeTool, granted_at)
    }

    /// Record consent limited to `scope`.
    ///
    /// Returns `None` on the same terms as [`StandingGrant::new`], and also
    /// for a kind/scope pairing [`ToolApprovalKind::grantable_at`] refuses —
    /// a workspace edit can be granted about a place and nothing wider.
    #[must_use]
    pub fn scoped(
        level: GrantLevel,
        tool_name: impl Into<String>,
        kind: ToolApprovalKind,
        scope: GrantScope,
        granted_at: DateTime<Utc>,
    ) -> Option<Self> {
        if !kind.grantable_at(&scope) {
            return None;
        }
        Some(Self {
            level,
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

    /// How far this standing consent reaches.
    #[must_use]
    pub const fn level(&self) -> GrantLevel {
        self.level
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
        project_id: Option<ProjectId>,
        tool_name: &str,
        kind: ToolApprovalKind,
        arguments: &serde_json::Value,
    ) -> bool {
        self.level.reaches(chat_id, project_id)
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
            existing.level == grant.level
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
        project_id: Option<ProjectId>,
        tool_name: &str,
        kind: ToolApprovalKind,
        arguments: &serde_json::Value,
    ) -> bool {
        self.read()
            .iter()
            .any(|grant| grant.covers(chat_id, project_id, tool_name, kind, arguments))
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
    /// Whether the Auto-mode judge should own this call once parked. Stamped
    /// inside the park transaction so the renderer never flashes a bare human
    /// card before the judge's placeholder. Storage refuses the flag for any
    /// kind that is not [`ToolApprovalKind::is_auto_judgeable`].
    pub auto_judge: bool,
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
            GrantLevel::Chat { chat_id },
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
        assert!(grants.covers(chat, None, "exec", kind, &no_args()));
        // Deny-by-default still holds for a different chat.
        assert!(!grants.covers(ChatId::new(), None, "exec", kind, &no_args()));
    }

    #[test]
    fn external_mcp_is_approvable_once_but_never_standing_grantable() {
        let kind = ToolApprovalKind::for_tool_name("mcp__documents__search");
        assert_eq!(kind, ToolApprovalKind::ExternalMcpMayCallServer);
        assert!(kind.is_approvable());
        assert!(!kind.is_standing_grantable());
        assert!(StandingGrant::new(
            GrantLevel::Chat {
                chat_id: ChatId::new()
            },
            "mcp__documents__search",
            kind,
            Utc::now(),
        )
        .is_none());
    }

    #[test]
    fn non_approvable_tools_cannot_be_granted() {
        assert!(StandingGrant::new(
            GrantLevel::Chat {
                chat_id: ChatId::new()
            },
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
            None,
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

        assert!(grants.covers(chat, None, "search", kind, &no_args()));
        assert!(!grants.covers(other_chat, None, "search", kind, &no_args()));
        assert!(!grants.covers(
            chat,
            None,
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
            files: Vec::new(),
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
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                exact_command("cargo", &["test"]),
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, None, "exec", kind, &exec_args("cargo", &["test"])));
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("cargo", &["publish"])));
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("rm", &["test"])));
        // An action the renderer could not describe was never the one granted.
        assert!(!grants.covers(chat, None, "exec", kind, &no_args()));
    }

    /// Staging is part of the action, and a grant retained from before the
    /// projection carried it must not stretch over calls that stage files.
    #[test]
    fn a_grant_that_named_no_staged_files_does_not_cover_a_call_that_stages_them() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        // How a grant stored before `files` existed reads back: the field is
        // absent from the persisted scope, so it defaults rather than failing
        // the row's load.
        let stored = serde_json::json!({
            "scope": "exact_action",
            "tool": "exec",
            "command": "cat",
            "args": ["notes.txt"],
            "cwd": "."
        });
        let scope: GrantScope = serde_json::from_value(stored).expect("a retained scope loads");
        grants.record(
            StandingGrant::scoped(
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                scope,
                Utc::now(),
            )
            .unwrap(),
        );

        // The call it was given for still runs unprompted.
        assert!(grants.covers(chat, None, "exec", kind, &exec_args("cat", &["notes.txt"])));
        // The same command handed a document is a different action, and the
        // retained grant does not reach it.
        assert!(!grants.covers(
            chat,
            None,
            "exec",
            kind,
            &serde_json::json!({
                "command": "cat",
                "args": ["notes.txt"],
                "files": ["documents/salaries.csv"]
            })
        ));
    }

    #[test]
    fn a_clamped_call_never_matches_or_creates_a_narrow_grant() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let long = "x".repeat(crate::preview::MAX_ACTION_FIELD_CHARS);
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                exact_command("echo", &[&long]),
                Utc::now(),
            )
            .unwrap(),
        );

        // Deliberately not an interpreter: the floor refuses those outright,
        // which would pass this test for a reason that has nothing to do with
        // the clamping it is here to check.
        //
        // Truncation: a longer argument sharing the granted prefix projects to
        // the same preview. Keying on the projection would run it unprompted.
        let appended = format!("{long}; rm -rf ~");
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("echo", &[&appended])));
        // A value exactly at the bound is not truncated, so it stays faithful
        // and the grant still covers the command it was actually given for.
        assert!(grants.covers(chat, None, "exec", kind, &exec_args("echo", &[&long])));
        // Nothing at or past the bound can be turned into a narrow grant.
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("echo", &[&appended])),
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
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                exact_command("foo", &["bar"]),
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, None, "exec", kind, &exec_args("foo", &["bar"])));
        // An empty argument used to clamp away entirely, making these two
        // different calls indistinguishable.
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("foo", &["", "bar"])));
        // A control character used to be stripped, so these collapsed too.
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("foo", &["b\u{0}ar"])));
        // A non-string argument used to be dropped, changing the call's arity.
        assert!(!grants.covers(
            chat,
            None,
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
            None,
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
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                GrantScope::ExactAction(ToolActionPreview::Exec {
                    command: "npm".into(),
                    args: vec!["install".into()],
                    cwd: "./sandbox".into(),
                    files: Vec::new(),
                }),
                Utc::now(),
            )
            .unwrap(),
        );

        let in_dir =
            |cwd: &str| serde_json::json!({ "command": "npm", "args": ["install"], "cwd": cwd });
        assert!(grants.covers(chat, None, "exec", kind, &in_dir("./sandbox")));
        // The card showed the directory, so the grant is about that directory.
        assert!(!grants.covers(chat, None, "exec", kind, &in_dir("/")));
    }

    #[test]
    fn an_executable_grant_covers_any_arguments_to_it() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                GrantScope::AnyArgsFor {
                    command: "cargo".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );

        assert!(grants.covers(chat, None, "exec", kind, &exec_args("cargo", &["test"])));
        assert!(grants.covers(chat, None, "exec", kind, &exec_args("cargo", &["publish"])));
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("rm", &["-rf"])));
    }

    /// No rung outruns the floor, including the widest one.
    ///
    /// "Don't ask again about commands" used to mean every command, so a
    /// grant taken for `cargo` also carried `rm -rf` and `bash -c`. It now
    /// means every command the analyzer would run on its own — a grant is a
    /// standing yes to the routine, never a blanket one.
    #[test]
    fn the_widest_grant_still_stops_at_the_floor() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("exec");
        let grants = StandingGrants::new();
        grants.record(grant(chat, "exec"));

        assert!(grants.covers(chat, None, "exec", kind, &exec_args("cargo", &["test"])));
        // Destructive, and an interpreter, and a path out of the workspace.
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("rm", &["-rf", "/"])));
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("bash", &["-c", "id"])));
        assert!(!grants.covers(
            chat,
            None,
            "exec",
            kind,
            &exec_args("cat", &["../../outside.txt"])
        ));
        // An action with nothing to read is still covered: that is what
        // granting the whole tool agreed to.
        assert!(grants.covers(chat, None, "exec", kind, &no_args()));
    }

    /// The judge never decides about code the agent could have written.
    ///
    /// A candidate has to clear the analyzer under a blanket rule, and a
    /// script executor used to clear it whenever its operands were
    /// workspace-relative. In Auto mode that closed a loop: `write_file`
    /// authors `script.py`, `exec python3 script.py` goes to a model, and no
    /// human sees either. It escalates now, while a grant naming the program
    /// still runs it without asking.
    #[test]
    fn a_script_execution_is_never_handed_to_the_judge() {
        let kind = ToolApprovalKind::for_tool_name("exec");
        assert!(!is_auto_judge_candidate(
            kind,
            "exec",
            &exec_args("python3", &["script.py"])
        ));
        assert!(!is_auto_judge_candidate(
            kind,
            "exec",
            &exec_args("pip", &["install", "requests"])
        ));
        // An ordinary command with nothing to run is still judgeable.
        assert!(is_auto_judge_candidate(
            kind,
            "exec",
            &exec_args("cargo", &["test"])
        ));

        let chat = ChatId::new();
        let grants = StandingGrants::new();
        grants.record(
            StandingGrant::scoped(
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                GrantScope::AnyArgsFor {
                    command: "python3".into(),
                },
                Utc::now(),
            )
            .unwrap(),
        );
        grants.record(
            StandingGrant::scoped(
                GrantLevel::Chat { chat_id: chat },
                "exec",
                kind,
                exact_command("pip", &["install", "requests"]),
                Utc::now(),
            )
            .unwrap(),
        );
        assert!(grants.covers(
            chat,
            None,
            "exec",
            kind,
            &exec_args("python3", &["script.py"])
        ));
        assert!(grants.covers(
            chat,
            None,
            "exec",
            kind,
            &exec_args("pip", &["install", "requests"])
        ));
    }

    #[test]
    fn the_ladder_runs_narrowest_first_and_never_names_one_grant_twice() {
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("cargo", &["test"])),
            vec![
                exact_command("cargo", &["test"]),
                // The rung this ladder previously could not offer: the
                // subcommand, not the whole executable.
                GrantScope::CommandPrefix {
                    tokens: vec!["cargo".into(), "test".into()],
                },
                GrantScope::CommandPrefix {
                    tokens: vec!["cargo".into()],
                },
                GrantScope::WholeTool,
            ]
        );
        // With no arguments the two narrow rungs are still different grants:
        // "exactly `true`" runs only that, while the prefix also covers
        // `true --whatever`.
        assert_eq!(
            GrantScope::ladder_for("exec", &exec_args("true", &[])),
            vec![
                exact_command("true", &[]),
                GrantScope::CommandPrefix {
                    tokens: vec!["true".into()],
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
                GrantLevel::Chat { chat_id: chat },
                "web_search",
                kind,
                exact_web_search("quarterly filings"),
                Utc::now(),
            )
            .unwrap(),
        );

        let query = |query: &str| serde_json::json!({ "query": query });
        assert!(grants.covers(chat, None, "web_search", kind, &query("quarterly filings")));
        // The tool trims before searching, so padding is not a different search.
        assert!(grants.covers(
            chat,
            None,
            "web_search",
            kind,
            &query("  quarterly filings ")
        ));
        assert!(!grants.covers(chat, None, "web_search", kind, &query("payroll")));
        // Same query, different tool: a grant is scoped to the tool it was
        // given for, and a private-source search is not a public web search.
        assert!(!grants.covers(
            chat,
            None,
            "search",
            ToolApprovalKind::for_tool_name("search"),
            &query("quarterly filings"),
        ));
        // The filters go to the provider too, so the same query with a domain
        // filter is a different disclosure and was never the one granted.
        assert!(!grants.covers(
            chat,
            None,
            "web_search",
            kind,
            &serde_json::json!({ "query": "quarterly filings", "domains": ["sec.gov"] }),
        ));
        // Bounding the response is not part of what leaves the machine.
        assert!(grants.covers(
            chat,
            None,
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
                    GrantLevel::Chat { chat_id: chat },
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
        assert!(grants.covers(chat, None, "exec", kind, &exec_args("git", &["log"])));
        assert!(!grants.covers(chat, None, "exec", kind, &exec_args("npm", &["i"])));
    }

    #[test]
    fn recording_is_idempotent_and_revocable() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::for_tool_name("search");
        let grants = StandingGrants::new();
        grants.record(grant(chat, "search"));
        grants.record(grant(chat, "search"));
        assert_eq!(grants.read().len(), 1);
        assert!(grants.covers(chat, None, "search", kind, &no_args()));

        grants.clear();
        assert!(!grants.covers(chat, None, "search", kind, &no_args()));
    }

    /// Canonical `write_file` arguments; the document rides along untouched
    /// because a place grant is indifferent to it.
    fn write_args(path: &str) -> serde_json::Value {
        serde_json::json!({ "path": path, "content": "drafted text" })
    }

    fn place_grant(chat: ChatId, prefix: &str) -> StandingGrant {
        StandingGrant::scoped(
            GrantLevel::Chat { chat_id: chat },
            "write_file",
            ToolApprovalKind::WorkspaceMayModifyFiles,
            GrantScope::PathSubtree {
                prefix: prefix.into(),
            },
            Utc::now(),
        )
        .expect("a workspace write is grantable about a place")
    }

    #[test]
    fn a_place_grant_covers_writes_under_it_and_nothing_else() {
        let chat = ChatId::new();
        let kind = ToolApprovalKind::WorkspaceMayModifyFiles;
        let grants = StandingGrants::from_grants(vec![place_grant(chat, "reports")]);
        let covers = |path: &str| grants.covers(chat, None, "write_file", kind, &write_args(path));

        assert!(covers("reports/q1.md"));
        assert!(covers("reports/2026/q1.md"));
        // Matching is canonical, segment-wise: a cosmetic respelling of the
        // same place is covered, a sibling that merely shares letters is not.
        assert!(covers("./reports//q1.md"));
        assert!(!covers("reports-old/q1.md"));
        assert!(!covers("notes.md"));
        // Anything that could point outside the workspace keeps asking.
        assert!(!covers("reports/../secret.md"));
        assert!(!covers("/reports/q1.md"));
        // A path the preview could not reproduce was never the one granted.
        let long = "x".repeat(crate::preview::MAX_ACTION_FIELD_CHARS + 1);
        assert!(!covers(&format!("reports/{long}.md")));
        // A place grant is scoped to the tool that writes, not borrowed by a
        // command that mentions the same path.
        assert!(!grants.covers(
            chat,
            None,
            "exec",
            ToolApprovalKind::ExecMayRunNetworkedCommand,
            &exec_args("touch", &["reports/q1.md"]),
        ));
    }

    #[test]
    fn a_workspace_write_is_grantable_only_about_a_place() {
        let level = GrantLevel::Chat {
            chat_id: ChatId::new(),
        };
        let kind = ToolApprovalKind::WorkspaceMayModifyFiles;
        // The whole-tool "yes" already exists as the chat's Auto mode.
        assert!(StandingGrant::new(level, "write_file", kind, Utc::now()).is_none());
        assert!(StandingGrant::scoped(
            level,
            "write_file",
            kind,
            GrantScope::ExactAction(ToolActionPreview::WriteFile {
                path: "notes.md".into()
            }),
            Utc::now(),
        )
        .is_none());
        // And the place rung belongs to workspace writes alone.
        assert!(StandingGrant::scoped(
            level,
            "exec",
            ToolApprovalKind::ExecMayRunNetworkedCommand,
            GrantScope::PathSubtree {
                prefix: "reports".into()
            },
            Utc::now(),
        )
        .is_none());
    }

    #[test]
    fn the_write_ladder_names_the_file_then_its_directory() {
        assert_eq!(
            GrantScope::ladder_for("write_file", &write_args("reports/q1.md")),
            vec![
                GrantScope::PathSubtree {
                    prefix: "reports/q1.md".into()
                },
                GrantScope::PathSubtree {
                    prefix: "reports".into()
                },
            ]
        );
        // A root-level file has no directory rung, and there is deliberately
        // no whole-workspace rung to fall back to.
        assert_eq!(
            GrantScope::ladder_for("write_file", &write_args("notes.md")),
            vec![GrantScope::PathSubtree {
                prefix: "notes.md".into()
            }]
        );
        // Nothing is offered for a place the card could not show faithfully.
        let long = "x".repeat(crate::preview::MAX_ACTION_FIELD_CHARS + 1);
        assert_eq!(
            GrantScope::ladder_for("write_file", &write_args(&long)),
            Vec::<GrantScope>::new()
        );
        assert_eq!(
            GrantScope::ladder_for("write_file", &write_args("reports/../q1.md")),
            Vec::<GrantScope>::new()
        );
        // An unknown Workspace-class tool has no place to name, and the
        // whole-tool default must not leak through the mintable ladder.
        assert_eq!(
            GrantScope::mintable_ladder_for(
                ToolApprovalKind::WorkspaceMayModifyFiles,
                "third_party_editor",
                &no_args(),
            ),
            Vec::<GrantScope>::new()
        );
    }
}
