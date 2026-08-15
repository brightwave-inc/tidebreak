//! Renderer-facing code-mode wire types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use tidebreak_core::{
    Attention, CapLevel, CodeEvent, CodePermissionMode, CodeRepo, CodeSession,
    CodeSessionLifecycle, CodeTurn, CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus,
    FenceReason, HarnessCaps, HarnessKind, HarnessTier, PullRequestDigest, QuickAction, RepoId,
    WorkspaceId,
};

/// A registered local git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeRepoSnapshot {
    pub id: RepoId,
    pub root_path: String,
    pub display_name: String,
    pub default_base_ref: String,
    pub branch_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub setup_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archive_script: Option<String>,
    pub quick_actions: Vec<QuickAction>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<CodeRepo> for CodeRepoSnapshot {
    fn from(repo: CodeRepo) -> Self {
        Self {
            id: repo.id,
            root_path: repo.root_path,
            display_name: repo.display_name,
            default_base_ref: repo.default_base_ref,
            branch_prefix: repo.branch_prefix,
            setup_script: repo.setup_script,
            archive_script: repo.archive_script,
            quick_actions: repo.quick_actions,
            created_at: repo.created_at,
        }
    }
}

/// One isolated workspace (worktree + branch) on a repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceSnapshot {
    pub id: WorkspaceId,
    pub repo_id: RepoId,
    pub title: String,
    pub worktree_path: String,
    pub branch_name: String,
    pub base_ref: String,
    pub status: CodeWorkspaceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr: Option<PullRequestDigest>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub archived_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<CodeWorkspace> for CodeWorkspaceSnapshot {
    fn from(workspace: CodeWorkspace) -> Self {
        Self {
            id: workspace.id,
            repo_id: workspace.repo_id,
            title: workspace.title,
            worktree_path: workspace.worktree_path,
            branch_name: workspace.branch_name,
            base_ref: workspace.base_ref,
            status: workspace.status,
            pr: workspace.pr,
            created_at: workspace.created_at,
            archived_at: workspace.archived_at,
        }
    }
}

/// One durable conversation with an external agent engine.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeSessionSnapshot {
    pub id: tidebreak_core::CodeSessionId,
    pub workspace_id: WorkspaceId,
    pub harness_kind: HarnessKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_resume_ref: Option<String>,
    pub permission_mode: CodePermissionMode,
    pub lifecycle: CodeSessionLifecycle,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fence_reason: Option<FenceReason>,
    pub attention: Attention,
    pub unrecognized_event_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<CodeSession> for CodeSessionSnapshot {
    fn from(session: CodeSession) -> Self {
        Self {
            id: session.id,
            workspace_id: session.workspace_id,
            harness_kind: session.harness_kind,
            harness_version: session.harness_version,
            harness_resume_ref: session.harness_resume_ref,
            permission_mode: session.permission_mode,
            lifecycle: session.lifecycle,
            fence_reason: session.fence_reason,
            attention: session.attention,
            unrecognized_event_count: session.unrecognized_event_count,
            created_at: session.created_at,
        }
    }
}

/// One user→engine turn.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeTurnSnapshot {
    pub id: tidebreak_core::CodeTurnId,
    pub session_id: tidebreak_core::CodeSessionId,
    pub ordinal: i64,
    pub status: CodeTurnStatus,
    pub user_input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage: Option<tidebreak_core::CodeUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checkpoint_ref: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<CodeTurn> for CodeTurnSnapshot {
    fn from(turn: CodeTurn) -> Self {
        Self {
            id: turn.id,
            session_id: turn.session_id,
            ordinal: turn.ordinal,
            status: turn.status,
            user_input: turn.user_input,
            usage: turn.usage,
            checkpoint_ref: turn.checkpoint_ref,
            started_at: turn.started_at,
            ended_at: turn.ended_at,
        }
    }
}

/// One journaled event on the per-session WebSocket.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct SequencedCodeEventFrame {
    pub seq: i64,
    pub event: CodeEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub replayed: Option<bool>,
}

/// Doctor report for every registered engine adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct HarnessDoctorReport {
    pub harnesses: Vec<HarnessDoctorEntry>,
}

/// One engine's probe, capabilities, and remediation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct HarnessDoctorEntry {
    pub kind: HarnessKind,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    pub tier: HarnessTier,
    pub caps: HarnessCaps,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authenticated: Option<bool>,
    pub remediation: String,
    pub stderr: String,
    pub unrecognized_event_count: i64,
}

/// Body of `POST /code/repos`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRepoBody {
    pub path: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub default_base_ref: Option<String>,
    #[serde(default)]
    pub branch_prefix: Option<String>,
    #[serde(default)]
    pub setup_script: Option<String>,
    #[serde(default)]
    pub archive_script: Option<String>,
}

/// Body of `PATCH /code/repos/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRepoBody {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub default_base_ref: Option<String>,
    #[serde(default)]
    pub branch_prefix: Option<String>,
    #[serde(default)]
    pub setup_script: Option<Option<String>>,
    #[serde(default)]
    pub archive_script: Option<Option<String>>,
}

/// Body of `POST /code/workspaces`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkspaceBody {
    pub repo_id: RepoId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub base_ref: Option<String>,
}

/// Body of `PATCH /code/workspaces/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchWorkspaceBody {
    #[serde(default)]
    pub title: Option<String>,
}

/// Body of `POST /code/workspaces/{id}/archive`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveWorkspaceBody {
    #[serde(default)]
    pub force: bool,
}

/// Body of `POST /code/workspaces/{id}/sessions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionBody {
    pub harness: HarnessKind,
    pub permission_mode: CodePermissionMode,
}

/// Body of `POST /code/sessions/{id}/turns`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTurnBody {
    pub message: String,
}

/// Body of `POST /code/sessions/{id}/steer`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerBody {
    pub message: String,
}

/// A follow-up parked while the session is already running a turn.
///
/// No turn id: the row is created when the worker promotes this slot.
/// `position` is 1-based in the single-slot queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct QueuedCodeTurn {
    pub session_id: tidebreak_core::CodeSessionId,
    pub message: String,
    pub position: i64,
}

/// Query for `GET /code/workspaces`.
#[derive(Debug, Deserialize)]
pub struct ListWorkspacesQuery {
    #[serde(default)]
    pub repo_id: Option<RepoId>,
}

/// Query for `GET /code/sessions/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct SessionEventsQuery {
    #[serde(default)]
    pub after: i64,
}

/// Used so capability flags stay reachable from the doctor root.
#[allow(dead_code)]
fn _cap_level(level: CapLevel) -> CapLevel {
    level
}
