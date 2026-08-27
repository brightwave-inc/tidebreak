use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::types::{
    ArchiveWorkspaceBody, CodeCheckpointRestore, CodeFileChange, CodeWorkspaceBlob,
    CodeWorkspaceDiff, CodeWorkspaceFiles, CodeWorkspaceSearch, CodeWorkspaceSearchMatch,
    CodeWorkspaceSnapshot, CodeWorkspaceTree, CodeWorktreeRoot, CreateWorkspaceBody,
    ListWorkspacesQuery, PatchWorkspaceBody, RestoreCheckpointBody, SetCodeWorktreeRootBody,
    WorkspaceBlobQuery, WorkspaceDiffQuery, WorkspaceFilesQuery, WorkspaceSearchQuery,
    WorkspaceTreeQuery,
};
use tidebreak_core::WorkspaceId;

/// `GET /code/worktree-root`: where the next workspace's worktree lands.
pub async fn get_worktree_root(code: ScopedCode) -> Result<Json<CodeWorktreeRoot>, ServerError> {
    Ok(Json(code.worktree_root().await?))
}

/// `PUT /code/worktree-root`.
///
/// New workspaces only. Worktrees already on disk keep the absolute path
/// stored on their row, because a git worktree records absolute paths in two
/// places that a move would have to repair.
pub async fn set_worktree_root(
    code: ScopedCode,
    Json(body): Json<SetCodeWorktreeRootBody>,
) -> Result<Json<CodeWorktreeRoot>, ServerError> {
    Ok(Json(code.set_worktree_root(body.root.as_deref()).await?))
}

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
    let archived = code
        .archive_workspace(id, body.force, state.terminals.as_ref())
        .await?;
    Ok(Json(CodeWorkspaceSnapshot::from(archived)))
}

/// `POST /code/workspaces/{id}/release` — reclaim an archived workspace's branch.
///
/// The deepest reclaim tier. The branch's own commits are bundled beside the
/// database and the ref is dropped, which frees the objects the branch held
/// alive. `restore_workspace` puts it back from that bundle, so the work stays
/// rebuildable and the transcript is untouched. 409 kinds:
/// `workspace_not_archived`, and `branch_unmerged` (the branch has commits the
/// base does not — pass force).
pub async fn release_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<ArchiveWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let released = code.release_workspace(id, body.force).await?;
    Ok(Json(CodeWorkspaceSnapshot::from(released)))
}

/// `POST /code/workspaces/{id}/restore` — reactivate an archived workspace.
///
/// The worktree comes back at the same path on the kept branch; session rows
/// and journal history were never deleted, so the conversation is readable
/// again the moment this returns. 409 kinds: `branch_missing` (the branch was
/// deleted since archive — fall back to a new workspace),
/// `released_branch_mismatch`, `released_tip_mismatch`,
/// `worktree_path_busy`, and `worktree_path_occupied`.
pub async fn restore_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let restored = code.restore_workspace(id).await?;
    Ok(Json(CodeWorkspaceSnapshot::from(restored)))
}

/// `POST /code/workspaces/{id}/retry-setup` — run the setup script again on the
/// worktree this workspace already has.
///
/// The only way out of `setup_failed` that keeps the checkout. Success returns
/// the now-Active workspace; another failure returns 422 `setup_failed`.
pub async fn retry_workspace_setup(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let workspace = code.retry_workspace_setup(id).await?;
    Ok(Json(CodeWorkspaceSnapshot::from(workspace)))
}

/// `POST /code/workspaces/{id}/restore-checkpoint` — put the files back to a
/// turn.
///
/// Not `/restore`, which reactivates an archived workspace. This one touches
/// files: the worktree goes back to what the turn's checkpoint holds, the
/// branch and `HEAD` stay where they are, and ignored files are left alone.
/// The worktree as it stands is snapshotted into a hidden ref first. 409
/// kinds: `workspace_busy` (a turn holds the workspace's checkout),
/// `workspace_not_ready`, `turn_running`, `session_fenced`,
/// `workspace_fenced` (a sibling is fenced until it is reaped), and
/// `no_checkpoint` (the turn ended before checkpoints, or its ref was reaped).
pub async fn restore_workspace_checkpoint(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<RestoreCheckpointBody>,
) -> Result<Json<CodeCheckpointRestore>, ServerError> {
    let restored = code.restore_workspace_to_turn(id, body.turn_id).await?;
    Ok(Json(CodeCheckpointRestore {
        turn_id: restored.turn_id,
        safety_ref: restored.safety_ref,
        stat: restored.diffstat,
    }))
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
                path: file.path.to_wire(),
                kind: file.kind,
                insertions: file.insertions,
                deletions: file.deletions,
                previous_path: file.previous_path.map(|path| path.to_wire()),
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
    let file = exact_diff_file(query.file);
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

fn exact_diff_file(file: Option<String>) -> Option<String> {
    file.filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::exact_diff_file;

    #[test]
    fn diff_file_keeps_path_whitespace_exact() {
        assert_eq!(
            exact_diff_file(Some(" leading and trailing ".to_owned())).as_deref(),
            Some(" leading and trailing ")
        );
        assert_eq!(exact_diff_file(Some(String::new())), None);
    }
}
