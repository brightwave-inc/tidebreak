//! The route surface a channel adapter speaks (docs/slack-sessions.md,
//! stage 2): external get-or-create, messages, session events over
//! WebSocket, interrupt, and reap — nothing else. No settings, no
//! repository administration.
//!
//! Every route authenticates by adapter token, and every session route
//! then checks that the token's grant tags the session through a binding.
//! A session another grant holds answers "not found", the same shape as a
//! session that does not exist, so the surface leaks nothing about other
//! grants' work.

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::Response;

use tidebreak_core::{
    CodeExternalGrant, CodeSessionId, ExternalSessionResolution, GrantRotation, HarnessKind,
    PermissionMode, RepoId,
};

use crate::code::runtime::{ExternalMessageOutcome, NewSessionSettings};
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

use super::types::{CodeSessionSnapshot, QueuedCodeTurn, SessionEventsQuery};

/// The authenticated grant behind an adapter request.
///
/// Fails closed: no code runtime, no bearer token, or a token matching no
/// live grant all answer 401 before any handler runs. A revoked grant's
/// token stops matching, so its next call dies here — the shape the grant
/// slice promises.
pub struct ExternalGrantAuth(pub CodeExternalGrant);

impl FromRequestParts<AppState> for ExternalGrantAuth {
    type Rejection = ServerError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(runtime) = state.code.clone() else {
            return Err(ServerError::unauthorized(
                "adapter access is not configured",
            ));
        };
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| ServerError::unauthorized("an adapter token is required"))?;
        let grant = runtime
            .authenticate_adapter_token(token)
            .await?
            .ok_or_else(|| ServerError::unauthorized("the adapter token matches no live grant"))?;
        Ok(Self(grant))
    }
}

/// Refuse a session the grant does not tag, with the not-found shape.
async fn require_bound(
    state: &AppState,
    grant: &CodeExternalGrant,
    session: CodeSessionId,
) -> Result<std::sync::Arc<crate::code::runtime::CodeRuntime>, ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let bound = tidebreak_core::db::code::session_bound_to_grant(
        &runtime.db,
        &grant.owner,
        session,
        grant.id,
    )
    .await?;
    if !bound {
        return Err(ServerError::not_found("code session not found"));
    }
    Ok(runtime)
}

#[derive(serde::Deserialize)]
pub struct ExternalSessionBody {
    /// The channel's durable conversation identity, opaque here.
    pub external_key: String,
    /// The repository the sandbox clones. Required in v1.
    pub repo_id: RepoId,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub harness: Option<HarnessKind>,
}

#[derive(serde::Serialize)]
pub struct ExternalSessionResponse {
    /// `created`, `existing`, or `ended`.
    pub status: &'static str,
    pub session_id: CodeSessionId,
}

/// `POST /external/code/sessions` — idempotent get-or-create for one
/// conversation. An ended session answers `ended` rather than
/// resurrecting; a conversation bound under another grant answers "not
/// found" like any other session outside this grant's scope.
pub async fn external_get_or_create(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Json(body): Json<ExternalSessionBody>,
) -> Result<(StatusCode, Json<ExternalSessionResponse>), ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let resolution = runtime
        .external_get_or_create(
            &grant.owner,
            grant.id,
            &grant.channel_kind,
            &body.external_key,
            body.repo_id,
            body.title,
            body.harness.unwrap_or(HarnessKind::ClaudeCode),
            NewSessionSettings {
                permission_mode: PermissionMode::Allow,
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                permission_mode_ceiling: None,
            },
        )
        .await?;
    let (status, response) = match resolution {
        ExternalSessionResolution::Created(binding) => (
            StatusCode::CREATED,
            ExternalSessionResponse {
                status: "created",
                session_id: binding.session_id,
            },
        ),
        ExternalSessionResolution::Existing(binding) => (
            StatusCode::OK,
            ExternalSessionResponse {
                status: "existing",
                session_id: binding.session_id,
            },
        ),
        ExternalSessionResolution::Ended { session_id } => (
            StatusCode::OK,
            ExternalSessionResponse {
                status: "ended",
                session_id,
            },
        ),
        ExternalSessionResolution::GrantMismatch => {
            return Err(ServerError::not_found("code session not found"));
        }
    };
    Ok((status, Json(response)))
}

#[derive(serde::Deserialize)]
pub struct ExternalMessageBody {
    pub text: String,
    /// The channel's delivery id; replays of it answer from the first row.
    pub event_id: String,
    /// The channel's ordering token; still-queued messages apply in its
    /// order.
    pub channel_ts: String,
}

#[derive(serde::Serialize)]
pub struct ExternalMessageResponse {
    /// `new_turn`, `queued`, or `dropped`.
    pub outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<tidebreak_core::CodeTurnId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<QueuedCodeTurn>,
}

/// `POST /external/code/sessions/{id}/messages` — deliver one message.
/// Idempotent on `event_id`; queue-default when the session is busy.
pub async fn external_messages(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<CodeSessionId>,
    Json(body): Json<ExternalMessageBody>,
) -> Result<Json<ExternalMessageResponse>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let outcome = runtime
        .external_submit_message(
            &grant.owner,
            grant.id,
            id,
            body.text,
            &body.event_id,
            &body.channel_ts,
        )
        .await?;
    let response = match outcome {
        ExternalMessageOutcome::NewTurn(turn) => ExternalMessageResponse {
            outcome: "new_turn",
            turn_id: Some(turn.id),
            queued: None,
        },
        ExternalMessageOutcome::Queued(row) => ExternalMessageResponse {
            outcome: "queued",
            turn_id: Some(row.id),
            queued: Some(QueuedCodeTurn::from(*row)),
        },
        ExternalMessageOutcome::Dropped => ExternalMessageResponse {
            outcome: "dropped",
            turn_id: None,
            queued: None,
        },
    };
    Ok(Json(response))
}

/// `WS /external/code/sessions/{id}/events?after=` — the desktop event
/// stream, scoped by grant, prefixed with a session snapshot (lifecycle
/// and attention), and severed the moment the grant is revoked.
pub async fn external_events(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<CodeSessionId>,
    Query(query): Query<SessionEventsQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let session = runtime.get_session(&grant.owner, id).await?;
    let owner = grant.owner.clone();
    let grant_id = grant.id;
    let revocations = runtime.grant_revocations();
    Ok(upgrade.on_upgrade(move |mut socket| async move {
        // The renderer needs the session's standing before the journal:
        // lifecycle and the attention snapshot arrive first, as their own
        // frame shape.
        let snapshot = serde_json::json!({
            "snapshot": CodeSessionSnapshot::from(session),
        });
        if let Ok(json) = serde_json::to_string(&snapshot) {
            if socket
                .send(axum::extract::ws::Message::Text(json.into()))
                .await
                .is_err()
            {
                return;
            }
        }
        let mut severed = revocations.subscribe();
        let stream =
            super::session_events::stream_events(socket, state, owner, id, query.after, None);
        tokio::pin!(stream);
        loop {
            tokio::select! {
                () = &mut stream => break,
                hit = severed.recv() => match hit {
                    // Dropping the stream future drops the socket, which
                    // closes the connection: revocation severs live.
                    Ok(revoked) if revoked == grant_id => break,
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        stream.await;
                        break;
                    }
                },
            }
        }
    }))
}

/// `POST /external/code/sessions/{id}/interrupt` — stop the active turn,
/// exactly as the desktop button does. The session stays live.
pub async fn external_interrupt(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<CodeSessionId>,
) -> Result<StatusCode, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let _ = runtime.get_session(&grant.owner, id).await?;
    runtime.interrupt(id).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /external/code/sessions/{id}/reap` — clear a fenced session,
/// exactly as the desktop button does.
pub async fn external_reap(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<CodeSessionId>,
) -> Result<Json<CodeSessionSnapshot>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let session = runtime.reap(&grant.owner, id).await?;
    Ok(Json(CodeSessionSnapshot::from(session)))
}

#[derive(serde::Deserialize)]
pub struct ExternalRotateBody {
    pub refresh: String,
}

#[derive(serde::Serialize)]
pub struct ExternalRotateResponse {
    pub token: String,
    pub refresh: String,
}

/// `POST /external/grants/rotate` — trade the refresh token for a new
/// pair. A replayed rotated token revokes the grant before this answers,
/// and the answer says so.
pub async fn external_rotate(
    State(state): State<AppState>,
    Json(body): Json<ExternalRotateBody>,
) -> Result<Json<ExternalRotateResponse>, ServerError> {
    let runtime = state
        .code
        .clone()
        .ok_or_else(|| ServerError::unauthorized("adapter access is not configured"))?;
    let (outcome, pair) = runtime.rotate_adapter_token(&body.refresh).await?;
    match outcome {
        GrantRotation::Rotated(_) => {
            let pair = pair.ok_or_else(|| ServerError::internal("rotation issued no pair"))?;
            Ok(Json(ExternalRotateResponse {
                token: pair.token,
                refresh: pair.refresh,
            }))
        }
        GrantRotation::ReuseDetected(_) => Err(ServerError::unauthorized(
            "this refresh token was already rotated; the grant is revoked",
        )),
        GrantRotation::Unknown => Err(ServerError::unauthorized(
            "the refresh token matches no live grant",
        )),
    }
}
