use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::require_code;
use super::types::{
    ArchiveWorkspaceBody, CodeWorkspaceSnapshot, CreateWorkspaceBody, ListWorkspacesQuery,
    PatchWorkspaceBody,
};
use tidebreak_core::WorkspaceId;

pub async fn create_workspace(
    State(state): State<AppState>,
    Json(body): Json<CreateWorkspaceBody>,
) -> Result<impl IntoResponse, ServerError> {
    let workspace = require_code(&state)?
        .create_workspace(body.repo_id, body.title, body.base_ref)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeWorkspaceSnapshot::from(workspace)),
    ))
}

pub async fn list_workspaces(
    State(state): State<AppState>,
    Query(query): Query<ListWorkspacesQuery>,
) -> Result<Json<Vec<CodeWorkspaceSnapshot>>, ServerError> {
    let workspaces = require_code(&state)?.list_workspaces(query.repo_id).await?;
    Ok(Json(
        workspaces
            .into_iter()
            .map(CodeWorkspaceSnapshot::from)
            .collect(),
    ))
}

pub async fn get_workspace(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    Ok(Json(CodeWorkspaceSnapshot::from(
        require_code(&state)?.get_workspace(id).await?,
    )))
}

pub async fn patch_workspace(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<PatchWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let runtime = require_code(&state)?;
    let mut workspace = runtime.get_workspace(id).await?;
    if let Some(title) = body.title {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(ServerError::bad_request("title must not be empty"));
        }
        workspace.title = title;
        runtime.save_workspace(&workspace).await?;
    }
    Ok(Json(CodeWorkspaceSnapshot::from(workspace)))
}

pub async fn archive_workspace(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<ArchiveWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    Ok(Json(CodeWorkspaceSnapshot::from(
        require_code(&state)?
            .archive_workspace(id, body.force)
            .await?,
    )))
}
