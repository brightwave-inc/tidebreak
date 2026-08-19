use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::error::ServerError;
use crate::extract::{Json, Path, RawBytes};
use crate::routes::image_attachment::{
    inspect_image_bytes, require_declared_type_matches, PublishedImageAttachment,
};
use crate::routes::SERVED_BYTES_CONTENT_POLICY;
use crate::state::AppState;

use super::require_code;
use super::types::{
    CodeSessionSnapshot, CodeTurnSnapshot, CreateSessionBody, QueuedCodeTurn,
    SequencedCodeEventFrame, SetAttentionBody, SteerBody, SubmitTurnBody,
};
use crate::code::runtime::SubmitTurnOutcome;
use tidebreak_core::{CodeSessionId, WorkspaceId};

pub async fn create_session(
    State(state): State<AppState>,
    Path(workspace_id): Path<WorkspaceId>,
    Json(body): Json<CreateSessionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let session = require_code(&state)?
        .create_session(workspace_id, body.harness, body.permission_mode, body.model)
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
    let runtime = require_code(&state)?;
    let requested = body
        .attachments
        .iter()
        .map(|item| (item.blob_id, item.media_type.clone()))
        .collect::<Vec<_>>();
    let attachments = runtime.resolve_turn_attachments(&requested).await?;
    // From the front of the submit, like chat titling from the front of a
    // turn: a code turn can run for minutes, and the derived name should land
    // while the engine works, not after.
    crate::code::titling::spawn_for_turn(&state, id, message.clone());
    match runtime
        .submit_turn(id, message.clone(), body.model, attachments)
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

pub async fn get_session_debug(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
) -> Result<Json<super::types::CodeSessionDebug>, ServerError> {
    let (session, turns, events) = require_code(&state)?.session_debug(id).await?;
    Ok(Json(super::types::CodeSessionDebug {
        session: CodeSessionSnapshot::from(session),
        turns: turns.into_iter().map(CodeTurnSnapshot::from).collect(),
        events: events
            .into_iter()
            .map(|item| SequencedCodeEventFrame {
                seq: item.seq,
                event: item.event,
                replayed: None,
            })
            .collect(),
    }))
}

pub async fn list_session_turns(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
) -> Result<Json<Vec<CodeTurnSnapshot>>, ServerError> {
    let turns = require_code(&state)?.list_session_turns(id).await?;
    Ok(Json(
        turns.into_iter().map(CodeTurnSnapshot::from).collect(),
    ))
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

pub async fn set_attention(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
    Json(body): Json<SetAttentionBody>,
) -> Result<Json<CodeSessionSnapshot>, ServerError> {
    let session = require_code(&state)?
        .set_attention(id, body.clear, body.note)
        .await?;
    Ok(Json(CodeSessionSnapshot::from(session)))
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

/// `POST /code/sessions/{id}/attachments/images` — publish pixels a later
/// turn can reference. Same sniff-and-store path as chat; the turn row is
/// what pins the blob.
pub async fn publish_session_image(
    State(state): State<AppState>,
    Path(id): Path<CodeSessionId>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    let runtime = require_code(&state)?;
    let _session = runtime.get_session(id).await?;
    let bytes = bytes.to_vec();
    let image = inspect_image_bytes(&bytes)?;
    require_declared_type_matches(&headers, image.media_type)?;
    let _blob_write = state.blob_writes.acquire(image.blob_id).await?;
    state.blobs.put(image.blob_id, bytes).await?;
    Ok((
        StatusCode::CREATED,
        crate::extract::Json(PublishedImageAttachment::from(image)),
    ))
}

/// `GET /code/sessions/{id}/attachments/images/{blob_id}` — pixels for an
/// image a turn on this session already referenced.
pub async fn get_session_image(
    State(state): State<AppState>,
    Path((id, blob_id)): Path<(CodeSessionId, uuid::Uuid)>,
) -> Result<Response, ServerError> {
    let runtime = require_code(&state)?;
    let turns = runtime.list_session_turns(id).await?;
    let attachment = turns
        .iter()
        .flat_map(|turn| turn.attachments.iter())
        .find(|item| item.blob_id == blob_id)
        .cloned()
        .ok_or_else(|| {
            ServerError::not_found(format!(
                "image attachment {blob_id} not found in session {id}"
            ))
        })?;
    let bytes = state.blobs.get(blob_id).await?.ok_or_else(|| {
        ServerError::internal(format!(
            "image attachment {blob_id} is missing from blob storage"
        ))
    })?;
    let actual_len = u64::try_from(bytes.len())
        .map_err(|_| ServerError::internal("image attachment byte length exceeds u64"))?;
    if actual_len != attachment.byte_len {
        return Err(ServerError::internal(format!(
            "image attachment {blob_id} does not match its descriptor"
        )));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, attachment.media_type.as_str())
        .header(header::CONTENT_LENGTH, actual_len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, SERVED_BYTES_CONTENT_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CONTENT_DISPOSITION, "inline")
        .body(Body::from(bytes))
        .map_err(|error| {
            ServerError::internal(format!(
                "failed to build image attachment response: {error}"
            ))
        })
}
