//! Commit, push, pull-request, and quick-action routes for a workspace.

use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};

use super::types::{
    CodeActionSnapshot, CodeCheckLog, CodeCheckLogError, CodeCheckLogsSnapshot, CodeCommitSnapshot,
    CodePrCommentsSnapshot, CodePrMergeMethod, CodePushSnapshot, CodeWatchSnapshot,
    CodeWorkspacePrSnapshot, CodeWorkspacePullRequestFact, CodeWorkspacePullRequests,
    CommitWorkspaceBody, CreatePullRequestBody, MergeCodePrBody,
};
use crate::code::gh::{ActionOutcome, CommitOutcome, MergeMethod, PushOutcome, WorkspaceGitStatus};
use tidebreak_core::WorkspaceId;

pub async fn commit_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CommitWorkspaceBody>,
) -> Result<Json<CodeCommitSnapshot>, ServerError> {
    let outcome = code.commit_workspace(id, body.message).await?;
    Ok(Json(commit_snapshot(outcome)))
}

pub async fn push_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodePushSnapshot>, ServerError> {
    let outcome = code.push_workspace(id).await?;
    Ok(Json(push_snapshot(outcome)))
}

pub async fn create_pull_request(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CreatePullRequestBody>,
) -> Result<impl IntoResponse, ServerError> {
    let snapshot = code.create_workspace_pr(id, body.title, body.body).await?;
    Ok((StatusCode::CREATED, Json(pr_snapshot(snapshot, None))))
}

pub async fn get_workspace_pr(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspacePrSnapshot>, ServerError> {
    let status = code.workspace_pr(id).await?;
    let watch = code.latest_watch(id).await?;
    Ok(Json(pr_snapshot(status, watch)))
}

/// Every pull request attributed to the workspace (decision 62), from the
/// durable fact store — no live host read.
pub async fn list_workspace_pull_requests(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspacePullRequests>, ServerError> {
    let facts = code.workspace_pull_requests(id).await?;
    let items = facts
        .into_iter()
        .map(|(fact, relation)| CodeWorkspacePullRequestFact {
            host: fact.host,
            repo_owner: fact.repo_owner,
            repo_name: fact.repo_name,
            number: fact.number,
            url: fact.url,
            title: fact.title,
            state: fact.state.as_str().to_owned(),
            draft: fact.draft,
            author: fact.author,
            head_branch: fact.head_branch,
            base_branch: fact.base_branch,
            head_sha: fact.head_sha,
            relation,
            created_at: fact.created_at,
            updated_at: fact.updated_at,
            merged_at: fact.merged_at,
            closed_at: fact.closed_at,
            last_seen_at: fact.last_seen_at,
        })
        .collect();
    Ok(Json(CodeWorkspacePullRequests {
        items,
        fetched_at: chrono::Utc::now(),
    }))
}

pub async fn refresh_workspace_pr(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspacePrSnapshot>, ServerError> {
    let status = code.refresh_workspace_pr(id).await?;
    let watch = code.latest_watch(id).await?;
    Ok(Json(pr_snapshot(status, watch)))
}

pub async fn start_workspace_watch(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<impl IntoResponse, ServerError> {
    let watch = code.start_watch(id).await?;
    Ok((StatusCode::CREATED, Json(CodeWatchSnapshot::from(watch))))
}

pub async fn stop_workspace_watch(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWatchSnapshot>, ServerError> {
    let watch = code.stop_watch(id).await?;
    Ok(Json(CodeWatchSnapshot::from(watch)))
}

pub async fn get_workspace_pr_comments(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodePrCommentsSnapshot>, ServerError> {
    let comments = code.workspace_pr_comments(id).await?;
    Ok(Json(CodePrCommentsSnapshot {
        number: comments.number,
        comments: comments.comments,
    }))
}

/// `POST /code/workspaces/{id}/pr/check-logs` — download the failing checks'
/// job logs and report where they landed.
///
/// A write, so a POST. The fix-errors action calls this before it sends its
/// prompt: the agent then opens a bounded file instead of spending its first
/// turns working out which job failed and asking GitHub for the whole log.
pub async fn write_workspace_check_logs(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<(StatusCode, Json<CodeCheckLogsSnapshot>), ServerError> {
    let (head_sha, written) = code.workspace_check_logs(id).await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeCheckLogsSnapshot {
            head_sha,
            logs: written
                .logs
                .into_iter()
                .map(|log| CodeCheckLog {
                    check: log.check,
                    path: log.path,
                    byte_len: log.byte_len,
                    truncated: log.truncated,
                    url: log.url,
                })
                .collect(),
            errors: written
                .failures
                .into_iter()
                .map(|failure| CodeCheckLogError {
                    check: failure.check,
                    message: failure.message,
                })
                .collect(),
        }),
    ))
}

pub async fn merge_workspace_pr(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<MergeCodePrBody>,
) -> Result<Json<CodeWorkspacePrSnapshot>, ServerError> {
    let method = match body.method {
        CodePrMergeMethod::Squash => MergeMethod::Squash,
        CodePrMergeMethod::Merge => MergeMethod::Merge,
        CodePrMergeMethod::Rebase => MergeMethod::Rebase,
    };
    let status = code.merge_workspace_pr(id, method, body.auto).await?;
    let watch = code.latest_watch(id).await?;
    Ok(Json(pr_snapshot(status, watch)))
}

pub async fn mark_workspace_pr_ready(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspacePrSnapshot>, ServerError> {
    let status = code.mark_workspace_pr_ready(id).await?;
    let watch = code.latest_watch(id).await?;
    Ok(Json(pr_snapshot(status, watch)))
}

pub async fn run_workspace_action(
    code: ScopedCode,
    Path((id, name)): Path<(WorkspaceId, String)>,
) -> Result<Json<CodeActionSnapshot>, ServerError> {
    let outcome = code.run_workspace_action(id, &name).await?;
    Ok(Json(action_snapshot(outcome)))
}

fn commit_snapshot(outcome: CommitOutcome) -> CodeCommitSnapshot {
    CodeCommitSnapshot {
        sha: outcome.sha,
        message: outcome.message,
        stat: outcome.stat,
    }
}

fn push_snapshot(outcome: PushOutcome) -> CodePushSnapshot {
    CodePushSnapshot {
        branch: outcome.branch,
        remote: outcome.remote,
    }
}

fn pr_snapshot(
    status: WorkspaceGitStatus,
    watch: Option<tidebreak_core::CodeWatch>,
) -> CodeWorkspacePrSnapshot {
    CodeWorkspacePrSnapshot {
        dirty: status.dirty,
        unpushed: status.unpushed,
        ahead: status.ahead,
        has_upstream: status.has_upstream,
        suggested_commit_message: status.suggested_commit_message,
        pr: status.pr,
        gh_found: status.gh_found,
        gh_authenticated: status.gh_authenticated,
        remediation: status.remediation,
        pushes_as: status.pushes_as,
        watch: watch.map(CodeWatchSnapshot::from),
    }
}

fn action_snapshot(outcome: ActionOutcome) -> CodeActionSnapshot {
    CodeActionSnapshot {
        name: outcome.name,
        success: outcome.success,
        exit_code: outcome.exit_code,
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        timed_out: outcome.timed_out,
    }
}
