//! Domain types for an external agent-engine session.
//!
//! These types are engine-neutral: they describe a supervised conversation
//! with an external agent engine, not a coding CLI specifically. Tidebreak's
//! own internal loop is a future implementor of the same contract.
//!
//! Id types are structurally identical to chat ids (UUID newtypes, transparent
//! serde) but distinct so the two surfaces cannot be confused at compile time.

mod attention;
mod caps;
mod event;
mod permission;

pub use attention::{
    should_replace, Attention, AttentionSource, AttentionState, FenceReason, MAX_ATTENTION_NOTE,
    MAX_ATTENTION_PROMPT,
};
pub use caps::{CapLevel, HarnessCaps, HarnessCommand, HarnessTier};
pub use event::{
    ApprovalDecisionKind, BoundedError, CheckpointHint, CodeEvent, CodeUsage, Diffstat,
    FileChangeKind, HarnessNoticeLevel, SequencedCodeEvent, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS, MAX_TOOL_SUMMARY_CHARS,
};
pub use permission::CodePermissionMode;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Declares a UUID-backed identifier newtype with the same impls as chat ids.
macro_rules! code_id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh, random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(s)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }
    };
}

code_id_type!(
    /// Identifies a registered local git repository.
    RepoId
);
code_id_type!(
    /// Identifies one isolated workspace (worktree + branch) on a repo.
    WorkspaceId
);
code_id_type!(
    /// Identifies one durable conversation with an external agent engine.
    CodeSessionId
);
code_id_type!(
    /// Identifies one user→engine cycle inside a code session.
    CodeTurnId
);
code_id_type!(
    /// Identifies one parked approval belonging to a code session.
    CodeApprovalId
);
code_id_type!(
    /// Identifies one auxiliary terminal attached to a workspace.
    CodeTerminalId
);
code_id_type!(
    /// Identifies one durable watch task on a workspace's pull request.
    CodeWatchId
);

/// Which external agent engine a session is bound to.
///
/// Named after the shipped adapters. The traits those adapters implement
/// stay engine-neutral; this enum is only the catalog of known engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    /// Claude Code.
    ClaudeCode,
    /// Codex CLI.
    Codex,
    /// opencode.
    Opencode,
    /// Grok CLI.
    Grok,
}

impl HarnessKind {
    /// Every known engine, in adapter-tier order.
    pub const ALL: &'static [Self] = &[Self::ClaudeCode, Self::Codex, Self::Opencode, Self::Grok];

    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Grok => "grok",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }

    /// Adapter maturity tier for this engine.
    #[must_use]
    pub const fn tier(self) -> HarnessTier {
        match self {
            Self::ClaudeCode => HarnessTier::Reference,
            Self::Codex => HarnessTier::Secondary,
            Self::Opencode => HarnessTier::Tertiary,
            Self::Grok => HarnessTier::BestEffort,
        }
    }
}

impl std::fmt::Display for HarnessKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Lifecycle of a persisted code session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeSessionLifecycle {
    /// Row exists; no engine child has been launched.
    Created,
    /// No turn is running.
    Idle,
    /// An engine child is servicing a turn.
    Running,
    /// Crash recovery parked the session until an explicit reap.
    Fenced,
    /// The session is closed.
    Ended,
}

impl CodeSessionLifecycle {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Fenced => "fenced",
            Self::Ended => "ended",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "created" => Some(Self::Created),
            "idle" => Some(Self::Idle),
            "running" => Some(Self::Running),
            "fenced" => Some(Self::Fenced),
            "ended" => Some(Self::Ended),
            _ => None,
        }
    }
}

/// Why a session exists: the user's conversation, or an automation task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeSessionKind {
    /// The user's conversation with the engine.
    Interactive,
    /// A watch task's session; it runs fix turns, never user input.
    Watch,
}

impl CodeSessionKind {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Watch => "watch",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "interactive" => Some(Self::Interactive),
            "watch" => Some(Self::Watch),
            _ => None,
        }
    }
}

/// Status of a persisted workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeWorkspaceStatus {
    /// Worktree creation is in progress.
    Creating,
    /// Setup script failed; checkout is preserved.
    SetupFailed,
    /// Ready for sessions.
    Active,
    /// Archived; worktree removed, branch kept.
    Archived,
    /// Released; worktree and branch both gone, commits kept as a bundle.
    ///
    /// The deepest reclaim tier. A worktree is gigabytes of checkout and build
    /// output; the branch's own commits are usually kilobytes, so bundling
    /// them and dropping the branch frees nearly everything and still restores
    /// exactly. The transcript is untouched at every tier.
    Released,
}

impl CodeWorkspaceStatus {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::SetupFailed => "setup_failed",
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Released => "released",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "creating" => Some(Self::Creating),
            "setup_failed" => Some(Self::SetupFailed),
            "active" => Some(Self::Active),
            "archived" => Some(Self::Archived),
            "released" => Some(Self::Released),
            _ => None,
        }
    }
}

/// Status of one user→engine turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeTurnStatus {
    /// The engine is still working this turn.
    Running,
    /// The turn finished successfully.
    Completed,
    /// The turn failed.
    Failed,
    /// The turn was interrupted (user or recovery).
    Interrupted,
}

impl CodeTurnStatus {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }
}

/// State of a persisted approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeApprovalState {
    /// Waiting on a decision.
    Pending,
    /// The user approved.
    Approved,
    /// The user denied, optionally with steering feedback.
    Denied,
    /// The tool call resolved before anyone decided, so the decision can no
    /// longer reach the engine. An engine that times a parked call out — or a
    /// turn that ends while the call is still open — leaves the approval here
    /// rather than pending forever.
    Abandoned,
}

impl CodeApprovalState {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Abandoned => "abandoned",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "denied" => Some(Self::Denied),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// Whether this state still accepts a decision.
    #[must_use]
    pub const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// A named command the user can run in a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct QuickAction {
    /// Display name.
    pub name: String,
    /// Command to run in the worktree.
    pub command: String,
    /// When true, run once after workspace creation.
    pub auto_run_on_create: bool,
}

/// One CI check on a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PullRequestCheck {
    /// Check name as the host reports it.
    pub name: String,
    /// pass, pending, fail, or skipped.
    pub bucket: PullRequestCheckBucket,
    /// Host status phrase, when distinct from the bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    /// Host URL for this check, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

/// Coarse CI bucket used to color a check row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestCheckBucket {
    /// The check passed.
    Pass,
    /// The check is queued or still running.
    Pending,
    /// The check failed.
    Fail,
    /// The host deliberately skipped or cancelled the check.
    Skipped,
}

/// Bounded pull-request digest stored on a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PullRequestDigest {
    /// PR number on the host.
    pub number: u64,
    /// Host URL, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Host state token (open, merged, closed, …).
    pub state: String,
    /// PR title, when the host reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// One-line checks summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_summary: Option<String>,
    /// Individual checks, when the host reported any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checks: Option<Vec<PullRequestCheck>>,
    /// True when the host reports the PR as a draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub draft: Option<bool>,
    /// True when the host reports the PR merged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged: Option<bool>,
    /// Lowercased host review decision (approved, changes_requested, review_required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_decision: Option<String>,
    /// Lowercased host mergeability (mergeable, conflicting, unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mergeable: Option<String>,
    /// Lowercased host merge-state status (clean, blocked, behind, dirty, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merge_state_status: Option<String>,
    /// Head branch name on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub head_branch: Option<String>,
    /// Base branch name on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_branch: Option<String>,
    /// Head commit SHA the digest was read against, when the host reported
    /// one. The watch sweep uses it to avoid re-fixing the same head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub head_sha: Option<String>,
    /// True when auto-merge is enabled on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_merge_enabled: Option<bool>,
    // True when the host timeline says the PR is currently in its merge queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub in_merge_queue: Option<bool>,
}

/// One pull-request comment: an issue comment, a review body, or an inline
/// review comment. Never persisted; fetched live from the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PullRequestComment {
    /// Where on the PR the comment lives.
    pub kind: PullRequestCommentKind,
    /// Stable host identifier, normalized to text across GraphQL and REST
    /// comment shapes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<String>,
    /// Author login, when the host reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    /// Author avatar URL, when the host reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub avatar_url: Option<String>,
    /// Host page for the comment or review, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    /// Host creation timestamp, verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created_at: Option<String>,
    /// Comment body, markdown as the host stores it.
    pub body: String,
    /// Lowercased review verdict (approved, changes_requested, commented), on
    /// review bodies only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_state: Option<String>,
    /// File path, on inline review comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// Line number, on inline review comments when the host reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub line: Option<u64>,
}

/// Which surface of the PR a comment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestCommentKind {
    /// Conversation-tab issue comment.
    Issue,
    /// Review submission body.
    Review,
    /// Inline review comment on a file.
    Inline,
}

/// Best-effort classification of what an approval is asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodeApprovalKind {
    /// A command the engine wants to run.
    Command {
        /// Command string.
        cmd: String,
        /// Working directory, when reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// File writes the engine wants to make.
    FileWrite {
        /// Paths involved.
        paths: Vec<String>,
    },
    /// Network access the engine wants.
    Network {
        /// Host or summary the engine reported.
        summary: String,
    },
    /// Anything else, with the engine's own summary.
    Other {
        /// Engine-provided summary.
        summary: String,
    },
}

/// Persisted repository record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRepo {
    /// Stable id.
    pub id: RepoId,
    /// Principal this repository belongs to. Repositories are never shared
    /// across owners: two users may register or clone the same remote and
    /// each gets a checkout of their own.
    pub owner: crate::OwnerId,
    /// Canonical git toplevel.
    pub root_path: String,
    /// Display name.
    pub display_name: String,
    /// Default base ref for new workspaces.
    pub default_base_ref: String,
    /// Prefix applied to workspace branch names.
    pub branch_prefix: String,
    /// Optional setup script, run inside a new worktree.
    pub setup_script: Option<String>,
    /// Optional archive script, run before worktree removal.
    pub archive_script: Option<String>,
    /// Named commands available in workspaces of this repo.
    pub quick_actions: Vec<QuickAction>,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the registration was removed, if it was. A removed repository
    /// stops appearing in the repo list but keeps its archived workspaces and
    /// their transcripts reachable.
    pub removed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Persisted workspace record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspace {
    /// Stable id.
    pub id: WorkspaceId,
    /// Principal this workspace belongs to, denormalized from its repo.
    pub owner: crate::OwnerId,
    /// Owning repo.
    pub repo_id: RepoId,
    /// Display title.
    pub title: String,
    /// Absolute worktree path.
    pub worktree_path: String,
    /// Branch owned by this workspace for life.
    pub branch_name: String,
    /// Base ref the worktree was created from.
    pub base_ref: String,
    /// Lifecycle status.
    pub status: CodeWorkspaceStatus,
    /// Latest PR digest, if any.
    pub pr: Option<PullRequestDigest>,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Archive time, when archived.
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Release time, when released.
    pub released_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Commit the released branch pointed at, so a restore can say what it
    /// rebuilds and a reader can recognize the work without unbundling.
    pub released_tip: Option<String>,
    /// Size of the stored bundle. Kept for the reclaim surface, which has to
    /// report what a release actually bought without stat-ing every file.
    pub bundle_bytes: Option<i64>,
}

/// Most subagent rows kept on one session. Done entries fall off first.
pub const MAX_SESSION_SUBAGENTS: usize = 8;

/// Status of a harness subagent, derived from its spanning `Task` call
/// (decision 52): the call's start is the subagent's start, its result is
/// the end and outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeSubagentStatus {
    /// The spanning `Task` call has started and not yet resolved.
    Running,
    /// The spanning call succeeded.
    Done,
    /// The spanning call failed or was denied.
    Failed,
}

/// What a running interactive session is actually occupied with. This is
/// intentionally coarser than a transcript tool name: list surfaces need to
/// distinguish agent generation, a shell, a passive monitor, and delegated
/// work without leaking command text into every digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeSessionActivity {
    /// The model is reasoning or composing its next response.
    Agent,
    /// A command process is still running.
    Shell,
    /// A wait/output/monitor tool is observing background work.
    Monitor,
    /// One or more harness-owned subagents are still running.
    Subagents,
    /// A file read or edit is still in flight.
    File,
    /// A search is still in flight.
    Search,
    /// Another tool is still in flight.
    Tool,
}

/// One harness subagent on a session, tracked for rail visibility. Not a
/// session: the harness owns its lifecycle, so the server can neither steer
/// nor resume it (decision 52).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CodeSubagentSummary {
    /// The spanning `Task` call's engine-native id.
    pub call_id: String,
    /// Display name: the Task's description or the tool name.
    pub name: String,
    /// Status derived from the spanning call.
    pub status: CodeSubagentStatus,
}

/// Keep the session's subagent list within [`MAX_SESSION_SUBAGENTS`],
/// dropping the oldest Done entries first, then the oldest of the rest.
pub fn bound_subagents(subagents: &mut Vec<CodeSubagentSummary>) {
    while subagents.len() > MAX_SESSION_SUBAGENTS {
        let victim = subagents
            .iter()
            .position(|entry| entry.status == CodeSubagentStatus::Done)
            .unwrap_or(0);
        subagents.remove(victim);
    }
}

/// Persisted session record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSession {
    /// Stable id.
    pub id: CodeSessionId,
    /// Principal this session belongs to, denormalized from its workspace.
    pub owner: crate::OwnerId,
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// Why the session exists: user conversation or watch task.
    pub kind: CodeSessionKind,
    /// Engine this session is bound to.
    pub harness_kind: HarnessKind,
    /// Version observed at last launch, when known.
    pub harness_version: Option<String>,
    /// Engine-native resume token, when the stream has reported one.
    pub harness_resume_ref: Option<String>,
    /// Permission mode for this session.
    pub permission_mode: CodePermissionMode,
    /// Engine model id for this session, when the user chose one.
    pub model: Option<String>,
    /// Session lifecycle.
    pub lifecycle: CodeSessionLifecycle,
    /// Why the session is fenced, when it is.
    pub fence_reason: Option<FenceReason>,
    /// Child pid recorded at spawn, when a child is live.
    pub child_pid: Option<i64>,
    /// Incremented on every spawn so a superseded worker cannot write.
    pub spawn_epoch: i64,
    /// Current attention.
    pub attention: Attention,
    /// Count of unrecognized engine events observed this session.
    pub unrecognized_event_count: i64,
    /// Harness subagents observed on this session, bounded (decision 52).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<CodeSubagentSummary>,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Persisted turn record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTurn {
    /// Stable id.
    pub id: CodeTurnId,
    /// Owning session.
    pub session_id: CodeSessionId,
    /// 1-based ordinal within the session.
    pub ordinal: i64,
    /// Turn status.
    pub status: CodeTurnStatus,
    /// User input, inline when small enough.
    pub user_input: String,
    /// Blob id when the input was spilled, unused in this layer.
    pub user_input_blob_id: Option<Uuid>,
    /// Bounded image references on this user turn. Bytes live in the blob
    /// store; these rows pin them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<CodeTurnAttachment>,
    /// Hidden checkpoint ref recorded at turn end, when any.
    pub checkpoint_ref: Option<String>,
    /// Diffstat of the turn's checkpoint, when recorded.
    pub diffstat: Option<Diffstat>,
    /// Token usage as reported by the engine.
    pub usage: Option<CodeUsage>,
    /// Asynchronous narrative; never blocks lifecycle.
    pub narrative: Option<String>,
    /// Start time.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// End time, when terminal.
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Bounded image reference recorded on a code-mode user turn.
///
/// Identity only: the journal never carries pixels. `byte_len` is the blob
/// store's length at submit time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CodeTurnAttachment {
    /// Content-addressed blob holding the pixels.
    pub blob_id: Uuid,
    /// Format declared on submit, already validated.
    pub media_type: crate::image::ImageMediaType,
    /// Size of the stored bytes.
    pub byte_len: u64,
}

/// State of a persisted watch task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeWatchState {
    /// Polling the host; nothing is actionable right now.
    Watching,
    /// A fix turn is running or was just submitted.
    Fixing,
    /// Progress needs the user; polling continues in case it clears.
    Blocked,
    /// Terminal: the pull request merged, closed, or became ready.
    Done,
    /// Terminal: the user stopped the watch.
    Stopped,
    /// Terminal: the watch cannot continue (session gone, workspace archived).
    Failed,
}

impl CodeWatchState {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Watching => "watching",
            Self::Fixing => "fixing",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "watching" => Some(Self::Watching),
            "fixing" => Some(Self::Fixing),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            "stopped" => Some(Self::Stopped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// True when the watch no longer sweeps.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Stopped | Self::Failed)
    }
}

/// Persisted watch task: a durable background loop that keeps one
/// workspace's pull request moving until it merges or needs the user.
///
/// The watch owns a dedicated [`CodeSessionKind::Watch`] session in the same
/// worktree. It never merges or arms auto-merge — decision 42 reserves those
/// for the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeWatch {
    /// Stable id.
    pub id: CodeWatchId,
    /// Principal this watch belongs to, denormalized from its workspace.
    pub owner: crate::OwnerId,
    /// Workspace whose pull request is watched.
    pub workspace_id: WorkspaceId,
    /// The watch's dedicated session.
    pub session_id: CodeSessionId,
    /// Pull request number at watch start.
    pub pr_number: u64,
    /// Watch state.
    pub state: CodeWatchState,
    /// Human-readable reason for the current state, when one exists.
    pub detail: Option<String>,
    /// Head SHA the last fix turn was submitted against, when any.
    pub last_fix_head: Option<String>,
    /// Fix turns submitted so far.
    pub cycles: i64,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last sweep write.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Persisted approval record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeApproval {
    /// Stable id.
    pub id: CodeApprovalId,
    /// Owning session.
    pub session_id: CodeSessionId,
    /// Turn that requested it.
    pub turn_id: CodeTurnId,
    /// Display-oriented classification.
    pub kind: CodeApprovalKind,
    /// Size-capped raw engine payload.
    pub harness_raw: serde_json::Value,
    /// Decision state.
    pub state: CodeApprovalState,
    /// Denial feedback, when denied with a reason.
    pub feedback: Option<String>,
    /// When the engine asked.
    pub requested_at: chrono::DateTime<chrono::Utc>,
    /// When the user decided.
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_ids_roundtrip_as_bare_uuids() {
        let id = CodeSessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<CodeSessionId>(&json).unwrap(), id);
        assert_eq!(id.to_string().parse::<CodeSessionId>().unwrap(), id);
    }

    #[test]
    fn harness_kind_tokens_are_stable() {
        assert_eq!(HarnessKind::ClaudeCode.as_str(), "claude_code");
        assert_eq!(HarnessKind::from_str("grok"), Some(HarnessKind::Grok));
        assert_eq!(HarnessKind::ClaudeCode.tier(), HarnessTier::Reference);
    }
}
