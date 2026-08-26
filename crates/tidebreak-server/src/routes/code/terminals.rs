use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::code::terminal::{TerminalError, TerminalRead, TerminalSnapshot};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;
use tidebreak_core::{CodeWorkspaceStatus, WorkspaceId};

use super::types::{
    CodeTerminalRead, CodeTerminalSnapshot, CreateTerminalBody, TerminalReadQuery,
    TerminalResizeBody, TerminalWriteBody, WorkspaceTerminalPath,
};

pub async fn create_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CreateTerminalBody>,
) -> Result<impl IntoResponse, ServerError> {
    let workspace = code.get_workspace(id).await?;
    require_active(&workspace.status)?;
    let write = code.workspace_write_lock(id);
    let _write_guard = write.lock().await;
    let workspace = code.get_workspace(id).await?;
    require_active(&workspace.status)?;
    let snap = state
        .terminals
        .open(
            code.owner(),
            id,
            std::path::Path::new(&workspace.worktree_path),
            body.cols,
            body.rows,
        )
        .map_err(map_terminal)?;
    Ok((StatusCode::CREATED, Json(snapshot_wire(snap))))
}

pub async fn list_terminals(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<Vec<CodeTerminalSnapshot>>, ServerError> {
    let _ = code.get_workspace(id).await?;
    Ok(Json(
        state
            .terminals
            .list(id)
            .into_iter()
            .map(snapshot_wire)
            .collect(),
    ))
}

pub async fn close_workspace_terminals(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<StatusCode, ServerError> {
    let _ = code.get_workspace(id).await?;
    state.terminals.close_workspace(id);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn close_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
) -> Result<StatusCode, ServerError> {
    let _ = code.get_workspace(path.id).await?;
    state
        .terminals
        .close(path.id, path.tid)
        .map_err(map_terminal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn read_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
    Query(query): Query<TerminalReadQuery>,
) -> Result<Json<CodeTerminalRead>, ServerError> {
    let _ = code.get_workspace(path.id).await?;
    Ok(Json(read_wire(
        path.tid,
        path.id,
        state.terminals.read(path.id, path.tid, query.cursor),
    )))
}

pub async fn write_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
    Json(body): Json<TerminalWriteBody>,
) -> Result<StatusCode, ServerError> {
    let workspace = code.get_workspace(path.id).await?;
    require_active(&workspace.status)?;
    let write = code.workspace_write_lock(path.id);
    let _write_guard = write.lock().await;
    let workspace = code.get_workspace(path.id).await?;
    require_active(&workspace.status)?;
    let bytes = decode_write(&body.bytes)?;
    state
        .terminals
        .write(path.id, path.tid, &bytes)
        .map_err(map_terminal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resize_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
    Json(body): Json<TerminalResizeBody>,
) -> Result<Json<CodeTerminalSnapshot>, ServerError> {
    let workspace = code.get_workspace(path.id).await?;
    require_active(&workspace.status)?;
    let write = code.workspace_write_lock(path.id);
    let _write_guard = write.lock().await;
    let workspace = code.get_workspace(path.id).await?;
    require_active(&workspace.status)?;
    let snap = state
        .terminals
        .resize(path.id, path.tid, body.cols, body.rows)
        .map_err(map_terminal)?;
    Ok(Json(snapshot_wire(snap)))
}

fn require_active(status: &CodeWorkspaceStatus) -> Result<(), ServerError> {
    if *status != CodeWorkspaceStatus::Active {
        return Err(ServerError::conflict_kind(
            "workspace_not_ready",
            format!("workspace is {}", status.as_str()),
        ));
    }
    Ok(())
}

fn decode_write(encoded: &str) -> Result<Vec<u8>, ServerError> {
    BASE64.decode(encoded.trim()).map_err(|_| {
        ServerError::bad_request_kind(
            "invalid_terminal_bytes",
            "write body is not standard base64",
        )
    })
}

fn snapshot_wire(snap: TerminalSnapshot) -> CodeTerminalSnapshot {
    CodeTerminalSnapshot {
        id: snap.id,
        workspace_id: snap.workspace_id,
        cols: snap.cols,
        rows: snap.rows,
        ended: snap.ended,
        created_at: snap.created_at,
    }
}

fn read_wire(
    id: tidebreak_core::CodeTerminalId,
    workspace_id: WorkspaceId,
    read: TerminalRead,
) -> CodeTerminalRead {
    CodeTerminalRead {
        id,
        workspace_id,
        bytes: BASE64.encode(read.data),
        cursor: read.next_cursor,
        overflow: read.overflow,
        truncated: read.truncated,
        ended: read.ended,
    }
}

fn map_terminal(err: TerminalError) -> ServerError {
    match err {
        TerminalError::WorkspaceCap => ServerError::too_many_requests_kind(
            "terminal_cap",
            "this workspace already has as many terminals as it can keep",
        ),
        TerminalError::WriteTooLarge => {
            ServerError::payload_too_large("terminal write exceeds the per-request cap")
        }
        TerminalError::Ended => {
            ServerError::conflict_kind("terminal_ended", "this shell has ended")
        }
        TerminalError::NotFound => ServerError::not_found("terminal not found"),
        TerminalError::InvalidSize => {
            ServerError::bad_request_kind("invalid_terminal_size", "cols and rows must be 1..=512")
        }
        TerminalError::Spawn(message) | TerminalError::Io(message) => {
            ServerError::internal(message)
        }
    }
}
