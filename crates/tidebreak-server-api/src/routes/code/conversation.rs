//! Grant-authenticated conversation-tool request transport.
//!
//! The adapter lists durable requests its live grant created and posts an
//! untrusted JSON result back. The wire carries no result/owner/internal
//! fields on listing; completion stores the bounded result for the parent
//! engine to hand to the model. Slack payload provenance is trusted at the
//! grant boundary; every result is content, never authority.

use axum::extract::State;
use axum::http::StatusCode;

use tidebreak_core::db::code::complete_conversation_request;
use tidebreak_core::{CodeBindingId, ConversationRequest, SessionId};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

use super::external::{ExternalGrantAuth, require_bound};

/// `GET /external/code/sessions/{id}/conversation-requests` — list the
/// unexpired pending jobs for the authenticated grant's bound session.
///
/// The response is deliberately lean: only id, binding, operation, and the
/// exact arguments. No result, owner, session, grant, or internal fields
/// leave this surface.
pub async fn external_conversation_requests(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path(id): Path<SessionId>,
) -> Result<Json<ConversationRequestsResponse>, ServerError> {
    let runtime = require_bound(&state, &grant, id).await?;
    let pending = tidebreak_core::db::code::list_pending_conversation_requests(
        &runtime.db,
        &grant.owner,
        id,
        grant.id,
    )
    .await?;
    Ok(Json(ConversationRequestsResponse {
        requests: pending
            .into_iter()
            .map(|request| ConversationRequestWire::from(request))
            .collect(),
    }))
}

/// The wire projection of one pending job.
#[derive(serde::Serialize)]
pub(crate) struct ConversationRequestWire {
    id: uuid::Uuid,
    binding_id: CodeBindingId,
    operation: String,
    arguments: serde_json::Value,
}

impl From<ConversationRequest> for ConversationRequestWire {
    fn from(request: ConversationRequest) -> Self {
        Self {
            id: request.id,
            binding_id: request.binding_id,
            operation: request.operation,
            arguments: request.arguments,
        }
    }
}

#[derive(serde::Serialize)]
pub(crate) struct ConversationRequestsResponse {
    pub requests: Vec<ConversationRequestWire>,
}

#[derive(serde::Deserialize)]
pub struct ConversationRequestResultBody {
    pub result: serde_json::Value,
}

/// `POST /external/code/sessions/{id}/conversation-requests/{request_id}`
/// with `{ "result": ... }`.
///
/// A duplicate equal result is idempotent; a different result on the
/// completed row is a conflict. The result must be a JSON object: the wire
/// contract has three envelope shapes — messages/export, error, attachment.
pub async fn external_conversation_request_result(
    State(state): State<AppState>,
    ExternalGrantAuth(grant): ExternalGrantAuth,
    Path((id, request_id)): Path<(SessionId, uuid::Uuid)>,
    Json(body): Json<ConversationRequestResultBody>,
) -> Result<StatusCode, ServerError> {
    if !body.result.is_object() {
        return Err(ServerError::bad_request_kind(
            "conversation_request_result_invalid",
            "a conversation request result must be a JSON object",
        ));
    }
    let runtime = require_bound(&state, &grant, id).await?;
    let completed = complete_conversation_request(
        &runtime.db,
        &grant.owner,
        id,
        grant.id,
        request_id,
        &body.result,
    )
    .await
    .map_err(map_conversation_request_error)?;
    if completed.is_none() {
        return Err(ServerError::not_found("conversation request not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Keep adapter-facing failures typed and non-revealing. Invalid target,
/// size, format, or scope answers bad request; a completed row with a
/// different result answers conflict; anything else stays internal.
fn map_conversation_request_error(error: tidebreak_core::AgentError) -> ServerError {
    match error {
        tidebreak_core::AgentError::InvalidTarget(message) => {
            ServerError::bad_request_kind("conversation_request_invalid", message)
        }
        tidebreak_core::AgentError::ConversationRequestConflict(message) => {
            ServerError::conflict_kind("conversation_request_result_conflict", message)
        }
        _ => ServerError::from(error),
    }
}
