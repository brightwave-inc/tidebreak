//! Review changes: another engine reads a workspace's changes, read-only,
//! and its findings come back for the diff to show as line comments.
//!
//! Starting and stopping a review takes what commit and push take; a caller
//! who may only view the workspace gets `404`. Reading reviews takes what
//! reading the diff takes. A review runs on its own task and never takes the
//! workspace's turn lock, so the conversation it was started from keeps
//! taking turns while it runs.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;
use tidebreak_core::{CodeReviewId, WorkspaceId};

use super::types::{CodeReviewList, CodeReviewSnapshot, StartCodeReviewBody};

/// Path of one review.
#[derive(Debug, Deserialize)]
pub struct WorkspaceReviewPath {
    pub id: WorkspaceId,
    pub review_id: CodeReviewId,
}

/// `POST /code/workspaces/{id}/reviews` — start a review; `202` with the
/// review as it starts. Poll `GET .../reviews/{review_id}` for progress.
///
/// Kinds a client branches on: `422 harness_not_found` and
/// `422 harness_not_authenticated` (the engine cannot run here),
/// `422 review_engine_unsupported` (it has no read-only posture),
/// `409 permission_mode_locked` (above the managed ceiling),
/// `409 review_empty` (nothing to review), `409 review_running` (another
/// review of this workspace is running), and `409 workspace_remote`.
pub async fn start_review(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<StartCodeReviewBody>,
) -> Result<impl IntoResponse, ServerError> {
    let permission_mode_ceiling = state.managed_policy()?.permission_mode_ceiling;
    let review = code.start_review(id, body, permission_mode_ceiling).await?;
    Ok((StatusCode::ACCEPTED, Json(review)))
}

/// `GET /code/workspaces/{id}/reviews` — this workspace's recent reviews,
/// newest first, running ones included.
pub async fn list_reviews(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeReviewList>, ServerError> {
    Ok(Json(CodeReviewList {
        reviews: code.list_reviews(id).await?,
    }))
}

/// `GET /code/workspaces/{id}/reviews/{review_id}` — one review.
pub async fn get_review(
    code: ScopedCode,
    Path(path): Path<WorkspaceReviewPath>,
) -> Result<Json<CodeReviewSnapshot>, ServerError> {
    Ok(Json(code.get_review(path.id, path.review_id).await?))
}

/// `POST /code/workspaces/{id}/reviews/{review_id}/cancel` — stop a running
/// review. The answer is the review as the request found it; it reads
/// `cancelled` once the reviewer has stopped.
pub async fn cancel_review(
    code: ScopedCode,
    Path(path): Path<WorkspaceReviewPath>,
) -> Result<Json<CodeReviewSnapshot>, ServerError> {
    Ok(Json(code.cancel_review(path.id, path.review_id).await?))
}
