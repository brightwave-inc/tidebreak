//! Renderer-facing delivery wire types.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use tidebreak_core::{CodePullRequestRelation, CodeWorkspaceStatus, RepoId, WorkspaceId};

/// Merge strategy for a user-initiated PR merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CodePrMergeMethod {
    Squash,
    Merge,
    Rebase,
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

/// Whether this caller's GitHub path can serve Delivery requests.
///
/// Desktops and self-host machines report the local GitHub CLI. A
/// gateway-authenticated hosted machine reports the caller's connected forge.
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
    /// Durable attribution behind this link, when one is stored: the
    /// workspace authored or contributed to the pull request (decision 77).
    /// Absent on links the live heuristic derived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relation: Option<CodePullRequestRelation>,
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
    /// True when the last reliable host observation placed the pull request
    /// in its merge queue. Absent when the list read cannot answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub in_merge_queue: Option<bool>,
    /// Issue comments visible from the list read. Review and inline comments
    /// remain detail-only, so an absent count means unknown rather than zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub comment_count: Option<u64>,
    pub checks: Vec<CodeDeliveryCheck>,
    pub attention_reasons: Vec<CodeDeliveryPrAttentionReason>,
    pub ready_to_merge: bool,
    pub workspace_links: Vec<CodeDeliveryWorkspaceLink>,
    /// The host stack this pull request belongs to (GitHub stacked pull
    /// requests), when the host reported one. Identifies the stack, not the
    /// PR.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stack_number: Option<u64>,
    /// Total layers in that stack, bottom to top, including merged ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stack_size: Option<u64>,
    /// The pull request this one is stacked on. Host stack order wins when
    /// the host reported a stack; branch inference from the durable fact set
    /// is the fallback (decision 77), so a parent outside the current page
    /// or filter still resolves. Absent when the base is the default branch
    /// or nothing tracked owns it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stack_parent_number: Option<u64>,
    /// A stack-shaped chain of inferred edges the host has no stack for,
    /// bottom to top, when one resolves gaplessly around this pull request
    /// and no member is host-registered. Creating the stack on GitHub makes
    /// the host own the ordering, the retargeting, and the whole-chain merge
    /// — without it, merging a layer lands it into the branch below rather
    /// than the default branch, which is easy to do by accident.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unregistered_stack_numbers: Option<Vec<u64>>,
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

/// One layer of a pull-request stack, in bottom-to-top order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryStackMember {
    pub number: u64,
    /// Host state token (open, closed).
    pub state: String,
    pub draft: bool,
    /// Set once this layer merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_at: Option<String>,
    /// Head branch name.
    pub head_branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub head_sha: Option<String>,
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
    /// The full stack chain this pull request belongs to, bottom to top,
    /// when the host reported one. Absent on hosts without stacked pull
    /// requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stack: Option<Vec<CodeDeliveryStackMember>>,
    pub commits: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub merged_by: Option<String>,
    /// Empty when the diff could not be read. Truncated by `files_truncated`
    /// rather than paged: the panel is a review aid, not a diff viewer.
    pub files: Vec<CodeDeliveryPullRequestFile>,
    pub files_truncated: bool,
    pub comments: Vec<tidebreak_core::PullRequestComment>,
    /// Section reads that failed after the pull request itself loaded.
    pub errors: Vec<CodeDeliverySourceError>,
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
        #[serde(default)]
        admin: bool,
        expected_head_sha: String,
    },
    CreateStack {
        /// The chain to register, bottom to top. Every pull request's base
        /// ref must match the previous one's head ref.
        numbers: Vec<u64>,
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

/// Result of rerunning one GitHub Actions workflow run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryRerunOutcome {
    pub workflow_run_id: u64,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}

/// Delivery mutation result. A partial rerun returns every per-run outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
pub struct CodeDeliveryActionResult {
    pub success: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rerun_outcomes: Vec<CodeDeliveryRerunOutcome>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run_attempt: Option<u64>,
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
    /// Section reads that failed after the run or deployment itself loaded.
    pub errors: Vec<CodeDeliverySourceError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeDeliveryRunAction {
    Rerun,
    RerunFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CodeDeliveryRunActionBody {
    pub target: CodeDeliveryRunTarget,
    pub action: CodeDeliveryRunAction,
}
