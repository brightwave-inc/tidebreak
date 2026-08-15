use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::require_code;
use super::types::{CodeSessionSnapshot, CodeTurnSnapshot, CreateSessionBody, SubmitTurnBody};
use tidebreak_core::{CodeSessionId, WorkspaceId};

pub async fn create_session(
    State(state): State<AppState>,
    Path(workspace_id): Path<WorkspaceId>,
    Json(body): Json<CreateSessionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let session = require_code(&state)?
        .create_session(workspace_id, body.harness, body.permission_mode)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeSessionSnapshot::from(session)),
    ))
}

pub async fn submit_turn(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
    Json(body): Json<SubmitTurnBody>,
) -> Result<impl IntoResponse, ServerError> {
    let message = body.message.trim().to_owned();
    if message.is_empty() {
        return Err(ServerError::bad_request("message must not be empty"));
    }
    let turn = require_code(&state)?.submit_turn(id, message).await?;
    Ok((StatusCode::ACCEPTED, Json(CodeTurnSnapshot::from(turn))))
}

pub async fn interrupt_session(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
) -> Result<StatusCode, ServerError> {
    require_code(&state)?.interrupt(id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn reap_session(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
) -> Result<(StatusCode, Json<CodeSessionSnapshot>), ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::internal("code mode is not configured on this server"))?;
    let session = runtime.reap(id).await?;
    Ok((StatusCode::OK, Json(CodeSessionSnapshot::from(session))))
}
