//! Domain types for an external agent-engine session.
//!
//! These types are engine-neutral: they describe a supervised conversation
//! with an external agent engine, not a coding CLI specifically. Tidebreak's
//! own internal loop is a future implementor of the same contract.
//!
//! Id types are structurally identical to chat ids (UUID newtypes, transparent
//! serde) but distinct so the two surfaces cannot be confused at compile time.

mod caps;
mod event;

pub use caps::{CapLevel, HarnessCaps, HarnessCommand, HarnessTier};
pub use event::{
    ApprovalDecisionKind, BoundedError, CheckpointHint, CodeEvent, CodeUsage, Diffstat,
    FileChangeKind, HarnessNoticeLevel, SequencedCodeEvent, ToolDetail, ToolOutcome,
    MAX_EVENT_TEXT_CHARS, MAX_NOTICE_CHARS, MAX_PREVIEW_CHARS, MAX_TOOL_SUMMARY_CHARS,
};

use crate::attention::{Attention, FenceReason};
use crate::image::ImageRef;
use crate::PermissionMode;
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
code_id_type!(
    /// Identifies one sandbox lifetime within a remote session.
    CodeIncarnationId
);
code_id_type!(
    /// Identifies one external-conversation binding on a session.
    CodeBindingId
);
code_id_type!(
    /// Identifies one adapter grant: a channel user's link to this machine.
    CodeGrantId
);
code_id_type!(
    /// Identifies one connect handshake behind a grant.
    CodeHandshakeId
);
code_id_type!(
    /// Identifies one durable trigger rule bound to a repository.
    CodeTriggerId
);
code_id_type!(
    /// Identifies one durable trigger delivery across every retry.
    CodeTriggerDeliveryId
);
code_id_type!(
    /// Identifies one observed pull request across every repository.
    CodePullRequestId
);
code_id_type!(
    /// Identifies one observed GitHub Actions workflow run.
    CodeWorkflowRunId
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
    /// Archive owns the checkout and rejects every new writer.
    Archiving,
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
            Self::Archiving => "archiving",
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
            "archiving" => Some(Self::Archiving),
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
    /// The remote this checkout was cloned from, when Tidebreak cloned it.
    ///
    /// `None` means the user registered a directory that already existed.
    /// Only a checkout Tidebreak created is Tidebreak's to delete.
    pub cloned_from: Option<String>,
    /// GitHub host parsed from the origin remote, e.g. `github.com`.
    ///
    /// Populated lazily the first time the origin resolves and refreshed when
    /// it changes; `None` until then, and for repositories whose origin is
    /// not GitHub-shaped. Persisted so pull-request facts join to local
    /// repositories without a git subprocess per read (decision 77).
    pub origin_host: Option<String>,
    /// Repository owner login parsed from the origin remote.
    pub origin_owner: Option<String>,
    /// Repository name parsed from the origin remote.
    pub origin_name: Option<String>,
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

impl CodeWorkspace {
    /// Whether this workspace's engine runs in a remote sandbox.
    ///
    /// A remote workspace has no host worktree: the clone lives inside the
    /// sandbox, and the branch state travels as WIP refs on the origin. The
    /// marker is `remote:<workspace-id>` — per-workspace, because the column
    /// is unique to guard two local workspaces from sharing one checkout,
    /// and remote workspaces must not collide with each other on a shared
    /// sentinel. An empty path is accepted as the marker too, defensively.
    /// Nothing on this machine ever creates, reads, or reclaims a checkout
    /// for either form.
    #[must_use]
    pub fn is_remote(&self) -> bool {
        self.worktree_path.is_empty() || self.worktree_path.starts_with("remote:")
    }

    /// The stored `worktree_path` marker for a remote workspace.
    #[must_use]
    pub fn remote_worktree_marker(id: WorkspaceId) -> String {
        format!("remote:{id}")
    }
}

/// Coarse lifecycle of an observed pull request (decision 77).
///
/// Deliberately narrower than the digest's free-form `state` string: the fact
/// table stores only what stack derivation and trigger edges key on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodePullRequestState {
    /// Open, draft or not.
    Open,
    /// Merged.
    Merged,
    /// Closed without merging.
    Closed,
}

impl CodePullRequestState {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

/// How strongly a workspace is tied to a pull request (decision 77).
///
/// Only two acts mint attribution: `gh pr create` (authored) and a push whose
/// branch is or becomes a pull request's head (contributed). Reading,
/// checking out, commenting on, closing, or merging a pull request never
/// does, so review and triage agents stay out of the attributed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodePullRequestRelation {
    /// The workspace opened the pull request.
    Authored,
    /// The workspace pushed commits to the pull request's head branch.
    Contributed,
}

impl CodePullRequestRelation {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Contributed => "contributed",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "authored" => Some(Self::Authored),
            "contributed" => Some(Self::Contributed),
            _ => None,
        }
    }
}

/// Which observer first tied a workspace to a pull request (decision 77).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodePullRequestDiscovery {
    /// The post-turn detector saw the act in the session's journaled shell
    /// commands and confirmed it against the host.
    Command,
    /// The reconcile sweep matched the pull request to the workspace by
    /// number or head SHA.
    Reconcile,
}

impl CodePullRequestDiscovery {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Reconcile => "reconcile",
        }
    }

    /// Parse a stored token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "command" => Some(Self::Command),
            "reconcile" => Some(Self::Reconcile),
            _ => None,
        }
    }
}

/// Durable observation of one pull request (decision 77).
///
/// GitHub stays authoritative; a row is a confirmed observation, never a
/// guess. Identity is `(owner, host, repo_owner, repo_name, number)`, so a
/// pull request in a repository with no local checkout is representable.
/// The snapshot fields keep what stack derivation, trigger edges, and list
/// rows need; `live` carries the volatile state a digest read observed
/// (decision 66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodePullRequestFact {
    /// Stable id.
    pub id: CodePullRequestId,
    /// Principal whose credential observed the pull request.
    pub owner: crate::OwnerId,
    /// Host, e.g. `github.com`.
    pub host: String,
    /// Repository owner login.
    pub repo_owner: String,
    /// Repository name.
    pub repo_name: String,
    /// Pull request number.
    pub number: u64,
    /// Web URL.
    pub url: String,
    /// Title at last observation.
    pub title: String,
    /// Coarse lifecycle.
    pub state: CodePullRequestState,
    /// Draft flag at last observation.
    pub draft: bool,
    /// Author login, when the host reported one.
    pub author: Option<String>,
    /// Head branch name.
    pub head_branch: String,
    /// Base branch name at last observation. Stack derivation keys on this.
    pub base_branch: String,
    /// Head commit at last observation.
    pub head_sha: Option<String>,
    /// When the host says the pull request was opened.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the host says it last changed.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Merge time, when merged.
    pub merged_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Close time, when closed.
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When this store first observed the pull request. Trigger `pr_opened`
    /// edges key on this; an upsert never moves it.
    pub first_seen_at: chrono::DateTime<chrono::Utc>,
    /// When this store last confirmed the snapshot.
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
    /// Volatile state from the newest digest read, when one has written it
    /// since the tier landed (decision 66).
    pub live: Option<CodePullRequestLiveState>,
}

/// Volatile pull-request state on a fact row (decision 66).
///
/// The snapshot fields answer "which pull request"; this tier answers "what
/// is it doing right now": the check rollup, review decision, mergeability,
/// merge state, auto-merge arming, and queue membership the digest carries.
/// A snapshot upsert never touches it, and writing it never counts as a
/// snapshot confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodePullRequestLiveState {
    /// One-line checks summary, as the digest carries it.
    pub checks_summary: Option<String>,
    /// Individual checks, when the host reported any.
    pub checks: Option<Vec<PullRequestCheck>>,
    /// Lowercased host review decision.
    pub review_decision: Option<String>,
    /// Lowercased host mergeability.
    pub mergeable: Option<String>,
    /// Lowercased host merge-state status.
    pub merge_state_status: Option<String>,
    /// Auto-merge armed on the host.
    pub auto_merge_enabled: Option<bool>,
    /// Merge-queue membership, when the host reported it.
    pub in_merge_queue: Option<bool>,
    /// When a digest read last wrote this tier.
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

impl CodePullRequestLiveState {
    /// The live tier one digest read carries (decision 66).
    #[must_use]
    pub fn from_digest(
        digest: &PullRequestDigest,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            checks_summary: digest.checks_summary.clone(),
            checks: digest.checks.clone(),
            review_decision: digest.review_decision.clone(),
            mergeable: digest.mergeable.clone(),
            merge_state_status: digest.merge_state_status.clone(),
            auto_merge_enabled: digest.auto_merge_enabled,
            in_merge_queue: digest.in_merge_queue,
            observed_at,
        }
    }

    /// Whether any live field other than `observed_at` differs. Broadcasts
    /// key on this, so a read that confirms no movement stays silent.
    #[must_use]
    pub fn differs_from(&self, other: &Self) -> bool {
        self.checks_summary != other.checks_summary
            || self.checks != other.checks
            || self.review_decision != other.review_decision
            || self.mergeable != other.mergeable
            || self.merge_state_status != other.merge_state_status
            || self.auto_merge_enabled != other.auto_merge_enabled
            || self.in_merge_queue != other.in_merge_queue
    }
}

/// Durable observation of one GitHub Actions workflow run.
///
/// GitHub stays authoritative; a row is a confirmed observation, never a
/// guess. Identity is `(owner, host, repo_owner, repo_name, github_id)`, so
/// a run in a repository with no local checkout is representable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkflowRunFact {
    /// Stable id.
    pub id: CodeWorkflowRunId,
    /// Principal whose credential observed the run.
    pub owner: crate::OwnerId,
    /// Host, e.g. `github.com`.
    pub host: String,
    /// Repository owner login.
    pub repo_owner: String,
    /// Repository name.
    pub repo_name: String,
    /// GitHub workflow run id.
    pub github_id: u64,
    /// Attempt number, when the host reported one.
    pub run_attempt: Option<u64>,
    /// Display title at last observation.
    pub name: String,
    /// Web URL.
    pub url: String,
    /// Lowercased host status (`queued`, `in_progress`, `completed`).
    pub status: String,
    /// Lowercased host conclusion, when finished.
    pub conclusion: Option<String>,
    /// Workflow name or path.
    pub workflow: Option<String>,
    /// Head branch, when the host reported one.
    pub branch: Option<String>,
    /// Head SHA, when the host reported one.
    pub sha: Option<String>,
    /// Triggering event.
    pub event: Option<String>,
    /// Actor login, when the host reported one.
    pub actor: Option<String>,
    /// When the host says the run started.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the host says it last changed.
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// When this store first observed the run.
    pub first_seen_at: chrono::DateTime<chrono::Utc>,
    /// When this store last confirmed the snapshot.
    pub last_seen_at: chrono::DateTime<chrono::Utc>,
}

impl CodeWorkflowRunFact {
    /// Whether any snapshot field other than id and the seen timestamps
    /// differs. Broadcasts key on this, so a read that confirms no movement
    /// stays silent.
    #[must_use]
    pub fn snapshot_differs(&self, other: &Self) -> bool {
        self.host != other.host
            || self.repo_owner != other.repo_owner
            || self.repo_name != other.repo_name
            || self.github_id != other.github_id
            || self.run_attempt != other.run_attempt
            || self.name != other.name
            || self.url != other.url
            || self.status != other.status
            || self.conclusion != other.conclusion
            || self.workflow != other.workflow
            || self.branch != other.branch
            || self.sha != other.sha
            || self.event != other.event
            || self.actor != other.actor
            || self.created_at != other.created_at
            || self.updated_at != other.updated_at
    }
}

/// One workspace's tie to one pull request (decision 77).
///
/// At most one row per `(pull_request, workspace)`; `relation` holds the
/// strongest claim, upgraded from contributed to authored when authoring
/// evidence appears. Plain foreign keys, no cascade: workspace rows are
/// soft-removed, so attribution survives archive and release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodePullRequestAttribution {
    /// Principal the attribution belongs to.
    pub owner: crate::OwnerId,
    /// The observed pull request.
    pub pull_request_id: CodePullRequestId,
    /// The workspace that worked on it.
    pub workspace_id: WorkspaceId,
    /// Strongest claim so far.
    pub relation: CodePullRequestRelation,
    /// Which observer minted the row.
    pub discovered_via: CodePullRequestDiscovery,
    /// Session whose act minted the row, when the detector minted it.
    pub session_id: Option<CodeSessionId>,
    /// The subagent `Task` span the minting command ran inside, when one did
    /// (decision 52). Absent when the parent session acted itself.
    pub parent_call_id: Option<String>,
    /// When the row was minted.
    pub created_at: chrono::DateTime<chrono::Utc>,
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
    pub permission_mode: PermissionMode,
    /// Engine model id for this session, when the user chose one.
    pub model: Option<String>,
    /// Reasoning effort for this session. `None` leaves the engine's own
    /// default in force, which no level on the ladder is equivalent to.
    #[serde(default)]
    pub reasoning_effort: Option<crate::model::ReasoningEffort>,
    /// Whether this session runs its turns in the engine's fast mode.
    ///
    /// Fast mode buys output speed at a higher price per token, so it is a
    /// spend decision rather than a quality one — unlike the effort ladder,
    /// which trades thinking depth for the same rate. Off is the engines' own
    /// default, so `false` is the honest starting value rather than a missing
    /// opinion. The server keeps this false when the selected model cannot
    /// serve fast mode.
    #[serde(default)]
    pub fast_mode: bool,
    /// Session lifecycle.
    pub lifecycle: CodeSessionLifecycle,
    /// Why the session is fenced, when it is.
    pub fence_reason: Option<FenceReason>,
    /// Child pid recorded at spawn, when a child is live.
    pub child_pid: Option<i64>,
    /// Opaque operating-system creation identity for the recorded child.
    ///
    /// A pid can be reused after the original process exits. Recovery must
    /// match this value before it signals the numeric pid.
    #[serde(default)]
    pub child_process_identity: Option<String>,
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
    /// Engine model selected when this turn started.
    ///
    /// A session may change models between turns, so analytics cannot recover
    /// this from the session row later. Older rows from before this snapshot
    /// was added leave it unset rather than assigning today's session model to
    /// historical usage.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether this turn started in the engine's fast service tier.
    #[serde(default)]
    pub fast_mode: bool,
    /// User input, inline when small enough.
    pub user_input: String,
    /// Blob id when the input was spilled, unused in this layer.
    pub user_input_blob_id: Option<Uuid>,
    /// Bounded image references on this user turn. Bytes live in the blob
    /// store; these rows pin them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImageRef>,
    /// Hidden checkpoint ref recorded at turn end, when any.
    pub checkpoint_ref: Option<String>,
    /// Diffstat of the turn's checkpoint, when recorded.
    pub diffstat: Option<Diffstat>,
    /// Token usage as reported by the engine.
    pub usage: Option<CodeUsage>,
    /// Asynchronous narrative; never blocks lifecycle.
    ///
    /// Derived after the turn ends and written only by
    /// [`crate::db::code::set_turn_narrative`]. `save_turn` deliberately leaves
    /// the column alone, because its callers hold a snapshot taken before this
    /// existed.
    pub narrative: Option<String>,
    /// Lucid rewrite of the closing message. The journal keeps the original.
    ///
    /// Derived after the turn ends and written only by
    /// [`crate::db::code::set_turn_rewrite`]. `save_turn` deliberately leaves
    /// the column alone, the same single-writer rule as `narrative`.
    #[serde(default)]
    pub rewrite: Option<String>,
    /// Start time.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// End time, when terminal.
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One durable queued follow-up: a message accepted while its session or its
/// workspace checkout was busy, promoted into a real turn strictly FIFO once
/// the session is free.
///
/// The id is the turn id the worker mints the promoted turn under, so the row
/// deletion and the turn insertion commit together and an ambiguous retry can
/// never run one message twice. Mirrors the chat queue contract (decision 9)
/// onto code sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeQueuedTurn {
    /// The turn id this row becomes when promoted.
    pub id: CodeTurnId,
    /// Owning session.
    pub session_id: CodeSessionId,
    /// Byte-exact user message.
    pub message: String,
    /// Bounded image references carried into the promoted turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImageRef>,
    /// Dense FIFO order within the session, starting at 0.
    pub position: i32,
    /// Enqueue time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last edit or reorder time; promotion refuses a row that moved under it.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl CodeQueuedTurn {
    /// Queue depth cap per session, matching the chat queue's per-chat cap.
    pub const MAX_PER_SESSION: usize = 32;
}

/// Lifecycle of one sandbox lifetime within a remote session.
///
/// Written in the order the protocol runs: the intent row commits before the
/// environment is asked to provision, activation records what the spawn
/// returned, and stopped is terminal. A row that never leaves intent marks a
/// spawn whose outcome nothing recorded — the reconcile sweep's quarry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncarnationState {
    /// Committed before provisioning; no sandbox is known yet.
    Intent,
    /// The environment accepted the spawn; `sandbox_id` names the sandbox.
    Active,
    /// Terminal. `stop_reason` says why, when known.
    Stopped,
}

impl IncarnationState {
    /// The stored token, matching the table's CHECK constraint.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Active => "active",
            Self::Stopped => "stopped",
        }
    }

    /// Parses a stored token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "intent" => Some(Self::Intent),
            "active" => Some(Self::Active),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

/// One sandbox lifetime of a remote session, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSessionIncarnation {
    /// Stable id.
    pub id: CodeIncarnationId,
    /// Owner, carried so machine-wide sweeps can act on what they find.
    pub owner: crate::OwnerId,
    /// Owning session.
    pub session_id: CodeSessionId,
    /// 1-based counter within the session; the agent names WIP refs with it.
    pub incarnation: i32,
    /// Where in the protocol this row is.
    pub state: IncarnationState,
    /// The environment's sandbox identifier, recorded at activation.
    pub sandbox_id: Option<String>,
    /// The turn number this incarnation starts at, for resume.
    pub starting_turn: i32,
    /// Terminal classification, when the environment named one.
    pub stop_reason: Option<String>,
    /// Last observed inference spend in micro-USD, for the session ledger.
    pub spend_microusd: Option<i64>,
    /// Whether this incarnation's terminal events reached the journal.
    ///
    /// Reincarnation waits on this: a successor built before the
    /// predecessor's terminal events land would resume without them.
    pub terminal_events_journaled: bool,
    /// Highest sandbox event sequence whose journal projection committed.
    ///
    /// Ingestion resumes after this sequence, so a server restart replays
    /// nothing and loses nothing.
    pub events_cursor: i64,
    /// The supervisor's terminal deliverable, when the run reported one.
    pub task_output: Option<String>,
    /// The last WIP checkpoint ref this incarnation pushed, for resume.
    pub last_wip_ref: Option<String>,
    /// Intent time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Activation time, when the spawn returned.
    pub activated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Stop time.
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last write time.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One external conversation's durable link to a session
/// (docs/slack-sessions.md, stage 2).
///
/// The key is opaque to the machine: a Slack thread key is one channel
/// kind, and later channels reuse the row shape unchanged. The grant id
/// tags which adapter credential created the binding; every
/// grant-authenticated call scopes through it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeExternalBinding {
    /// Stable id.
    pub id: CodeBindingId,
    /// Owner.
    pub owner: crate::OwnerId,
    /// Which channel family the key belongs to (for example `slack`).
    pub channel_kind: String,
    /// The channel's durable conversation identity, opaque here.
    pub external_key: String,
    /// The grant whose call created the binding.
    pub grant_id: CodeGrantId,
    /// The bound session.
    pub session_id: CodeSessionId,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The credential a channel adapter holds per linked user
/// (`docs/slack-sessions.md`, stage 2).
///
/// The row carries no secrets: the machine stores only hashes, and the
/// adapter holds the token pair. `revoked_at`/`revoked_reason` make
/// revocation durable and visible — the desktop grants list renders the
/// reason, so a theft-triggered revoke reaches the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeExternalGrant {
    /// Stable id; bindings tag sessions with it.
    pub id: CodeGrantId,
    /// Owner.
    pub owner: crate::OwnerId,
    /// Which channel family linked (for example `slack`).
    pub channel_kind: String,
    /// The channel's identity for the linked user, opaque here.
    pub external_identity: String,
    /// The channel's workspace identity (for example a Slack team id).
    pub workspace_identity: String,
    /// When the token pair last rotated.
    pub rotated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the grant was revoked; a live grant carries `None`.
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Why, in owner-facing words.
    pub revoked_reason: Option<String>,
}

/// One connect handshake: the human half of minting a grant
/// (`docs/slack-sessions.md`, stage 2).
///
/// The row walks `Pending` (card posted) to `Approved` (the owner said
/// "this is me" on the hosted approval page) to `Completed` (the adapter's
/// closing confirm, after its DM proved control of the channel account).
/// Only completion mints: a forwarded link can reach `Approved` at most,
/// which binds nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeConnectHandshake {
    /// Stable id.
    pub id: CodeHandshakeId,
    /// Which channel family is linking (for example `slack`).
    pub channel_kind: String,
    /// The channel's identity for the linking user, opaque here.
    pub external_identity: String,
    /// The channel's workspace identity (for example a Slack team id).
    pub workspace_identity: String,
    /// The person's display name in the channel, for "is this you?".
    pub display_name: String,
    /// The channel workspace's human name, shown on the approval page.
    pub workspace_name: String,
    /// The person's avatar in the channel, when the channel offers one.
    pub avatar_url: Option<String>,
    /// Where the handshake stands.
    pub state: CodeConnectState,
    /// The Tidebreak owner this approval surface is bound to. The first
    /// authenticated view claims it, so a CSRF token copied from one owner
    /// cannot approve for another.
    pub approval_owner: Option<crate::OwnerId>,
    /// The grant minted by the completed handshake, when one exists.
    pub grant_id: Option<CodeGrantId>,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When the nonce stops working.
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Human-facing identity retained with a grant-producing handshake.
///
/// Grant credentials keep channel identities opaque for authorization. This
/// projection lets owner-facing settings show the names and avatar that the
/// approval page showed without changing those authorization keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeGrantProfile {
    /// The grant this identity describes.
    pub grant_id: CodeGrantId,
    /// The channel user's display name at connect time.
    pub display_name: String,
    /// The channel workspace's display name at connect time.
    pub workspace_name: String,
    /// The channel user's avatar at connect time, when the adapter supplied a
    /// safe public HTTPS URL.
    pub avatar_url: Option<String>,
}

/// Where a connect handshake stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeConnectState {
    /// The card is posted; nobody has approved.
    Pending,
    /// The owner approved on the hosted page; nothing is minted yet.
    Approved,
    /// The adapter's closing confirm landed; the grant is minted.
    Completed,
}

impl CodeConnectState {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Completed => "completed",
        }
    }

    /// Parse a stored token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }
}

/// What presenting a refresh token for rotation found.
#[derive(Debug, Clone, PartialEq)]
pub enum GrantRotation {
    /// The presented token was current: the pair rotated, and the old
    /// refresh hash stays behind for reuse detection.
    Rotated(Box<CodeExternalGrant>),
    /// The presented token was already rotated away. Only two parties ever
    /// held it, and the adapter discarded it — so a replay is theft. The
    /// grant is revoked, durably, before this answer returns.
    ReuseDetected(Box<CodeExternalGrant>),
    /// The token matches nothing live.
    Unknown,
}

/// What an external get-or-create resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalSessionResolution {
    /// No binding existed; the workspace, session, and binding committed
    /// together.
    Created(Box<CodeExternalBinding>),
    /// The conversation was already bound to a live session.
    Existing(Box<CodeExternalBinding>),
    /// The bound session has ended. The adapter closes its routing row;
    /// the machine never resurrects.
    Ended {
        /// The ended session.
        session_id: CodeSessionId,
    },
    /// The conversation is bound under a different grant. Refused: one
    /// grant must never reach another grant's sessions.
    GrantMismatch,
}

/// What recording one external message delivery committed
/// (`docs/slack-sessions.md`, stage 2).
///
/// The event id and the queue row it caused commit in one transaction, so a
/// replayed delivery never writes a second row: it answers `Replay` with the
/// id the first delivery minted, and the caller derives the outcome from
/// that row's current state.
#[derive(Debug, Clone, PartialEq)]
pub enum ExternalMessageRecord {
    /// First delivery: the queue row and event row committed together.
    Recorded(Box<CodeQueuedTurn>),
    /// The event was already recorded. `turn_id` names the row the first
    /// delivery caused — still queued, promoted into a turn, or retracted.
    Replay {
        /// The id shared by the queue row and the turn it promotes into.
        turn_id: CodeTurnId,
    },
}

/// The outcome of asking for a new incarnation under the owner's cap.
#[derive(Debug, Clone, PartialEq)]
pub enum IncarnationAdmission {
    /// The intent row committed; provision against it. Boxed because the
    /// row dwarfs the refusal variant.
    Admitted(Box<CodeSessionIncarnation>),
    /// The session already holds a live incarnation — another submit won
    /// the race between observing a stopped predecessor and reserving.
    /// Retry after it settles.
    AlreadyLive {
        /// The live incarnation's 1-based counter.
        incarnation: i32,
    },
    /// The owner is at their concurrent-sandbox cap.
    CapExhausted {
        /// Sessions holding the live incarnations, so the refusal can name
        /// what is running instead of only a number.
        running: Vec<CodeSessionId>,
    },
}

/// Bounded image reference recorded on a code-mode user turn.
///
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
    /// Engine-native call ID. Older rows may predate the binding migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_call_id: Option<String>,
    /// Opaque server capability for the exact parked native request.
    #[serde(default, skip_serializing)]
    pub server_capability: Option<String>,
    /// SHA-256 of the exact approval request before display capping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_sha256: Option<String>,
    /// Worker epoch that owned the native request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_epoch: Option<i64>,
    /// Atomic claim held while one decision reaches the engine.
    #[serde(default, skip_serializing)]
    pub decision_claim: Option<uuid::Uuid>,
    /// When the decision claim was acquired.
    #[serde(default, skip_serializing)]
    pub claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Decision state.
    pub state: CodeApprovalState,
    /// Denial feedback, when denied with a reason.
    pub feedback: Option<String>,
    /// When the engine asked.
    pub requested_at: chrono::DateTime<chrono::Utc>,
    /// When the user decided.
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A pull-request fact a trigger fires on.
///
/// The vocabulary is the watch classifier's, minus the states nothing can act
/// on. A trigger fires on the *transition* into one of these, once per head
/// SHA — see [`CodeTriggerFire`] (decision 60).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeTriggerCondition {
    /// At least one check reported a failure.
    ChecksFailed,
    /// The host reports the branch conflicting with its base.
    Conflicts,
    /// A reviewer requested changes.
    ChangesRequested,
    /// A review or repository requirement is outstanding.
    ReviewRequired,
    /// The branch is behind its base.
    Behind,
    /// Mergeable and clean: nothing is outstanding.
    ReadyToMerge,
    /// The pull request merged.
    Merged,
    /// The pull request closed without merging.
    Closed,
    /// A pull request came into existence (decision 77). Edge-sourced from
    /// the durable fact store's `first_seen_at`, never from
    /// [`classify_trigger_condition`].
    PrOpened,
    /// A tracked pull request's head moved (decision 77). Edge-sourced from
    /// the fact store, once per distinct head; the first observed head is a
    /// silent baseline, never a notification.
    PrUpdated,
}

impl CodeTriggerCondition {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChecksFailed => "checks_failed",
            Self::Conflicts => "conflicts",
            Self::ChangesRequested => "changes_requested",
            Self::ReviewRequired => "review_required",
            Self::Behind => "behind",
            Self::ReadyToMerge => "ready_to_merge",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::PrOpened => "pr_opened",
            Self::PrUpdated => "pr_updated",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "checks_failed" => Some(Self::ChecksFailed),
            "conflicts" => Some(Self::Conflicts),
            "changes_requested" => Some(Self::ChangesRequested),
            "review_required" => Some(Self::ReviewRequired),
            "behind" => Some(Self::Behind),
            "ready_to_merge" => Some(Self::ReadyToMerge),
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            "pr_opened" => Some(Self::PrOpened),
            "pr_updated" => Some(Self::PrUpdated),
            _ => None,
        }
    }

    /// Every condition, for enumerating the arming surface.
    #[must_use]
    pub const fn all() -> [Self; 10] {
        [
            Self::ChecksFailed,
            Self::Conflicts,
            Self::ChangesRequested,
            Self::ReviewRequired,
            Self::Behind,
            Self::ReadyToMerge,
            Self::Merged,
            Self::Closed,
            Self::PrOpened,
            Self::PrUpdated,
        ]
    }

    /// One clause naming the fact, for the message a fire composes.
    ///
    /// Content discipline follows `fix_turn_instruction`: name the fact, not
    /// the logs.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::ChecksFailed => "checks are failing",
            Self::Conflicts => "the branch conflicts with its base",
            Self::ChangesRequested => "a reviewer requested changes",
            Self::ReviewRequired => "a review or repository requirement is outstanding",
            Self::Behind => "the branch is behind its base",
            Self::ReadyToMerge => "the pull request is ready to merge",
            Self::Merged => "the pull request merged",
            Self::Closed => "the pull request closed without merging",
            Self::PrOpened => "a pull request opened",
            Self::PrUpdated => "the pull request's head moved",
        }
    }

    /// True when the condition ends the pull request's life.
    ///
    /// A terminal condition fires once and the trigger stops tracking that
    /// pull request; there is no later head SHA to fire against.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Closed)
    }
}

/// What a trigger does when its condition fires.
///
/// Two actions in v1. Merge, auto-merge, and mark-ready stay with the user
/// (decision 42), and shell commands and webhooks need their own record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeTriggerAction {
    /// Deliver a bounded message to the workspace's active session.
    Deliver,
    /// Raise a notification and leave the session alone.
    Notify,
}

impl CodeTriggerAction {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deliver => "deliver",
            Self::Notify => "notify",
        }
    }

    /// Parse a stored/wire token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "deliver" => Some(Self::Deliver),
            "notify" => Some(Self::Notify),
            _ => None,
        }
    }
}

/// A standing rule binding one pull-request condition to one action.
///
/// Triggers bind per repository rather than per workspace: a trigger is a
/// preference about how the user wants to be reached, not a decision about one
/// pull request the way a watch is (decision 60). Every workspace on the
/// repository that has a pull request is in scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTrigger {
    /// Stable id.
    pub id: CodeTriggerId,
    /// Principal this trigger belongs to.
    pub owner: crate::OwnerId,
    /// Repository whose workspaces are in scope.
    pub repo_id: RepoId,
    /// The fact this trigger fires on.
    pub condition: CodeTriggerCondition,
    /// What firing does.
    pub action: CodeTriggerAction,
    /// False while the user has the trigger switched off; rows are kept so the
    /// scoping survives a toggle.
    pub enabled: bool,
    /// Creation time.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Last write.
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The exact edge identity for one trigger and pull request head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTriggerFireIdentity {
    /// Trigger that fired.
    pub trigger_id: CodeTriggerId,
    /// Principal the fire belongs to, denormalized from its trigger.
    pub owner: crate::OwnerId,
    /// Workspace whose pull request it fired against.
    pub workspace_id: WorkspaceId,
    /// Pull request number on the host.
    pub pr_number: u64,
    /// Head SHA the fire was fingerprinted against.
    ///
    /// A digest with no head SHA never fires: without it the fire cannot be
    /// bounded, and re-firing every sweep is worse than not firing.
    pub head_sha: String,
}

/// Durable sink selected for one trigger delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeTriggerDeliverySink {
    Turn,
    Steer,
    Attention,
}

impl CodeTriggerDeliverySink {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Steer => "steer",
            Self::Attention => "attention",
        }
    }
}

/// Durable state for one trigger delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeTriggerFireState {
    /// The side effect still needs delivery or acknowledgement.
    Pending,
    /// A sink acknowledged the side effect.
    Delivered,
    /// The rule was disabled before any sink accepted the side effect.
    Cancelled,
}

impl CodeTriggerFireState {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored token.
    #[must_use]
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "delivered" => Some(Self::Delivered),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Immutable input captured when a pull-request edge enters the outbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTriggerFirePayload {
    /// Action selected when the edge fired.
    pub action: CodeTriggerAction,
    /// Condition selected when the edge fired.
    pub condition: CodeTriggerCondition,
    /// Fully rendered message delivered to a turn or steering sink.
    pub message: String,
}

/// One trigger outbox row against one exact pull request head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeTriggerFire {
    /// Exact edge identity.
    pub identity: CodeTriggerFireIdentity,
    /// Stable idempotency key reused by every retry.
    pub delivery_id: CodeTriggerDeliveryId,
    /// Original delivery input. Legacy rows that were already delivered have
    /// no payload because they never need another attempt.
    pub payload: Option<CodeTriggerFirePayload>,
    /// Delivery state.
    pub state: CodeTriggerFireState,
    /// Number of leases granted for this delivery.
    pub attempt_count: i64,
    /// Worker lease token, when claimed.
    pub lease_token: Option<Uuid>,
    /// Worker lease expiry, when claimed.
    pub lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Earliest time a pending row may be claimed.
    pub next_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Last explicit delivery failure, bounded by [`Self::MAX_LAST_ERROR_CHARS`].
    pub last_error: Option<String>,
    /// When the edge first entered the outbox.
    pub fired_at: chrono::DateTime<chrono::Utc>,
    /// When a sink acknowledged delivery.
    pub delivered_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When disabling the rule cancelled the unaccepted delivery.
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl CodeTriggerFire {
    /// Maximum persisted error detail.
    pub const MAX_LAST_ERROR_CHARS: usize = 4_096;
    /// First retry delay after an explicit failure.
    pub const INITIAL_RETRY_DELAY_SECS: i64 = 5;
    /// Maximum retry delay after repeated failures.
    pub const MAX_RETRY_DELAY_SECS: i64 = 15 * 60;

    /// Bounded exponential delay for the current claimed attempt.
    #[must_use]
    pub fn retry_delay(attempt_count: i64) -> chrono::Duration {
        let exponent = u32::try_from(attempt_count.saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(20);
        let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
        let seconds = Self::INITIAL_RETRY_DELAY_SECS
            .saturating_mul(multiplier)
            .min(Self::MAX_RETRY_DELAY_SECS);
        chrono::Duration::seconds(seconds)
    }
}

/// Classify a digest into the condition a trigger would fire on, if any.
///
/// Generalizes the watch's `assess` over the same digest and the same host
/// tokens, and keeps its precedence: the most actionable fact wins, so a
/// conflicting branch reports conflicts rather than the failing checks that
/// conflict caused. Returns `None` for a digest nothing is armed for — a draft,
/// or a pull request merely waiting on pending checks.
///
/// Deliberately never returns [`CodeTriggerCondition::PrOpened`] or
/// [`CodeTriggerCondition::PrUpdated`]: those are edges of the durable fact
/// store (decision 77), not states a digest can carry, and the trigger
/// sweep's fact pass fires them without a host read.
#[must_use]
pub fn classify_trigger_condition(pr: &PullRequestDigest) -> Option<CodeTriggerCondition> {
    let state = pr.state.trim().to_ascii_lowercase();
    if pr.merged == Some(true) || state == "merged" {
        return Some(CodeTriggerCondition::Merged);
    }
    if state == "closed" {
        return Some(CodeTriggerCondition::Closed);
    }
    // A draft is the author's "not yet". Nothing fires on it, the way the
    // watch parks rather than acting.
    if pr.draft == Some(true) {
        return None;
    }
    let mergeable = pr.mergeable.as_deref().map(str::trim).unwrap_or("");
    let merge_state = pr
        .merge_state_status
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    if mergeable == "conflicting" || merge_state == "dirty" {
        return Some(CodeTriggerCondition::Conflicts);
    }
    let review = pr.review_decision.as_deref().map(str::trim).unwrap_or("");
    if review == "changes_requested" {
        return Some(CodeTriggerCondition::ChangesRequested);
    }
    let checks = pr.checks.as_deref().unwrap_or(&[]);
    if checks
        .iter()
        .any(|check| check.bucket == PullRequestCheckBucket::Fail)
    {
        return Some(CodeTriggerCondition::ChecksFailed);
    }
    if merge_state == "behind" {
        return Some(CodeTriggerCondition::Behind);
    }
    // Pending checks are the "not yet" the watch waits through. Reporting
    // ready or review-required here would fire before the checks that decide
    // the pull request have reported.
    if checks
        .iter()
        .any(|check| check.bucket == PullRequestCheckBucket::Pending)
    {
        return None;
    }
    if review == "review_required" {
        return Some(CodeTriggerCondition::ReviewRequired);
    }
    if merge_state == "blocked" || merge_state == "unstable" {
        return Some(CodeTriggerCondition::ReviewRequired);
    }
    if mergeable == "mergeable" && merge_state == "clean" {
        return Some(CodeTriggerCondition::ReadyToMerge);
    }
    None
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

    fn digest() -> PullRequestDigest {
        PullRequestDigest {
            number: 1,
            url: None,
            state: "open".to_owned(),
            title: None,
            checks_summary: None,
            checks: None,
            draft: None,
            merged: None,
            review_decision: None,
            mergeable: None,
            merge_state_status: None,
            head_branch: None,
            base_branch: None,
            head_sha: Some("abc123".to_owned()),
            auto_merge_enabled: None,
            in_merge_queue: None,
        }
    }

    fn check(bucket: PullRequestCheckBucket) -> PullRequestCheck {
        PullRequestCheck {
            name: "ci".to_owned(),
            bucket,
            detail: None,
            url: None,
        }
    }

    #[test]
    fn trigger_condition_tokens_round_trip() {
        for condition in CodeTriggerCondition::all() {
            assert_eq!(
                CodeTriggerCondition::from_str(condition.as_str()),
                Some(condition),
                "{} did not round-trip",
                condition.as_str()
            );
        }
    }

    /// Conflicts outrank the failing checks a conflict causes. Classifying on
    /// checks first would wake an agent to fix tests that pass once the
    /// conflict is resolved.
    #[test]
    fn conflicts_outrank_failing_checks() {
        let pr = PullRequestDigest {
            mergeable: Some("conflicting".to_owned()),
            checks: Some(vec![check(PullRequestCheckBucket::Fail)]),
            ..digest()
        };
        assert_eq!(
            classify_trigger_condition(&pr),
            Some(CodeTriggerCondition::Conflicts)
        );
    }

    /// A draft is the author's "not yet". Nothing fires on it, however bad the
    /// checks look.
    #[test]
    fn a_draft_fires_nothing() {
        let pr = PullRequestDigest {
            draft: Some(true),
            checks: Some(vec![check(PullRequestCheckBucket::Fail)]),
            ..digest()
        };
        assert_eq!(classify_trigger_condition(&pr), None);
    }

    /// Ready must wait for the checks that decide it. A classifier that
    /// reported ready while checks were still pending would fire on every
    /// pull request seconds after it opened.
    #[test]
    fn pending_checks_are_not_ready() {
        let pr = PullRequestDigest {
            mergeable: Some("mergeable".to_owned()),
            merge_state_status: Some("clean".to_owned()),
            checks: Some(vec![check(PullRequestCheckBucket::Pending)]),
            ..digest()
        };
        assert_eq!(classify_trigger_condition(&pr), None);

        let settled = PullRequestDigest {
            checks: Some(vec![check(PullRequestCheckBucket::Pass)]),
            ..pr
        };
        assert_eq!(
            classify_trigger_condition(&settled),
            Some(CodeTriggerCondition::ReadyToMerge)
        );
    }

    /// A blocked or unstable merge-state summary can reflect checks that have
    /// not finished. Wait for those checks before asking for review.
    #[test]
    fn pending_checks_outrank_blocked_merge_and_review_required() {
        for merge_state in ["blocked", "unstable"] {
            let pr = PullRequestDigest {
                review_decision: Some("review_required".to_owned()),
                merge_state_status: Some(merge_state.to_owned()),
                checks: Some(vec![check(PullRequestCheckBucket::Pending)]),
                ..digest()
            };
            assert_eq!(
                classify_trigger_condition(&pr),
                None,
                "{merge_state} fired while checks were pending"
            );
        }
    }

    /// A requested change remains actionable while checks run.
    #[test]
    fn changes_requested_outranks_pending_checks() {
        let pr = PullRequestDigest {
            review_decision: Some("changes_requested".to_owned()),
            merge_state_status: Some("blocked".to_owned()),
            checks: Some(vec![check(PullRequestCheckBucket::Pending)]),
            ..digest()
        };
        assert_eq!(
            classify_trigger_condition(&pr),
            Some(CodeTriggerCondition::ChangesRequested)
        );
    }

    /// A merged pull request classifies as merged even while the host still
    /// reports the stale open state it had a moment ago.
    #[test]
    fn merged_wins_over_a_stale_open_state() {
        let pr = PullRequestDigest {
            merged: Some(true),
            ..digest()
        };
        assert_eq!(
            classify_trigger_condition(&pr),
            Some(CodeTriggerCondition::Merged)
        );
        assert!(CodeTriggerCondition::Merged.is_terminal());
        assert!(!CodeTriggerCondition::ChecksFailed.is_terminal());
    }
}
