use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::types::{
    ArchiveWorkspaceBody, CodeFileChange, CodeWorkspaceBlob, CodeWorkspaceDiff, CodeWorkspaceFiles,
    CodeWorkspaceSearch, CodeWorkspaceSearchMatch, CodeWorkspaceSnapshot, CodeWorkspaceTree,
    CreateWorkspaceBody, ListWorkspacesQuery, PatchWorkspaceBody, WorkspaceBlobQuery,
    WorkspaceDiffQuery, WorkspaceFilesQuery, WorkspaceSearchQuery, WorkspaceTreeQuery,
};
use tidebreak_core::WorkspaceId;

pub async fn create_workspace(
    code: ScopedCode,
    Json(body): Json<CreateWorkspaceBody>,
) -> Result<impl IntoResponse, ServerError> {
    let workspace = code
        .create_workspace(body.repo_id, body.title, body.base_ref)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeWorkspaceSnapshot::from(workspace)),
    ))
}

pub async fn list_workspaces(
    code: ScopedCode,
    Query(query): Query<ListWorkspacesQuery>,
) -> Result<Json<Vec<CodeWorkspaceSnapshot>>, ServerError> {
    let workspaces = code.list_workspaces(query.repo_id).await?;
    Ok(Json(
        workspaces
            .into_iter()
            .map(CodeWorkspaceSnapshot::from)
            .collect(),
    ))
}

pub async fn get_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    Ok(Json(CodeWorkspaceSnapshot::from(
        code.get_workspace(id).await?,
    )))
}

pub async fn patch_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<PatchWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let mut workspace = code.get_workspace(id).await?;
    if let Some(title) = body.title {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(ServerError::bad_request("title must not be empty"));
        }
        workspace.title = title;
        code.save_workspace(&workspace).await?;
    }
    Ok(Json(CodeWorkspaceSnapshot::from(workspace)))
}

pub async fn archive_workspace(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<ArchiveWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let archived = code.archive_workspace(id, body.force).await?;
    state.terminals.close_workspace(id);
    Ok(Json(CodeWorkspaceSnapshot::from(archived)))
}

pub async fn list_workspace_tree(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<CodeWorkspaceTree>, ServerError> {
    let (paths, truncated) = code
        .workspace_tree(id, query.query.as_deref().unwrap_or(""), query.limit)
        .await?;
    Ok(Json(CodeWorkspaceTree { paths, truncated }))
}

pub async fn search_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceSearchQuery>,
) -> Result<Json<CodeWorkspaceSearch>, ServerError> {
    let (matches, truncated) = code
        .workspace_search(
            id,
            &query.query,
            query.include.as_deref().unwrap_or(""),
            query.exclude.as_deref().unwrap_or(""),
            query.limit,
        )
        .await?;
    Ok(Json(CodeWorkspaceSearch {
        matches: matches
            .into_iter()
            .map(|matched| CodeWorkspaceSearchMatch {
                path: matched.path,
                line_number: matched.line_number,
                line: matched.line,
            })
            .collect(),
        truncated,
    }))
}

pub async fn get_workspace_blob(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceBlobQuery>,
) -> Result<Json<CodeWorkspaceBlob>, ServerError> {
    let blob = code.workspace_blob(id, &query.path).await?;
    Ok(Json(CodeWorkspaceBlob {
        path: blob.path,
        content: blob.content,
        truncated: blob.truncated,
        binary: blob.binary,
    }))
}

pub async fn list_workspace_files(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceFilesQuery>,
) -> Result<Json<CodeWorkspaceFiles>, ServerError> {
    let (files, truncated, stat, turn_id) = code.workspace_files(id, query.turn).await?;
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
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceDiffQuery>,
) -> Result<Json<CodeWorkspaceDiff>, ServerError> {
    let file = query
        .file
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let (diff, truncated, stat, turn_id) =
        code.workspace_diff(id, query.turn, file.as_deref()).await?;
    Ok(Json(CodeWorkspaceDiff {
        diff,
        truncated,
        stat,
        turn_id,
        file,
    }))
}
