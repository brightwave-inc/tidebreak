//! Commit, push, pull-request, and quick-action routes for a workspace.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::require_code;
use super::types::{
    CodeActionSnapshot, CodeCommitSnapshot, CodePushSnapshot, CodeWorkspacePrSnapshot,
    CommitWorkspaceBody, CreatePullRequestBody,
};
use crate::code::gh::{ActionOutcome, CommitOutcome, PushOutcome, WorkspaceGitStatus};
use tidebreak_core::WorkspaceId;

pub async fn commit_workspace(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CommitWorkspaceBody>,
) -> Result<Json<CodeCommitSnapshot>, ServerError> {
    let outcome = require_code(&state)?
        .commit_workspace(id, body.message)
        .await?;
    Ok(Json(commit_snapshot(outcome)))
}

pub async fn push_workspace(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodePushSnapshot>, ServerError> {
    let outcome = require_code(&state)?.push_workspace(id).await?;
    Ok(Json(push_snapshot(outcome)))
}

pub async fn create_pull_request(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CreatePullRequestBody>,
) -> Result<impl IntoResponse, ServerError> {
    let snapshot = require_code(&state)?
        .create_workspace_pr(id, body.title, body.body)
        .await?;
    Ok((StatusCode::CREATED, Json(pr_snapshot(snapshot))))
}

pub async fn get_workspace_pr(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspacePrSnapshot>, ServerError> {
    Ok(Json(pr_snapshot(
        require_code(&state)?.workspace_pr(id).await?,
    )))
}

pub async fn run_workspace_action(
    State(state): State<AppState>,
    Path((id, name)): Path<(WorkspaceId, String)>,
) -> Result<Json<CodeActionSnapshot>, ServerError> {
    let outcome = require_code(&state)?
        .run_workspace_action(id, &name)
        .await?;
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

fn pr_snapshot(status: WorkspaceGitStatus) -> CodeWorkspacePrSnapshot {
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
