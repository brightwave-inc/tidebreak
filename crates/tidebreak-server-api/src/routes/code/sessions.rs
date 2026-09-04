use crate::code::ScopedCode;
use crate::error::ServerError;
use crate::extract::{Json, Path, RawBytes};
use crate::routes::image_attachment::{
    inspect_image_bytes, require_declared_type_matches, PublishedImageAttachment,
};
use crate::routes::providers_models::{
    refuse_permission_mode_over_ceiling, refuse_permission_mode_over_ceiling_value,
};
use crate::routes::SERVED_BYTES_CONTENT_POLICY;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{FromRequestParts, State};
use axum::http::{header, request::Parts, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use super::types::{
    CodeForkTranscript, CreateInternalSessionBody, CreateRemoteSessionBody, CreateSessionBody,
    QueuePausedBody, QueuedTurn, QueuedTurnUpdate, QueuedTurnsSnapshot, SequencedEventFrame,
    SessionExternalOrigin, SessionSnapshot, SetAttentionBody, SetFastModeBody,
    SetPermissionModeBody, SetReasoningEffortBody, SteerBody, SubmitTurnBody, TurnSnapshot,
};
use crate::code::runtime::{NewSessionSettings, SubmitTurnOutcome};
use tidebreak_core::{PermissionMode, SessionId, TurnSteer, WorkspaceId};

/// The managed ceiling remote session creation needs, resolved before the
/// handler receives request fields.
pub(crate) struct RemoteSessionPolicy {
    permission_mode_ceiling: Option<PermissionMode>,
}

impl FromRequestParts<AppState> for RemoteSessionPolicy {
    type Rejection = ServerError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let permission_mode_ceiling = state.managed_policy()?.permission_mode_ceiling;
        refuse_permission_mode_over_ceiling_value(
            permission_mode_ceiling,
            Some(PermissionMode::Allow),
        )?;
        Ok(Self {
            permission_mode_ceiling,
        })
    }
}

pub async fn create_session(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(workspace_id): Path<WorkspaceId>,
    Json(body): Json<CreateSessionBody>,
) -> Result<impl IntoResponse, ServerError> {
    // Ceiling vs. engine intersection is decided after probe, inside create:
    // an empty intersection must name the conflict instead of looking like a
    // single over-ceiling pick.
    let permission_mode_ceiling = state.managed_policy()?.permission_mode_ceiling;
    let session = code
        .create_session(
            workspace_id,
            body.harness,
            NewSessionSettings {
                permission_mode: body.permission_mode,
                model: body.model,
                reasoning_effort: body.reasoning_effort,
                fast_mode: body.fast_mode,
                permission_mode_ceiling,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(SessionSnapshot::from(session))))
}

/// `POST /sessions` — create a conversation with no workspace. The
/// in-process engine hosts it (decision 0048 step 5).
pub async fn create_internal_session(
    State(state): State<AppState>,
    code: ScopedCode,
    Json(body): Json<CreateInternalSessionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let permission_mode_ceiling = state.managed_policy()?.permission_mode_ceiling;
    let session = code
        .create_internal_session(NewSessionSettings {
            permission_mode: body.permission_mode,
            model: body.model,
            reasoning_effort: body.reasoning_effort,
            fast_mode: false,
            permission_mode_ceiling,
        })
        .await?;
    Ok((StatusCode::CREATED, Json(SessionSnapshot::from(session))))
}

/// `POST /code/remote/workspaces/{id}/sessions` — create a session whose
/// engine runs inside the configured sandbox runtime.
pub async fn create_remote_session(
    code: ScopedCode,
    RemoteSessionPolicy {
        permission_mode_ceiling,
    }: RemoteSessionPolicy,
    Path(workspace_id): Path<WorkspaceId>,
    Json(body): Json<CreateRemoteSessionBody>,
) -> Result<impl IntoResponse, ServerError> {
    let session = code
        .create_remote_session(
            workspace_id,
            body.harness,
            NewSessionSettings {
                permission_mode: PermissionMode::Allow,
                model: body.model,
                reasoning_effort: body.reasoning_effort,
                fast_mode: false,
                permission_mode_ceiling,
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(SessionSnapshot::from(session))))
}

/// One session as a snapshot with its external origin attached.
///
/// Every handler that hands the desktop a single session goes through
/// here: the desktop writes the answer over its listed row, so a snapshot
/// that hard-coded no origin would erase the provenance banner on the
/// next reap, mode change, or attention edit.
async fn snapshot_with_origin(
    code: &ScopedCode,
    session: tidebreak_core::Session,
) -> Result<SessionSnapshot, ServerError> {
    let origin = code
        .external_bindings_for_sessions(&[session.id])
        .await?
        .into_iter()
        .next()
        .map(|binding| SessionExternalOrigin {
            channel_kind: binding.channel_kind,
            external_key: binding.external_key,
        });
    let mut snapshot = SessionSnapshot::from(session);
    snapshot.external_origin = origin;
    Ok(snapshot)
}

pub async fn list_workspace_sessions(
    code: ScopedCode,
    Path(workspace_id): Path<WorkspaceId>,
) -> Result<Json<Vec<SessionSnapshot>>, ServerError> {
    let sessions = code.list_workspace_sessions(workspace_id).await?;
    let ids: Vec<SessionId> = sessions.iter().map(|session| session.id).collect();
    let origins: std::collections::HashMap<SessionId, SessionExternalOrigin> = code
        .external_bindings_for_sessions(&ids)
        .await?
        .into_iter()
        .map(|binding| {
            (
                binding.session_id,
                SessionExternalOrigin {
                    channel_kind: binding.channel_kind,
                    external_key: binding.external_key,
                },
            )
        })
        .collect();
    Ok(Json(
        sessions
            .into_iter()
            .map(|session| {
                let origin = origins.get(&session.id).cloned();
                let mut snapshot = SessionSnapshot::from(session);
                snapshot.external_origin = origin;
                snapshot
            })
            .collect(),
    ))
}

/// `GET /sessions` — the owner's conversations that bind no
/// workspace, newest first.
pub async fn list_internal_sessions(
    code: ScopedCode,
) -> Result<Json<Vec<SessionSnapshot>>, ServerError> {
    let sessions = code.list_internal_sessions().await?;
    Ok(Json(
        sessions.into_iter().map(SessionSnapshot::from).collect(),
    ))
}

/// `GET /sessions/{id}` — one session by id, whatever it binds.
pub async fn get_session(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<Json<SessionSnapshot>, ServerError> {
    let session = code.get_session(id).await?;
    let origin = code
        .external_bindings_for_sessions(&[id])
        .await?
        .into_iter()
        .next()
        .map(|binding| SessionExternalOrigin {
            channel_kind: binding.channel_kind,
            external_key: binding.external_key,
        });
    let mut snapshot = SessionSnapshot::from(session);
    snapshot.external_origin = origin;
    Ok(Json(snapshot))
}

pub async fn submit_turn(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SubmitTurnBody>,
) -> Result<impl IntoResponse, ServerError> {
    let message = body.message.trim().to_owned();
    if message.is_empty() {
        return Err(ServerError::bad_request("message must not be empty"));
    }
    let requested = body
        .attachments
        .iter()
        .map(|item| (item.blob_id, item.media_type.clone()))
        .collect::<Vec<_>>();
    let attachments = code.resolve_turn_attachments(id, &requested).await?;
    // From the front of the submit, like chat titling from the front of a
    // turn: a code turn can run for minutes, and the derived name should land
    // while the engine works, not after.
    crate::code::titling::spawn_for_turn(&state, code.owner(), id, message.clone());
    let actor = tidebreak_core::TurnActor {
        principal: Some(code.owner().to_string()),
        display: None,
        channel_kind: None,
        external_identity: None,
    };
    match code
        .submit_turn(
            id,
            message.clone(),
            body.model,
            body.reasoning_effort,
            attachments,
        )
        .await?
    {
        SubmitTurnOutcome::Ran(mut turn) => {
            code.record_submission_actor(id, turn.id, false, &actor)
                .await?;
            turn.actor = Some(actor);
            Ok((StatusCode::ACCEPTED, Json(TurnSnapshot::from(*turn))).into_response())
        }
        SubmitTurnOutcome::Queued(mut row) => {
            code.record_submission_actor(id, row.id, true, &actor)
                .await?;
            row.actor = Some(actor);
            Ok((StatusCode::ACCEPTED, Json(QueuedTurn::from(*row))).into_response())
        }
        // Trigger submits never come through this route; the enum is shared
        // with the trigger sweep, which is the only caller that can see this.
        SubmitTurnOutcome::AlreadyDelivered => Err(ServerError::internal(
            "a user turn reported a trigger delivery outcome",
        )),
    }
}

/// `GET /sessions/{id}/queued` — the session's queued messages, FIFO,
/// plus whether promotion is paused.
pub async fn list_queued_turns(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<Json<QueuedTurnsSnapshot>, ServerError> {
    let (queued, paused) = code.list_queued_turns(id).await?;
    Ok(Json(QueuedTurnsSnapshot {
        queued: queued.into_iter().map(QueuedTurn::from).collect(),
        paused,
    }))
}

/// `PATCH /sessions/{id}/queued/{queued_id}` — edit or reorder one
/// queued message.
pub async fn patch_queued_turn(
    code: ScopedCode,
    Path((id, queued_id)): Path<(SessionId, tidebreak_core::TurnId)>,
    Json(body): Json<QueuedTurnUpdate>,
) -> Result<Json<QueuedTurn>, ServerError> {
    if let Some(message) = body.message.as_deref() {
        if message.trim().is_empty() || message.contains('\0') {
            return Err(ServerError::bad_request(
                "message must be non-empty and contain no NUL characters",
            ));
        }
    }
    let updated = code
        .update_queued_turn(id, queued_id, body.message.as_deref(), body.position)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("queued turn {queued_id} not found")))?;
    Ok(Json(QueuedTurn::from(updated)))
}

/// `DELETE /sessions/{id}/queued/{queued_id}` — retract one queued
/// message.
pub async fn delete_queued_turn(
    code: ScopedCode,
    Path((id, queued_id)): Path<(SessionId, tidebreak_core::TurnId)>,
) -> Result<StatusCode, ServerError> {
    if code.delete_queued_turn(id, queued_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ServerError::not_found(format!(
            "queued turn {queued_id} not found"
        )))
    }
}

/// `PUT /sessions/{id}/queue-paused` — pause or release promotion for
/// this session; queued rows stay put while paused.
pub async fn put_queue_paused(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<QueuePausedBody>,
) -> Result<StatusCode, ServerError> {
    code.set_queue_paused(id, body.paused).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /sessions/{id}/queued/send-now` — clear this session's pause
/// and wake the worker so the head row starts.
///
/// The tray composes the full gesture client-side exactly as chat's does:
/// pause, move the row first, stop the live turn, then this.
pub async fn post_queue_send_now(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<StatusCode, ServerError> {
    code.send_queued_now(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /sessions/{id}/fork` — write this session's handoff into
/// private storage and report where it landed.
///
/// Creating the child session is a separate call: forking is a file, and the
/// reader picks the engine and edits the framing before anything is sent.
/// The body is optional so a client that predates fork points still forks at
/// the newest turn.
pub async fn fork_session(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<CodeForkTranscript>), ServerError> {
    let at_turn = if body.is_empty() {
        None
    } else {
        serde_json::from_slice::<super::types::CodeForkBody>(&body)
            .map_err(|err| ServerError::bad_request(format!("invalid fork body: {err}")))?
            .at_turn
    };
    let written = code.fork_transcript(id, at_turn).await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeForkTranscript {
            path: written.path,
            dir: written.dir,
            byte_len: written.byte_len,
            turns: written.turns,
            total_turns: written.total_turns,
            at_turn_ordinal: written.at_turn_ordinal,
            truncated: written.truncated,
        }),
    ))
}

pub async fn get_session_debug(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<Json<super::types::SessionDebug>, ServerError> {
    let (session, turns, events) = code.session_debug(id).await?;
    Ok(Json(super::types::SessionDebug {
        session: SessionSnapshot::from(session),
        turns: turns.into_iter().map(TurnSnapshot::from).collect(),
        events: events
            .into_iter()
            .map(|item| SequencedEventFrame {
                seq: item.seq,
                event: item.event,
                replayed: None,
                transient: None,
                replacement: None,
                truncated: None,
            })
            .collect(),
    }))
}

pub async fn list_session_turns(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<Json<Vec<TurnSnapshot>>, ServerError> {
    let turns = code.list_session_turns(id).await?;
    Ok(Json(turns.into_iter().map(TurnSnapshot::from).collect()))
}

pub async fn steer_session(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    let guidance = body.guidance.trim().to_owned();
    if guidance.is_empty()
        || guidance.contains('\0')
        || guidance.chars().count() > TurnSteer::MAX_CONTENT_LEN
    {
        return Err(ServerError::bad_request(
            "guidance must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    code.steer(id, body.expected_turn_id, guidance).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn interrupt_session(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<StatusCode, ServerError> {
    code.interrupt(id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub async fn set_attention(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SetAttentionBody>,
) -> Result<Json<SessionSnapshot>, ServerError> {
    let session = code.set_attention(id, body.clear, body.note).await?;
    Ok(Json(snapshot_with_origin(&code, session).await?))
}

pub async fn reap_session(
    code: ScopedCode,
    Path(id): Path<SessionId>,
) -> Result<(StatusCode, Json<SessionSnapshot>), ServerError> {
    let session = code.reap(id).await?;
    Ok((
        StatusCode::OK,
        Json(snapshot_with_origin(&code, session).await?),
    ))
}

/// `POST /sessions/{id}/mode` — change a session's permission mode.
///
/// The engine is relaunched against the new posture, so this is refused
/// while a turn is running and while the session has ended. A mode the
/// engine cannot honor is refused here rather than approximated, and so is
/// a mode above the ceiling a managed profile asserts.
pub async fn set_session_permission_mode(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SetPermissionModeBody>,
) -> Result<(StatusCode, Json<SessionSnapshot>), ServerError> {
    refuse_permission_mode_over_ceiling(&state, Some(body.permission_mode)).await?;
    let session = code.set_permission_mode(id, body.permission_mode).await?;
    Ok((
        StatusCode::OK,
        Json(snapshot_with_origin(&code, session).await?),
    ))
}

/// `POST /sessions/{id}/effort` — change a session's reasoning effort.
///
/// No relaunch: every adapter reads the level off the turn, so the next turn
/// carries it. Refused while a turn is running and after the session ends, on
/// the same rule the mode route applies.
pub async fn set_session_reasoning_effort(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SetReasoningEffortBody>,
) -> Result<(StatusCode, Json<SessionSnapshot>), ServerError> {
    let session = code.set_reasoning_effort(id, body.reasoning_effort).await?;
    Ok((
        StatusCode::OK,
        Json(snapshot_with_origin(&code, session).await?),
    ))
}

/// `POST /sessions/{id}/fast-mode` — arm or disarm the engine's fast mode.
///
/// Fast mode buys output speed at a premium rate, so this is a spend switch
/// rather than a quality one. Refused while a turn is running and after the
/// session ends, on the rule the mode and effort routes share, and refused
/// when the session's model does not serve the tier.
pub async fn set_session_fast_mode(
    code: ScopedCode,
    Path(id): Path<SessionId>,
    Json(body): Json<SetFastModeBody>,
) -> Result<(StatusCode, Json<SessionSnapshot>), ServerError> {
    let session = code.set_fast_mode(id, body.fast_mode).await?;
    Ok((
        StatusCode::OK,
        Json(snapshot_with_origin(&code, session).await?),
    ))
}

/// `POST /sessions/{id}/attachments/images` — publish pixels a later
/// turn can reference. Same sniff-and-store path as chat; the turn row is
/// what pins the blob.
pub async fn publish_session_image(
    State(state): State<AppState>,
    code: ScopedCode,
    Path(id): Path<SessionId>,
    headers: HeaderMap,
    RawBytes(bytes): RawBytes,
) -> Result<impl IntoResponse, ServerError> {
    let _session = code.get_session(id).await?;
    let bytes = bytes.to_vec();
    let image = inspect_image_bytes(&bytes)?;
    require_declared_type_matches(&headers, image.media_type)?;
    let _blob_write = state.blob_writes.acquire(image.blob_id).await?;
    state.blobs.put(image.blob_id, bytes).await?;
    // Publication is the authority a later turn attachment is checked against.
    // Storing the bytes is not enough: the blob store is content-addressed and
    // owner-blind, so without this row any known id could be bound into any
    // session and read back through this session's own image route.
    if !code.publish_session_image(id, &image).await? {
        state
            .store
            .ensure_orphan_blob_retirement(image.blob_id)
            .await?;
        state.blob_retirement_wake.notify_one();
        return Err(ServerError::conflict_kind(
            "session_ended",
            format!("session {id} ended before the image could be published"),
        ));
    }
    Ok((
        StatusCode::CREATED,
        crate::extract::Json(PublishedImageAttachment::from(image)),
    ))
}

/// `GET /sessions/{id}/attachments/images/{blob_id}` — pixels for an
/// image a turn on this session already referenced.
pub async fn get_session_image(
    State(state): State<AppState>,
    code: ScopedCode,
    Path((id, blob_id)): Path<(SessionId, uuid::Uuid)>,
) -> Result<Response, ServerError> {
    let turns = code.list_session_turns(id).await?;
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
