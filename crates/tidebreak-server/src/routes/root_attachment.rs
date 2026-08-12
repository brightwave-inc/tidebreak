//! Native-only HTTP boundary for durable root-attachment reconciliation.
//!
//! These routes only begin, recover, and finish product-side state. Broker
//! dispatch remains the trusted desktop reconciler's responsibility.

use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tidebreak_core::{
    BeginRootAttachmentChange, BeginRootAttachmentChangeOutcome, ChatId,
    FinishRootAttachmentChangeOutcome, HostRootId, RootAttachmentChange,
    RootAttachmentChangeAction, RootAttachmentChangeFailure, RootAttachmentChangeId,
    RootAttachmentChangePhase, RootAttachmentChangeTerminal, RootAttachmentOrigin,
    RootAttachmentSubjectKind, MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};
use uuid::Uuid;

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::principal::ClientExecutor;
use crate::state::AppState;

const DEFAULT_PENDING_LIMIT: u64 = 64;

/// Caller intent for one exact root-attachment change.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginRootAttachmentChangeBody {
    pub root_id: HostRootId,
    pub action: RootAttachmentChangeAction,
    pub expected_attachment_revision: i64,
    pub created_at: DateTime<Utc>,
}

/// Terminal broker observation for one exact root-attachment change.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinishRootAttachmentChangeBody {
    pub terminal: RootAttachmentChangeTerminal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingRootAttachmentChangesQuery {
    pub limit: Option<u64>,
}

/// Whether a begin call committed work or recovered an exact retry.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeginRootAttachmentChangeDisposition {
    Begun,
    Existing,
}

/// Whether a finish call committed work or recovered an exact retry.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishRootAttachmentChangeDisposition {
    Finished,
    Existing,
}

/// Native recovery view. The private executor identity is intentionally absent.
#[derive(Debug, Serialize)]
pub struct RootAttachmentChangeView {
    pub id: RootAttachmentChangeId,
    pub chat_id: ChatId,
    pub root_id: HostRootId,
    pub action: RootAttachmentChangeAction,
    pub subject_kind: RootAttachmentSubjectKind,
    pub subject_id: Uuid,
    pub origin: Option<RootAttachmentOrigin>,
    pub projection_position: Option<u32>,
    pub projection_existed_before: bool,
    pub expected_revision: i64,
    pub before_revision: i64,
    pub intent_revision: i64,
    pub phase: RootAttachmentChangePhase,
    pub result_revision: Option<i64>,
    pub projection_changed: Option<bool>,
    pub broker_changed: Option<bool>,
    pub broker_currently_attached: Option<bool>,
    pub failure: Option<RootAttachmentChangeFailure>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl From<RootAttachmentChange> for RootAttachmentChangeView {
    fn from(change: RootAttachmentChange) -> Self {
        Self {
            id: change.id,
            chat_id: change.chat_id,
            root_id: change.root_id,
            action: change.action,
            subject_kind: change.subject_kind,
            subject_id: change.subject_id,
            origin: change.origin,
            projection_position: change.projection_position,
            projection_existed_before: change.projection_existed_before,
            expected_revision: change.expected_revision,
            before_revision: change.before_revision,
            intent_revision: change.intent_revision,
            phase: change.phase,
            result_revision: change.result_revision,
            projection_changed: change.projection_changed,
            broker_changed: change.broker_changed,
            broker_currently_attached: change.broker_currently_attached,
            failure: change.failure,
            created_at: change.created_at,
            finished_at: change.finished_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct BegunRootAttachmentChange {
    pub disposition: BeginRootAttachmentChangeDisposition,
    pub change: RootAttachmentChangeView,
}

#[derive(Debug, Serialize)]
pub struct FinishedRootAttachmentChange {
    pub disposition: FinishRootAttachmentChangeDisposition,
    pub change: RootAttachmentChangeView,
}

#[derive(Debug, Serialize)]
pub struct PendingRootAttachmentChanges {
    pub changes: Vec<RootAttachmentChangeView>,
}

/// Begin or recover one exact product-side attachment intent.
pub async fn begin_root_attachment_change(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Path((chat_id, change_id)): Path<(ChatId, RootAttachmentChangeId)>,
    Json(body): Json<BeginRootAttachmentChangeBody>,
) -> Result<Json<BegunRootAttachmentChange>, ServerError> {
    if chat_id.as_uuid().is_nil() {
        return Err(ServerError::bad_request("chat id must not be nil"));
    }
    let request = BeginRootAttachmentChange {
        id: change_id,
        chat_id,
        executor_id: state.client_executor_id,
        root_id: body.root_id,
        action: body.action,
        expected_attachment_revision: body.expected_attachment_revision,
        created_at: body.created_at,
    };
    request.validate().map_err(ServerError::bad_request)?;

    let (disposition, change) = match state.store.begin_root_attachment_change(&request).await? {
        BeginRootAttachmentChangeOutcome::Begun(change) => {
            (BeginRootAttachmentChangeDisposition::Begun, change)
        }
        BeginRootAttachmentChangeOutcome::Existing(change) => {
            (BeginRootAttachmentChangeDisposition::Existing, change)
        }
        BeginRootAttachmentChangeOutcome::ChatNotFound => {
            return Err(ServerError::not_found("conversation not found"));
        }
        BeginRootAttachmentChangeOutcome::IdentityConflict => {
            return Err(ServerError::conflict_kind(
                "root_attachment_identity_conflict",
                "root attachment change identity is already in use",
            ));
        }
        BeginRootAttachmentChangeOutcome::RevisionConflict { .. } => {
            return Err(ServerError::conflict_kind(
                "root_attachment_revision_conflict",
                "root attachment projection changed before this request",
            ));
        }
        BeginRootAttachmentChangeOutcome::CapacityExceeded => {
            return Err(ServerError::conflict_kind(
                "root_attachment_capacity_exceeded",
                "root attachment projection is at capacity",
            ));
        }
        BeginRootAttachmentChangeOutcome::RevisionExhausted => {
            return Err(ServerError::conflict_kind(
                "root_attachment_revision_exhausted",
                "root attachment revision cannot advance",
            ));
        }
        BeginRootAttachmentChangeOutcome::ChatBusy => {
            return Err(ServerError::conflict_kind(
                "root_attachment_chat_busy",
                "a root attachment change is already in progress",
            ));
        }
    };
    Ok(Json(BegunRootAttachmentChange {
        disposition,
        change: change.into(),
    }))
}

/// List awaiting work owned by this app-private native executor.
pub async fn list_pending_root_attachment_changes(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Query(query): Query<PendingRootAttachmentChangesQuery>,
) -> Result<Json<PendingRootAttachmentChanges>, ServerError> {
    let limit = query.limit.unwrap_or(DEFAULT_PENDING_LIMIT);
    if !(1..=MAX_PENDING_ROOT_ATTACHMENT_CHANGES).contains(&limit) {
        return Err(ServerError::bad_request(format!(
            "limit must be between 1 and {MAX_PENDING_ROOT_ATTACHMENT_CHANGES}",
        )));
    }
    let changes = state
        .store
        .list_pending_root_attachment_changes(state.client_executor_id, limit)
        .await?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(Json(PendingRootAttachmentChanges { changes }))
}

/// Finish or recover one exact product-side attachment change.
pub async fn finish_root_attachment_change(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Path(change_id): Path<RootAttachmentChangeId>,
    Json(body): Json<FinishRootAttachmentChangeBody>,
) -> Result<Json<FinishedRootAttachmentChange>, ServerError> {
    body.terminal.validate().map_err(ServerError::bad_request)?;
    let (disposition, change) = match state
        .store
        .finish_root_attachment_change(
            change_id,
            state.client_executor_id,
            &body.terminal,
            Utc::now(),
        )
        .await?
    {
        FinishRootAttachmentChangeOutcome::Finished(change) => {
            (FinishRootAttachmentChangeDisposition::Finished, change)
        }
        FinishRootAttachmentChangeOutcome::Existing(change) => {
            (FinishRootAttachmentChangeDisposition::Existing, change)
        }
        FinishRootAttachmentChangeOutcome::NotFound
        | FinishRootAttachmentChangeOutcome::ExecutorMismatch => {
            return Err(ServerError::not_found("root attachment change not found"));
        }
        FinishRootAttachmentChangeOutcome::AlreadyTerminal(_) => {
            return Err(ServerError::conflict_kind(
                "root_attachment_already_terminal",
                "root attachment change already has a different result",
            ));
        }
        FinishRootAttachmentChangeOutcome::BrokerStateMismatch => {
            return Err(ServerError::conflict_kind(
                "root_attachment_broker_state_mismatch",
                "broker state contradicts this attachment result",
            ));
        }
    };
    Ok(Json(FinishedRootAttachmentChange {
        disposition,
        change: change.into(),
    }))
}
