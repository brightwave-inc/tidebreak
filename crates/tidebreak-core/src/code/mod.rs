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
    /// Archived; worktree removed.
    Archived,
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
}

impl CodeApprovalState {
    /// Stable database and wire token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
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
            _ => None,
        }
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
    /// True when auto-merge is enabled on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_merge_enabled: Option<bool>,
}

/// One pull-request comment: an issue comment, a review body, or an inline
/// review comment. Never persisted; fetched live from the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PullRequestComment {
    /// Where on the PR the comment lives.
    pub kind: PullRequestCommentKind,
    /// Author login, when the host reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
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
