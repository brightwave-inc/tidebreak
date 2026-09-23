use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::routes::SERVED_BYTES_CONTENT_POLICY;
use crate::state::AppState;

use super::types::{
    ArchiveWorkspaceBody, CodeWorkspaceBlob, CodeWorkspaceDiff, CodeWorkspaceFileSaved,
    CodeWorkspaceFiles, CodeWorkspaceHistorySearchMatch, CodeWorkspaceHistorySearchSource,
    CodeWorkspaceSearch, CodeWorkspaceSearchMatch, CodeWorkspaceSnapshot, CodeWorkspaceTree,
    CodeWorktreeRoot, CreateRemoteWorkspaceBody, CreateWorkspaceBody, ListWorkspacesQuery,
    PatchWorkspaceBody, SaveWorkspaceFileBody, SetCodeWorktreeRootBody, WorkspaceBlobQuery,
    WorkspaceDiffQuery, WorkspaceFilesQuery, WorkspaceSearchQuery, WorkspaceTitleBody,
    WorkspaceTitleProposal, WorkspaceTreeQuery,
};
use tidebreak_core::WorkspaceId;

/// The largest `PUT /code/workspaces/{id}/file` body the route buffers.
///
/// The text itself is capped at the viewer's 512 KiB, checked after parsing so
/// an oversized save answers `413` with a sentence rather than a transport
/// error. JSON escaping can grow text up to six times (a control character
/// becomes `\u0001`), so the body limit leaves room for that and nothing more.
pub(crate) const MAX_WORKSPACE_FILE_SAVE_BODY_BYTES: usize =
    6 * crate::code::file_save::MAX_SAVE_BYTES + 64 * 1_024;

/// Naming improves the checkout but must never hold workspace creation behind
/// a slow provider stream. A later first turn can still update the display
/// title when this foreground attempt falls back.
const WORKSPACE_TITLE_DEADLINE: Duration = Duration::from_secs(10);

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
    let (workspace, base_refresh_warning) = code
        .create_workspace_with_warning(
            body.repo_id,
            body.title,
            body.suggested_title,
            body.base_ref,
        )
        .await?;
    let mut snapshot = CodeWorkspaceSnapshot::from(workspace);
    snapshot.base_refresh_warning = base_refresh_warning;
    Ok((StatusCode::CREATED, Json(snapshot)))
}

/// `POST /code/workspace-title` — name a workspace before its checkout exists.
pub async fn propose_workspace_title(
    State(state): State<AppState>,
    code: ScopedCode,
    Json(body): Json<WorkspaceTitleBody>,
) -> Result<Json<WorkspaceTitleProposal>, ServerError> {
    let title = match tokio::time::timeout(
        WORKSPACE_TITLE_DEADLINE,
        crate::code::titling::propose_for_creation(&state, code.owner(), &body.message),
    )
    .await
    {
        Ok(Ok(title)) => title,
        Ok(Err(error)) => {
            tracing::warn!(?error, "could not name workspace before creation");
            None
        }
        Err(_) => {
            tracing::warn!("workspace naming timed out before creation");
            None
        }
    };
    Ok(Json(WorkspaceTitleProposal { title }))
}

/// `POST /code/remote/workspaces` — create a workspace whose checkout lives
/// only in the configured sandbox runtime.
pub async fn create_remote_workspace(
    code: ScopedCode,
    Json(body): Json<CreateRemoteWorkspaceBody>,
) -> Result<impl IntoResponse, ServerError> {
    let workspace = code
        .create_remote_workspace(body.repo_id, body.title)
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
    let workspaces = code.list_readable_workspaces(query.repo_id).await?;
    let mut snapshots = Vec::with_capacity(workspaces.len());
    for workspace in workspaces {
        snapshots.push(workspace_snapshot(&code, workspace).await?);
    }
    Ok(Json(snapshots))
}

pub async fn get_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let workspace = code.read_workspace(id).await?;
    Ok(Json(workspace_snapshot(&code, workspace).await?))
}

pub async fn patch_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<PatchWorkspaceBody>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let mut workspace = code.require_workspace_management(id).await?;
    if let Some(title) = body.title {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(ServerError::bad_request("title must not be empty"));
        }
        workspace.title = title;
        code.save_workspace(&workspace).await?;
    }
    Ok(Json(workspace_snapshot(&code, workspace).await?))
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
    Ok(Json(workspace_snapshot(&code, archived).await?))
}

/// `POST /code/workspaces/{id}/restore` — reactivate an archived workspace.
///
/// The worktree comes back at the same path. A released workspace rebuilds
/// from its saved bundle; a remote workspace keeps its remote recovery path.
/// Session rows and journal history were never deleted, so the conversation
/// is readable again the moment this returns. 409 kinds: `branch_missing`
/// (the branch was deleted since an older archive — fall back to a new
/// workspace), `released_branch_mismatch`, `released_tip_mismatch`,
/// `worktree_path_busy`, and `worktree_path_occupied`.
pub async fn restore_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<CodeWorkspaceSnapshot>, ServerError> {
    let restored = code.restore_workspace(id).await?;
    Ok(Json(workspace_snapshot(&code, restored).await?))
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
    Ok(Json(workspace_snapshot(&code, workspace).await?))
}

pub async fn list_workspace_tree(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<CodeWorkspaceTree>, ServerError> {
    let (paths, truncated, source) = code
        .workspace_tree(id, query.query.as_deref().unwrap_or(""), query.limit)
        .await?;
    Ok(Json(CodeWorkspaceTree {
        paths,
        truncated,
        revision: source.as_ref().map(|source| source.revision),
        revision_saved_at: source.as_ref().and_then(|source| source.saved_at),
        revision_ref: source.and_then(|source| source.revision_ref),
    }))
}

pub async fn search_workspace(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceSearchQuery>,
) -> Result<Json<CodeWorkspaceSearch>, ServerError> {
    if query.history {
        if query
            .include
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            || query
                .exclude
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        {
            return Err(ServerError::bad_request(
                "history search does not support path include or exclude filters",
            ));
        }
        let history_query = query.query.trim();
        if history_query.chars().count()
            > tidebreak_core::db::code::MAX_TRANSCRIPT_SEARCH_QUERY_CHARS
        {
            return Err(ServerError::bad_request(format!(
                "search query must be at most {} characters",
                tidebreak_core::db::code::MAX_TRANSCRIPT_SEARCH_QUERY_CHARS
            )));
        }
        let searched = code
            .workspace_transcript_search(id, history_query, query.limit)
            .await?;
        return Ok(Json(CodeWorkspaceSearch {
            matches: Vec::new(),
            history_matches: searched
                .matches
                .into_iter()
                .map(|matched| CodeWorkspaceHistorySearchMatch {
                    workspace_id: matched.workspace_id,
                    workspace_title: matched.workspace_title,
                    session_id: matched.session_id,
                    turn_id: matched.turn_id,
                    source: match matched.source {
                        tidebreak_core::db::code::CodeTranscriptSearchSource::TurnUserInput => {
                            CodeWorkspaceHistorySearchSource::TurnUserInput
                        }
                        tidebreak_core::db::code::CodeTranscriptSearchSource::TurnNarrative => {
                            CodeWorkspaceHistorySearchSource::TurnNarrative
                        }
                        tidebreak_core::db::code::CodeTranscriptSearchSource::Event => {
                            CodeWorkspaceHistorySearchSource::Event
                        }
                    },
                    preview: matched.preview,
                    created_at: matched.created_at,
                })
                .collect(),
            truncated: searched.truncated,
        }));
    }
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
        history_matches: Vec::new(),
        truncated,
    }))
}

pub async fn get_workspace_blob(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceBlobQuery>,
) -> Result<Json<CodeWorkspaceBlob>, ServerError> {
    let (blob, source) = code.workspace_blob(id, &query.path).await?;
    Ok(Json(CodeWorkspaceBlob {
        path: blob.path,
        content: blob.content,
        truncated: blob.truncated,
        binary: blob.binary,
        hash: blob.hash,
        revision: source.as_ref().map(|source| source.revision),
        revision_saved_at: source.as_ref().and_then(|source| source.saved_at),
        revision_ref: source.and_then(|source| source.revision_ref),
    }))
}

pub async fn get_workspace_file(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceBlobQuery>,
) -> Result<Response, ServerError> {
    let file = code.workspace_file(id, &query.path).await?;
    let content_length = u64::try_from(file.bytes.len())
        .map_err(|_| ServerError::internal("workspace file length exceeds u64"))?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, file.media_type)
        .header(header::CONTENT_LENGTH, content_length.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, SERVED_BYTES_CONTENT_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CONTENT_DISPOSITION, "inline")
        .body(Body::from(file.bytes))
        .map_err(|error| {
            ServerError::internal(format!("failed to build workspace file response: {error}"))
        })
}

/// `PUT /code/workspaces/{id}/file` — save one existing text file.
///
/// The file viewer's editor sends the text with the hash it loaded from
/// `GET /blob`. Kinds a client branches on: `409 file_changed` (the file
/// moved on disk; `current_hash` names what is there now), `409
/// workspace_remote` (a sandbox workspace, which is read-only here), and
/// `413 payload_too_large`. A caller who may only view the workspace gets
/// `404`, as for commit and push.
pub async fn save_workspace_file(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<SaveWorkspaceFileBody>,
) -> Result<Json<CodeWorkspaceFileSaved>, ServerError> {
    let saved = code
        .save_workspace_file(id, &body.path, &body.content, &body.base_hash)
        .await?;
    Ok(Json(CodeWorkspaceFileSaved {
        path: saved.path,
        hash: saved.hash,
    }))
}

pub async fn list_workspace_files(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceFilesQuery>,
) -> Result<Json<CodeWorkspaceFiles>, ServerError> {
    let (files, truncated, stat, turn_id, source) = code.workspace_files(id, query.turn).await?;
    Ok(Json(CodeWorkspaceFiles {
        files: super::undo::file_changes(files),
        truncated,
        stat,
        turn_id,
        revision: source.as_ref().map(|source| source.revision),
        revision_saved_at: source.as_ref().and_then(|source| source.saved_at),
        revision_ref: source.and_then(|source| source.revision_ref),
    }))
}

pub async fn get_workspace_diff(
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Query(query): Query<WorkspaceDiffQuery>,
) -> Result<Json<CodeWorkspaceDiff>, ServerError> {
    let file = exact_diff_file(query.file);
    let (diff, truncated, stat, turn_id, source) =
        code.workspace_diff(id, query.turn, file.as_deref()).await?;
    Ok(Json(CodeWorkspaceDiff {
        diff,
        truncated,
        stat,
        turn_id,
        file,
        revision: source.as_ref().map(|source| source.revision),
        revision_saved_at: source.as_ref().and_then(|source| source.saved_at),
        revision_ref: source.and_then(|source| source.revision_ref),
    }))
}

fn exact_diff_file(file: Option<String>) -> Option<String> {
    file.filter(|value| !value.is_empty())
}

async fn workspace_snapshot(
    code: &ScopedCode,
    workspace: tidebreak_core::CodeWorkspace,
) -> Result<CodeWorkspaceSnapshot, ServerError> {
    let is_owner = workspace.owner == *code.owner();
    let read_only = !code.can_manage_workspace(&workspace).await?;
    let repo_display_name = code.workspace_repo_display_name(workspace.id).await?;
    let mut snapshot = CodeWorkspaceSnapshot::from(workspace);
    snapshot.read_only = Some(read_only);
    snapshot.is_owner = Some(is_owner);
    snapshot.repo_display_name = repo_display_name;
    Ok(snapshot)
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
