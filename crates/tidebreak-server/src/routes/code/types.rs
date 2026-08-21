//! Renderer-facing code-mode wire types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use tidebreak_core::{
    Attention, CapLevel, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeEvent, CodePermissionMode, CodeRepo, CodeSession, CodeSessionKind, CodeSessionLifecycle,
    CodeSubagentSummary, CodeTerminalId, CodeTurn, CodeTurnId, CodeTurnStatus, CodeWatch,
    CodeWatchId, CodeWatchState, CodeWorkspace, CodeWorkspaceStatus, Diffstat, FenceReason,
    FileChangeKind, HarnessCaps, HarnessKind, HarnessTier, PullRequestDigest, QuickAction, RepoId,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub released_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Commit the released branch pointed at, so a client can name the work
    /// without the branch existing.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub released_tip: Option<String>,
    /// Stored bundle size, for reporting what a release reclaimed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub bundle_bytes: Option<i64>,
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
            released_at: workspace.released_at,
            released_tip: workspace.released_tip,
            bundle_bytes: workspace.bundle_bytes,
        }
    }
}

/// One durable conversation with an external agent engine.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeSessionSnapshot {
    pub id: tidebreak_core::CodeSessionId,
    pub workspace_id: WorkspaceId,
    pub kind: CodeSessionKind,
    pub harness_kind: HarnessKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub harness_resume_ref: Option<String>,
    pub permission_mode: CodePermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
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
            kind: session.kind,
            harness_kind: session.harness_kind,
            harness_version: session.harness_version,
            harness_resume_ref: session.harness_resume_ref,
            permission_mode: session.permission_mode,
            model: session.model,
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
    pub attachments: Vec<tidebreak_core::CodeTurnAttachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub usage: Option<tidebreak_core::CodeUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checkpoint_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diffstat: Option<Diffstat>,
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
            attachments: turn.attachments,
            usage: turn.usage,
            checkpoint_ref: turn.checkpoint_ref,
            diffstat: turn.diffstat,
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

/// `GET /code/sessions/{id}/debug` — journal plus turn rows for a bug report.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeSessionDebug {
    pub session: CodeSessionSnapshot,
    pub turns: Vec<CodeTurnSnapshot>,
    pub events: Vec<SequencedCodeEventFrame>,
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
    pub commands: Vec<tidebreak_core::HarnessCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authenticated: Option<bool>,
    pub remediation: String,
    pub stderr: String,
    pub unrecognized_event_count: i64,
}

/// One model a harness CLI listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct HarnessModel {
    pub id: String,
    pub label: String,
    pub default: bool,
}

/// `GET /code/harnesses/{kind}/models`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct HarnessModelList {
    pub kind: HarnessKind,
    pub models: Vec<HarnessModel>,
}

/// Body of `POST /code/repos/clone`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloneRepoBody {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub github: Option<String>,
    pub parent_dir: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Snapshot of an in-flight or finished clone job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeCloneJobSnapshot {
    pub id: String,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub percent: Option<u8>,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo_id: Option<RepoId>,
}

/// State of one warm harness install, returned by
/// `POST /code/harnesses/{kind}/install` and restated on the live bus.
///
/// `phase` is `installing`, `ready`, or `failed`. npm reports no usable
/// percentage to a pipe, so there is no bar to show — only which of the three
/// the engine is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeHarnessInstallSnapshot {
    pub kind: HarnessKind,
    /// The pinned version being installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    pub phase: String,
    pub done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Remembered clone destination plus observed `gh` status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeCloneDefaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent_dir: Option<String>,
    pub gh_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gh_authenticated: Option<bool>,
    pub gh_remediation: String,
}

/// A written fork transcript: `POST /code/sessions/{id}/fork`.
///
/// `path` is worktree-relative, which is what the child agent needs: the file
/// already sits in its working directory, so the prompt names a path rather
/// than carrying the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeForkTranscript {
    pub path: String,
    pub byte_len: u64,
    pub turns: u32,
    /// True when anything was left out to fit the size cap: the oldest
    /// turns, or the end of a turn too large on its own.
    pub truncated: bool,
}

/// Where new worktrees land: `GET`/`PUT /code/worktree-root`.
///
/// `root` is the stored setting and is absent while the deployment runs on its
/// default. `effective_root` is what the next workspace uses, and
/// `default_root` is what clearing the setting returns to — so a reader can
/// tell a chosen path from an inherited one without repeating the rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorktreeRoot {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub root: Option<String>,
    pub effective_root: String,
    pub default_root: String,
}

/// Body of `PUT /code/worktree-root`. A null or blank root clears the setting.
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SetCodeWorktreeRootBody {
    #[serde(default)]
    pub root: Option<String>,
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
pub struct RemoveRepoQuery {
    /// Delete the checkout on disk as well as the registration.
    ///
    /// Honored only when Tidebreak cloned it. A registered directory is the
    /// user's, and removal leaves it alone whatever this says.
    #[serde(default)]
    pub reclaim_checkout: bool,
}

/// Body of `POST /code/workspaces/{id}/archive` and `/release`.
#[derive(Debug, Deserialize)]
pub struct ArchiveWorkspaceBody {
    #[serde(default)]
    pub force: bool,
}

/// Body of `POST /code/workspaces/{id}/git/commit`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitWorkspaceBody {
    #[serde(default)]
    pub message: Option<String>,
}

/// Body of `POST /code/workspaces/{id}/git/pr`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePullRequestBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Result of staging and committing the workspace worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeCommitSnapshot {
    pub sha: String,
    pub message: String,
    pub stat: Diffstat,
}

/// Result of pushing the workspace branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodePushSnapshot {
    pub branch: String,
    pub remote: String,
}

/// PR + checks digest plus the local git facts the PR card needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspacePrSnapshot {
    pub dirty: bool,
    pub unpushed: bool,
    pub ahead: u64,
    pub has_upstream: bool,
    pub suggested_commit_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr: Option<PullRequestDigest>,
    pub gh_found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub gh_authenticated: Option<bool>,
    pub remediation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub watch: Option<CodeWatchSnapshot>,
}

/// One durable watch task on a workspace's pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWatchSnapshot {
    pub id: CodeWatchId,
    pub workspace_id: WorkspaceId,
    pub session_id: tidebreak_core::CodeSessionId,
    pub pr_number: u64,
    pub state: CodeWatchState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    pub cycles: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<CodeWatch> for CodeWatchSnapshot {
    fn from(watch: CodeWatch) -> Self {
        Self {
            id: watch.id,
            workspace_id: watch.workspace_id,
            session_id: watch.session_id,
            pr_number: watch.pr_number,
            state: watch.state,
            detail: watch.detail,
            cycles: watch.cycles,
            created_at: watch.created_at,
            updated_at: watch.updated_at,
        }
    }
}

/// Body of `POST /code/workspaces/{id}/pr/merge`.
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MergeCodePrBody {
    pub method: CodePrMergeMethod,
    /// True arms host auto-merge instead of merging immediately.
    #[serde(default)]
    pub auto: bool,
}

/// Merge strategy for a user-initiated PR merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodePrMergeMethod {
    Squash,
    Merge,
    Rebase,
}

/// `GET /code/workspaces/{id}/pr/comments`: the PR conversation, read live
/// from the host and never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodePrCommentsSnapshot {
    /// PR number the comments belong to.
    pub number: u64,
    /// Issue comments, review bodies, and inline review comments, ordered by
    /// creation time.
    pub comments: Vec<tidebreak_core::PullRequestComment>,
}

/// GitHub repository identity used by the install-wide delivery surfaces.
///
/// `host` keeps GitHub Enterprise repositories distinct without introducing a
/// generic provider abstraction. `tidebreak_repo_id` is present only when the
/// repository was resolved from the current owner's registered local catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeGitHubRepositoryRef {
    pub host: String,
    pub owner: String,
    pub name: String,
    pub name_with_owner: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tidebreak_repo_id: Option<RepoId>,
}

/// Minimal repository selector accepted by delivery query and action routes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeGitHubRepositoryTarget {
    #[serde(default = "default_github_host")]
    pub host: String,
    pub owner: String,
    pub name: String,
}

fn default_github_host() -> String {
    "github.com".to_owned()
}

/// Whether the local GitHub CLI can serve delivery requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeGitHubCapability {
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub authenticated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub viewer_login: Option<String>,
    pub remediation: String,
}

/// One repository-level failure in an otherwise usable aggregate response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliverySourceError {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repository: Option<CodeGitHubRepositoryTarget>,
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Registered repositories that resolve to GitHub, plus partial failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryRepositoriesSnapshot {
    pub capability: CodeGitHubCapability,
    pub repositories: Vec<CodeGitHubRepositoryRef>,
    pub errors: Vec<CodeDeliverySourceError>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

/// Body for validating manually tracked GitHub repositories. Values may be
/// `owner/repo`, `host/owner/repo`, or a GitHub HTTPS/SSH URL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ResolveCodeDeliveryRepositoriesBody {
    pub repositories: Vec<String>,
}

/// One Tidebreak workspace that plausibly produced a remote delivery item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryWorkspaceLink {
    pub workspace_id: WorkspaceId,
    pub repo_id: RepoId,
    pub title: String,
    pub branch_name: String,
    pub status: CodeWorkspaceStatus,
    pub exact: bool,
}

/// Why an open pull request belongs in the default Needs attention view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeDeliveryPrAttentionReason {
    ChangesRequested,
    ChecksFailed,
    Conflicts,
    Behind,
    Blocked,
}

/// One CI check, enriched with the workflow run that can be rerun when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryCheck {
    pub name: String,
    pub bucket: tidebreak_core::PullRequestCheckBucket,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workflow_run_id: Option<u64>,
}

/// Pull request row shared by the overview and notification monitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryPullRequestSummary {
    pub id: String,
    pub repository: CodeGitHubRepositoryRef,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub state: String,
    pub draft: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author_avatar_url: Option<String>,
    pub head_branch: String,
    pub base_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mergeable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merge_state_status: Option<String>,
    pub auto_merge_enabled: bool,
    pub checks: Vec<CodeDeliveryCheck>,
    pub attention_reasons: Vec<CodeDeliveryPrAttentionReason>,
    pub ready_to_merge: bool,
    pub workspace_links: Vec<CodeDeliveryWorkspaceLink>,
    pub labels: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Set only once the pull request merged. `state` alone cannot separate a
    /// merged pull request from a closed one on every host response, and the
    /// row says *when* it settled rather than when it was last touched.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Server-side PR query. Saved views are client-owned; their resolved filters
/// are sent here so paging remains bounded across many repositories.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryPullRequestQuery {
    pub repositories: Vec<CodeGitHubRepositoryTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub search: Option<String>,
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub review_states: Vec<String>,
    #[serde(default)]
    pub check_states: Vec<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub attention_only: bool,
    #[serde(default)]
    pub ready_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tidebreak_linked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub updated_after: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u16>,
    /// Skip the short list cache and reread GitHub.
    ///
    /// Set only by an explicit user refresh. Paging never sets it, so
    /// following a cursor stays on the aggregate the first page came from.
    #[serde(default)]
    pub refresh: bool,
}

/// One page of pull requests, with repository-local failures kept alongside
/// the usable rows instead of failing the entire cross-repository query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryPullRequestsPage {
    pub capability: CodeGitHubCapability,
    pub items: Vec<CodeDeliveryPullRequestSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
    pub errors: Vec<CodeDeliverySourceError>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

/// Target for a pull-request detail read or action.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryPullRequestTarget {
    pub repository: CodeGitHubRepositoryTarget,
    pub number: u64,
}

/// One file in a pull request's diff.
///
/// `patch` is the host's unified hunk text and is absent for binary files and
/// for diffs GitHub declines to render. It is bounded by the host, not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryPullRequestFile {
    pub path: String,
    /// `added`, `modified`, `removed`, `renamed`, `copied`, or `changed`.
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub patch: Option<String>,
}

/// Full PR drawer payload. Conversation entries retain the existing bounded
/// comment contract used by workspace PRs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryPullRequestDetail {
    pub summary: CodeDeliveryPullRequestSummary,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub requested_reviewers: Vec<String>,
    pub changed_files: u64,
    pub additions: u64,
    pub deletions: u64,
    pub commits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_by: Option<String>,
    /// Empty when the diff could not be read. Truncated by `files_truncated`
    /// rather than paged: the panel is a review aid, not a diff viewer.
    pub files: Vec<CodeDeliveryPullRequestFile>,
    pub files_truncated: bool,
    pub comments: Vec<tidebreak_core::PullRequestComment>,
    pub can_mark_ready: bool,
    pub can_merge: bool,
    pub can_rerun_failed: bool,
    pub can_close: bool,
    pub can_reopen: bool,
    pub can_comment: bool,
}

/// User-initiated global PR action. Code-changing actions deliberately do not
/// exist here; they remain workspace-scoped agent prompts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeDeliveryPullRequestAction {
    MarkReady,
    Merge {
        method: CodePrMergeMethod,
        #[serde(default)]
        auto: bool,
        expected_head_sha: String,
    },
    RerunFailed {
        workflow_run_ids: Vec<u64>,
    },
    Close,
    Reopen,
    Comment {
        body: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryPullRequestActionBody {
    pub target: CodeDeliveryPullRequestTarget,
    pub action: CodeDeliveryPullRequestAction,
}

/// Successful delivery mutation; callers refresh the affected detail after
/// showing this bounded result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryActionResult {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeDeliveryRunKind {
    WorkflowRun,
    Deployment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeDeliveryRunAttentionReason {
    Failure,
    TimedOut,
    ActionRequired,
    StartupFailure,
}

/// Normalized Actions workflow run or GitHub deployment row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryRunSummary {
    pub id: String,
    pub repository: CodeGitHubRepositoryRef,
    pub kind: CodeDeliveryRunKind,
    pub github_id: u64,
    pub name: String,
    pub url: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub workflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub actor: Option<String>,
    pub attention_reasons: Vec<CodeDeliveryRunAttentionReason>,
    pub workspace_links: Vec<CodeDeliveryWorkspaceLink>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryRunQuery {
    pub repositories: Vec<CodeGitHubRepositoryTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub search: Option<String>,
    #[serde(default)]
    pub kinds: Vec<CodeDeliveryRunKind>,
    #[serde(default)]
    pub statuses: Vec<String>,
    #[serde(default)]
    pub conclusions: Vec<String>,
    #[serde(default)]
    pub workflows: Vec<String>,
    #[serde(default)]
    pub environments: Vec<String>,
    #[serde(default)]
    pub branches: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default)]
    pub attention_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tidebreak_linked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub created_after: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u16>,
    /// Skip the short list cache and reread GitHub. See the pull-request query.
    #[serde(default)]
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryRunsPage {
    pub capability: CodeGitHubCapability,
    pub items: Vec<CodeDeliveryRunSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
    pub errors: Vec<CodeDeliverySourceError>,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryRunTarget {
    pub repository: CodeGitHubRepositoryTarget,
    pub kind: CodeDeliveryRunKind,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryWorkflowJob {
    pub id: u64,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conclusion: Option<String>,
    pub url: String,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failed_steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryDeploymentStatus {
    pub id: u64,
    pub state: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub environment_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub log_url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryRunDetail {
    pub summary: CodeDeliveryRunSummary,
    pub jobs: Vec<CodeDeliveryWorkflowJob>,
    pub deployment_statuses: Vec<CodeDeliveryDeploymentStatus>,
    pub can_rerun_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeDeliveryRunAction {
    RerunFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryRunActionBody {
    pub target: CodeDeliveryRunTarget,
    pub action: CodeDeliveryRunAction,
}

/// Bounded output of one named quick action. Never journaled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeActionSnapshot {
    pub name: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Body of `POST /code/workspaces/{id}/sessions`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSessionBody {
    pub harness: HarnessKind,
    pub permission_mode: CodePermissionMode,
    #[serde(default)]
    pub model: Option<String>,
}

/// Body of `POST /code/sessions/{id}/mode`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPermissionModeBody {
    pub permission_mode: CodePermissionMode,
}

/// Body of `POST /code/sessions/{id}/turns`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTurnBody {
    pub message: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub attachments: Vec<SubmitTurnAttachment>,
}

/// One image the client already published to the blob store.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitTurnAttachment {
    pub blob_id: uuid::Uuid,
    pub media_type: String,
}

/// Body of `POST /code/sessions/{id}/steer`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerBody {
    pub expected_turn_id: tidebreak_core::CodeTurnId,
    pub guidance: String,
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

/// Query for `GET /code/workspaces/{id}/tree`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceTreeQuery {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Bounded path listing for `GET /code/workspaces/{id}/tree`.
///
/// Paths only. Never file contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceTree {
    pub paths: Vec<String>,
    pub truncated: bool,
}

/// Query for `GET /code/workspaces/{id}/search`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceSearchQuery {
    pub query: String,
    #[serde(default)]
    pub include: Option<String>,
    #[serde(default)]
    pub exclude: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// One matching line from a workspace content search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceSearchMatch {
    pub path: String,
    pub line_number: u32,
    pub line: String,
}

/// Bounded content-search response for `GET /code/workspaces/{id}/search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceSearch {
    pub matches: Vec<CodeWorkspaceSearchMatch>,
    pub truncated: bool,
}

/// Query for `GET /code/workspaces/{id}/files`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceFilesQuery {
    #[serde(default)]
    pub turn: Option<CodeTurnId>,
}

/// Query for `GET /code/workspaces/{id}/blob`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceBlobQuery {
    pub path: String,
}

/// One worktree file's text for the center viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceBlob {
    pub path: String,
    pub content: String,
    pub truncated: bool,
    pub binary: bool,
}

/// Query for `GET /code/workspaces/{id}/diff`.
#[derive(Debug, Deserialize)]
pub struct WorkspaceDiffQuery {
    #[serde(default)]
    pub turn: Option<CodeTurnId>,
    #[serde(default)]
    pub file: Option<String>,
}

/// One changed path in a workspace or turn file list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeFileChange {
    pub path: String,
    pub kind: FileChangeKind,
    pub insertions: u32,
    pub deletions: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous_path: Option<String>,
}

/// Bounded changed-file list for `GET /code/workspaces/{id}/files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceFiles {
    pub files: Vec<CodeFileChange>,
    pub truncated: bool,
    pub stat: Diffstat,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<CodeTurnId>,
}

/// Bounded unified diff for `GET /code/workspaces/{id}/diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeWorkspaceDiff {
    pub diff: String,
    pub truncated: bool,
    pub stat: Diffstat,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<CodeTurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file: Option<String>,
}

/// One parked or decided engine approval.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeApprovalSnapshot {
    pub id: CodeApprovalId,
    pub session_id: tidebreak_core::CodeSessionId,
    pub turn_id: tidebreak_core::CodeTurnId,
    pub kind: CodeApprovalKind,
    /// Exact JSON the engine sent, already size-capped. The card renders this.
    pub harness_raw_json: String,
    pub state: CodeApprovalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback: Option<String>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<CodeApproval> for CodeApprovalSnapshot {
    fn from(approval: CodeApproval) -> Self {
        Self {
            id: approval.id,
            session_id: approval.session_id,
            turn_id: approval.turn_id,
            kind: approval.kind,
            harness_raw_json: approval.harness_raw.to_string(),
            state: approval.state,
            feedback: approval.feedback,
            requested_at: approval.requested_at,
            decided_at: approval.decided_at,
        }
    }
}

/// Body of `POST /code/approvals/{id}/decision`.
#[derive(Debug, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeApprovalDecisionBody {
    pub decision: CodeApprovalDecision,
    #[serde(default)]
    #[ts(optional)]
    pub feedback: Option<String>,
}

/// `approve` or `deny`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodeApprovalDecision {
    Approve,
    Deny,
}

/// Query for `GET /code/approvals`.
#[derive(Debug, Deserialize)]
pub struct ListApprovalsQuery {
    #[serde(default)]
    pub state: Option<CodeApprovalState>,
    #[serde(default)]
    pub session_id: Option<tidebreak_core::CodeSessionId>,
}

/// Query for `GET /code/sessions/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct SessionEventsQuery {
    #[serde(default)]
    pub after: i64,
}

/// One live auxiliary terminal. Bytes live only in the process ring.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeTerminalSnapshot {
    pub id: CodeTerminalId,
    pub workspace_id: WorkspaceId,
    pub cols: u16,
    pub rows: u16,
    pub ended: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Cursor-pull response for `GET /code/workspaces/{id}/terminals/{tid}/read`.
///
/// `bytes` is standard base64 of the raw ring slice. `overflow` is true when
/// the requested cursor had already fallen out of the ring; the payload then
/// starts with the inline truncation marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeTerminalRead {
    pub id: CodeTerminalId,
    pub workspace_id: WorkspaceId,
    pub bytes: String,
    pub cursor: u64,
    pub overflow: bool,
    pub truncated: bool,
    pub ended: bool,
}

/// Unsequenced activity notice published on the updates channel.
///
/// Never journaled. A client that missed one just pulls from its last cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[allow(dead_code)]
pub struct CodeTerminalActivityNotice {
    pub workspace_id: WorkspaceId,
    pub terminal_id: CodeTerminalId,
}

/// Cheap per-session digest on `/code/updates`.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct CodeSessionDigest {
    pub workspace: WorkspaceId,
    pub session: tidebreak_core::CodeSessionId,
    pub kind: CodeSessionKind,
    pub lifecycle: CodeSessionLifecycle,
    pub attention: Attention,
    pub title: String,
    pub turn_count: i64,
    /// What the live turn is occupied with, while running.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub activity: Option<tidebreak_core::CodeSessionActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr_state: Option<PullRequestDigest>,
    /// Watch progress, present only on `kind: watch` digests.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub watch_state: Option<CodeWatchState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub watch_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub watch_cycles: Option<i64>,
    /// Harness subagents on this session, present only when any were
    /// observed (decision 52).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub subagents: Option<Vec<CodeSubagentSummary>>,
}

impl From<crate::code::bus::SessionDigest> for CodeSessionDigest {
    fn from(digest: crate::code::bus::SessionDigest) -> Self {
        Self {
            workspace: digest.workspace,
            session: digest.session,
            kind: digest.kind,
            lifecycle: digest.lifecycle,
            attention: digest.attention,
            title: digest.title,
            turn_count: digest.turn_count,
            activity: digest.activity,
            pr_state: digest.pr_state,
            watch_state: digest.watch_state,
            watch_detail: digest.watch_detail,
            watch_cycles: digest.watch_cycles,
            subagents: digest.subagents,
        }
    }
}

/// One unsequenced notice on `WS /code/updates`.
///
/// A connect is restated as [`Self::Snapshot`]; later notices are live only.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CodeUpdateNotice {
    /// Full current digest of every non-ended session.
    Snapshot {
        /// One row per live session.
        sessions: Vec<CodeSessionDigest>,
    },
    /// One session's current digest.
    Digest {
        workspace: WorkspaceId,
        session: tidebreak_core::CodeSessionId,
        kind: CodeSessionKind,
        lifecycle: CodeSessionLifecycle,
        attention: Attention,
        title: String,
        turn_count: i64,
        /// What the live turn is occupied with, while running.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        activity: Option<tidebreak_core::CodeSessionActivity>,
        /// Boxed to keep the notice enum's variants near one size; the wire
        /// shape is unchanged.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pr_state: Option<Box<PullRequestDigest>>,
        /// Watch progress, present only on `kind: watch` digests.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        watch_state: Option<CodeWatchState>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        watch_detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        watch_cycles: Option<i64>,
        /// Harness subagents on this session, present only when any were
        /// observed (decision 52).
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        subagents: Option<Vec<CodeSubagentSummary>>,
    },
    /// Coalesced terminal activity. Not restated on connect.
    TerminalActivity {
        workspace_id: WorkspaceId,
        terminal_id: CodeTerminalId,
    },
    /// Progress of one `git clone` job. Not restated on connect.
    CloneProgress {
        job: String,
        phase: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        percent: Option<u8>,
        done: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        repo_id: Option<RepoId>,
    },
    /// Progress of one warm harness install. Not restated on connect.
    HarnessInstall {
        kind: HarnessKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        version: Option<String>,
        phase: String,
        done: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error: Option<String>,
    },
}

impl CodeUpdateNotice {
    pub(crate) fn harness_install(progress: crate::code::bus::HarnessInstallProgress) -> Self {
        Self::HarnessInstall {
            kind: progress.kind,
            version: progress.version,
            phase: progress.phase,
            done: progress.done,
            error: progress.error,
        }
    }

    pub(crate) fn clone_progress(progress: crate::code::bus::CloneProgress) -> Self {
        Self::CloneProgress {
            job: progress.job,
            phase: progress.phase,
            percent: progress.percent,
            done: progress.done,
            error: progress.error,
            repo_id: progress.repo_id,
        }
    }

    pub(crate) fn digest(digest: crate::code::bus::SessionDigest) -> Self {
        let wire = CodeSessionDigest::from(digest);
        Self::Digest {
            workspace: wire.workspace,
            session: wire.session,
            kind: wire.kind,
            lifecycle: wire.lifecycle,
            attention: wire.attention,
            title: wire.title,
            turn_count: wire.turn_count,
            activity: wire.activity,
            pr_state: wire.pr_state.map(Box::new),
            watch_state: wire.watch_state,
            watch_detail: wire.watch_detail,
            watch_cycles: wire.watch_cycles,
            subagents: wire.subagents,
        }
    }
}

/// Body of `POST /code/sessions/{id}/attention`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetAttentionBody {
    /// Drop a Manual pin and restore computed state.
    #[serde(default)]
    pub clear: bool,
    /// Pin a Manual note. Ignored when `clear` is true.
    #[serde(default)]
    pub note: Option<String>,
}

/// Body of `POST /code/workspaces/{id}/terminals`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTerminalBody {
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
}

/// Query for `GET /code/workspaces/{id}/terminals/{tid}/read`.
#[derive(Debug, Deserialize)]
pub struct TerminalReadQuery {
    #[serde(default)]
    pub cursor: u64,
}

/// Body of `POST /code/workspaces/{id}/terminals/{tid}/write`.
///
/// `bytes` is standard base64. Decoded length is capped by the write bound.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalWriteBody {
    pub bytes: String,
}

/// Body of `POST /code/workspaces/{id}/terminals/{tid}/resize`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalResizeBody {
    pub cols: u16,
    pub rows: u16,
}

/// Path of a workspace-scoped terminal.
#[derive(Debug, Deserialize)]
pub struct WorkspaceTerminalPath {
    pub id: WorkspaceId,
    pub tid: CodeTerminalId,
}

/// Used so capability flags stay reachable from the doctor root.
#[allow(dead_code)]
fn _cap_level(level: CapLevel) -> CapLevel {
    level
}
