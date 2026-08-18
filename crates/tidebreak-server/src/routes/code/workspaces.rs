use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::require_code;
use super::types::{
    ArchiveWorkspaceBody, CodeFileChange, CodeWorkspaceDiff, CodeWorkspaceFiles,
    CodeWorkspaceSnapshot, CodeWorkspaceTree, CreateWorkspaceBody, ListWorkspacesQuery,
    PatchWorkspaceBody, WorkspaceDiffQuery, WorkspaceFilesQuery, WorkspaceTreeQuery,
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
    let archived = require_code(&state)?
        .archive_workspace(id, body.force)
        .await?;
    state.terminals.close_workspace(id);
    Ok(Json(CodeWorkspaceSnapshot::from(archived)))
}

pub async fn list_workspace_tree(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<CodeWorkspaceTree>, ServerError> {
    let (paths, truncated) = require_code(&state)?
        .workspace_tree(id, query.query.as_deref().unwrap_or(""), query.limit)
        .await?;
    Ok(Json(CodeWorkspaceTree { paths, truncated }))
}

pub async fn list_workspace_files(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceFilesQuery>,
) -> Result<Json<CodeWorkspaceFiles>, ServerError> {
    let (files, truncated, stat, turn_id) = require_code(&state)?
        .workspace_files(id, query.turn)
        .await?;
    Ok(Json(CodeWorkspaceFiles {
        files: files
            .into_iter()
            .map(|file| CodeFileChange {
                path: file.path,
                kind: file.kind,
                insertions: file.insertions,
                deletions: file.deletions,
                previous_path: file.previous_path,
            })
            .collect(),
        truncated,
        stat,
        turn_id,
    }))
}

pub async fn get_workspace_diff(
    State(state): State<AppState>,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceDiffQuery>,
) -> Result<Json<CodeWorkspaceDiff>, ServerError> {
    let file = query
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let (diff, truncated, stat, turn_id) = require_code(&state)?
        .workspace_diff(id, query.turn, file.as_deref())
        .await?;
    Ok(Json(CodeWorkspaceDiff {
        diff,
        truncated,
        stat,
        turn_id,
        file,
    }))
}
