use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

use crate::code::terminal::{TerminalError, TerminalRead, TerminalSnapshot};
use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;
use tidebreak_core::{HarnessKind, WorkspaceId};

use super::types::{
    CodeTerminalRead, CodeTerminalSnapshot, CreateTerminalBody, HarnessSignInPath,
    HarnessSignInRead, HarnessSignInTerminal, TerminalReadQuery, TerminalResizeBody,
    TerminalWriteBody, WorkspaceTerminalPath,
};

pub async fn create_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
    Json(body): Json<CreateTerminalBody>,
) -> Result<impl IntoResponse, ServerError> {
    let workspace = code.require_live_workspace(id).await?;
    // The first terminal of a launch captures the login environment, which
    // can take a moment. Do it before taking the workspace's write lock.
    let mut launch = code
        .shell_launch(std::path::Path::new(&workspace.worktree_path))
        .await;
    let write = code.workspace_write_lock(id);
    let _write_guard = write.lock().await;
    let workspace = code.require_live_workspace(id).await?;
    launch.cwd = std::path::PathBuf::from(&workspace.worktree_path);
    let snap = state
        .terminals
        .open(code.owner(), id, &launch, body.cols, body.rows)
        .map_err(map_terminal)?;
    Ok((StatusCode::CREATED, Json(snapshot_wire(snap))))
}

pub async fn list_terminals(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(id): Path<WorkspaceId>,
) -> Result<Json<Vec<CodeTerminalSnapshot>>, ServerError> {
    let _ = code.require_workspace_owner(id).await?;
    Ok(Json(
        state
            .terminals
            .list(id)
            .into_iter()
            .map(snapshot_wire)
            .collect(),
    ))
}

pub async fn close_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
) -> Result<StatusCode, ServerError> {
    let _ = code.require_workspace_owner(path.id).await?;
    let terminals = state.terminals.clone();
    tokio::task::spawn_blocking(move || terminals.close(path.id, path.tid))
        .await
        .map_err(|error| map_terminal(TerminalError::Io(error.to_string())))?
        .map_err(map_terminal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn read_terminal(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<WorkspaceTerminalPath>,
    Query(query): Query<TerminalReadQuery>,
) -> Result<Json<CodeTerminalRead>, ServerError> {
    let _ = code.require_workspace_owner(path.id).await?;
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
    code.require_live_workspace(path.id).await?;
    let write = code.workspace_write_lock(path.id);
    let _write_guard = write.lock().await;
    code.require_live_workspace(path.id).await?;
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
    code.require_live_workspace(path.id).await?;
    let write = code.workspace_write_lock(path.id);
    let _write_guard = write.lock().await;
    code.require_live_workspace(path.id).await?;
    let snap = state
        .terminals
        .resize(path.id, path.tid, body.cols, body.rows)
        .map_err(map_terminal)?;
    Ok(Json(snapshot_wire(snap)))
}

/// Start one engine's own sign-in command in a terminal the reader can type
/// in, or return the sign-in already running for them.
///
/// The command is the pinned binary's (`claude auth login`), run by its
/// absolute path: the binary Tidebreak drives is not on the reader's `PATH`,
/// so a person who pressed Download has nowhere else to run it. The terminal
/// is outside any workspace and belongs to the caller alone. Credentials land
/// in the engine's own files; Tidebreak observes them the way it always has
/// (decision 34) and never reads them.
pub async fn start_harness_sign_in(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(kind): Path<HarnessKind>,
    Json(body): Json<CreateTerminalBody>,
) -> Result<impl IntoResponse, ServerError> {
    let launch = code.sign_in_launch(kind).await?;
    let owner = code.owner().clone();
    let terminals = state.sign_in_terminals.clone();
    let snap = tokio::task::spawn_blocking(move || {
        terminals.start(&owner, kind, &launch, body.cols, body.rows)
    })
    .await
    .map_err(|error| map_terminal(TerminalError::Io(error.to_string())))?
    .map_err(map_terminal)?;
    Ok((StatusCode::CREATED, Json(sign_in_wire(kind, snap))))
}

pub async fn read_harness_sign_in(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<HarnessSignInPath>,
    Query(query): Query<TerminalReadQuery>,
) -> Result<Json<HarnessSignInRead>, ServerError> {
    let read = state
        .sign_in_terminals
        .read(code.owner(), path.kind, path.tid, query.cursor);
    Ok(Json(HarnessSignInRead {
        id: path.tid,
        kind: path.kind,
        bytes: BASE64.encode(read.data),
        cursor: read.next_cursor,
        overflow: read.overflow,
        truncated: read.truncated,
        ended: read.ended,
    }))
}

pub async fn write_harness_sign_in(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<HarnessSignInPath>,
    Json(body): Json<TerminalWriteBody>,
) -> Result<StatusCode, ServerError> {
    let bytes = decode_write(&body.bytes)?;
    state
        .sign_in_terminals
        .write(code.owner(), path.kind, path.tid, &bytes)
        .map_err(map_terminal)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn resize_harness_sign_in(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<HarnessSignInPath>,
    Json(body): Json<TerminalResizeBody>,
) -> Result<Json<HarnessSignInTerminal>, ServerError> {
    let snap = state
        .sign_in_terminals
        .resize(code.owner(), path.kind, path.tid, body.cols, body.rows)
        .map_err(map_terminal)?;
    Ok(Json(sign_in_wire(path.kind, snap)))
}

/// Stop the sign-in command, whether or not it finished.
pub async fn close_harness_sign_in(
    axum::extract::State(state): axum::extract::State<AppState>,
    code: ScopedCode,
    Path(path): Path<HarnessSignInPath>,
) -> Result<StatusCode, ServerError> {
    let owner = code.owner().clone();
    let terminals = state.sign_in_terminals.clone();
    tokio::task::spawn_blocking(move || terminals.close(&owner, path.kind, path.tid))
        .await
        .map_err(|error| map_terminal(TerminalError::Io(error.to_string())))?
        .map_err(map_terminal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn sign_in_wire(kind: HarnessKind, snap: TerminalSnapshot) -> HarnessSignInTerminal {
    HarnessSignInTerminal {
        id: snap.id,
        kind,
        command: tidebreak_harness::sign_in_command(kind).unwrap_or_default(),
        cols: snap.cols,
        rows: snap.rows,
        ended: snap.ended,
        created_at: snap.created_at,
    }
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
