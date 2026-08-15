use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::require_code;
use super::types::{
    CodeSessionSnapshot, CodeTurnSnapshot, CreateSessionBody, QueuedCodeTurn, SteerBody,
    SubmitTurnBody,
};
use crate::code::runtime::SubmitTurnOutcome;
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

pub async fn list_workspace_sessions(
    State(state): State<AppState>,
    Path(workspace_id): Path<WorkspaceId>,
) -> Result<Json<Vec<CodeSessionSnapshot>>, ServerError> {
    let sessions = require_code(&state)?
        .list_workspace_sessions(workspace_id)
        .await?;
    Ok(Json(
        sessions
            .into_iter()
            .map(CodeSessionSnapshot::from)
            .collect(),
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
    match require_code(&state)?
        .submit_turn(id, message.clone())
        .await?
    {
        SubmitTurnOutcome::Ran(turn) => {
            Ok((StatusCode::ACCEPTED, Json(CodeTurnSnapshot::from(*turn))).into_response())
        }
        SubmitTurnOutcome::Queued => Ok((
            StatusCode::ACCEPTED,
            Json(QueuedCodeTurn {
                session_id: id,
                message,
                position: 1,
            }),
        )
            .into_response()),
    }
}

pub async fn steer_session(
    Path(_id): Path<CodeSessionId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    let message = body.message.trim();
    if message.is_empty() {
        return Err(ServerError::bad_request("message must not be empty"));
    }
    Err(ServerError::unprocessable_kind(
        "steering_unavailable",
        "explicit mid-turn steering is not yet available; the message was not queued",
    ))
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
