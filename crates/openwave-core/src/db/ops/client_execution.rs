use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{CallId, ChatId};
use crate::model::{ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus};
use crate::storage::{
    AcceptToolCallOutcome, ClaimClientToolCallOutcome, ClientToolCallClaim,
    HeartbeatClientToolCallOutcome, ResolveToolCallOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock};

pub(in crate::db) async fn accept_tool_call(
    store: &DbStore,
    call: &ToolCallRecord,
) -> Result<AcceptToolCallOutcome> {
    validate_accept(call)?;
    let created_at = canonical_db_timestamp(call.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, call.chat_id).await? {
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            call.chat_id
        )));
    }
    if let Some(existing) = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = if immutable_request_matches(&existing, call, created_at) {
            AcceptToolCallOutcome::Existing(tool_call_from_model(existing)?)
        } else {
            AcceptToolCallOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }

    let inserted = entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(call.turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        created_at: Set(created_at),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = tool_call_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptToolCallOutcome::Accepted(inserted))
}

pub(in crate::db) async fn claim_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    executor_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<ClaimClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if executor_id.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "client executor id and lease token must not be nil".into(),
        ));
    }
    if lease_expires_at <= now {
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0
        || existing.execution != ToolCallExecution::Client.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    if existing.client_executor_id == Some(executor_id)
        && existing.client_lease_token == Some(lease_token)
        && existing.client_lease_expires_at == Some(lease_expires_at)
        && lease_expires_at > now
    {
        let claim = client_claim_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Existing(claim));
    }
    if existing.client_executor_id.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }

    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_executor_id = Set(Some(executor_id));
    active.client_lease_token = Set(Some(lease_token));
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    let claimed = active.update(&transaction).await.map_err(store_err)?;
    let claim = client_claim_from_model(claimed)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ClaimClientToolCallOutcome::Claimed(claim))
}

pub(in crate::db) async fn heartbeat_client_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<HeartbeatClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    validate_lease(lease_token, now, lease_expires_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let current_expiry = existing.client_lease_expires_at;
    if existing.execution != ToolCallExecution::Client.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.client_lease_token != Some(lease_token)
        || current_expiry.is_none_or(|expiry| expiry <= now || lease_expires_at < expiry)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    if current_expiry == Some(lease_expires_at) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::Existing);
    }
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(HeartbeatClientToolCallOutcome::Extended)
}

pub(in crate::db) async fn resolve_server_tool_call(
    store: &DbStore,
    id: CallId,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::Server,
        resolved_at,
        resolution,
    )
    .await
}

pub(in crate::db) async fn resolve_client_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::LiveClient { lease_token, now },
        resolved_at,
        resolution,
    )
    .await
}

pub(in crate::db) async fn resolve_expired_client_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::ExpiredClient { lease_token, now },
        resolved_at,
        resolution,
    )
    .await
}

pub(in crate::db) async fn list_pending_client_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    let models = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::CreatedAt)
        .order_by_asc(entities::tool_call::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    models.into_iter().map(tool_call_from_model).collect()
}

async fn resolve_tool_call(
    store: &DbStore,
    id: CallId,
    authority: ResolutionAuthority,
    resolved_at: DateTime<Utc>,
    resolution: &ToolCallResolution,
) -> Result<ResolveToolCallOutcome> {
    validate_resolution(resolution)?;
    let resolved_at = canonical_db_timestamp(resolved_at)?;
    let authority = authority.canonicalized()?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveToolCallOutcome::NotFound);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if resolved_at < existing.created_at {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(
            "tool call cannot resolve before it was created".into(),
        ));
    }
    if existing.status != ToolCallStatus::Pending.as_str() {
        let outcome = if !terminal_authority_matches(&existing, authority) {
            ResolveToolCallOutcome::LeaseLost
        } else if terminal_payload_matches(&existing, resolution, resolved_at) {
            ResolveToolCallOutcome::Existing
        } else {
            ResolveToolCallOutcome::AlreadyTerminal
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }

    let owns = match authority {
        ResolutionAuthority::Server => existing.execution == ToolCallExecution::Server.as_str(),
        ResolutionAuthority::LiveClient { lease_token, now } => {
            existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry > now)
        }
        ResolutionAuthority::ExpiredClient { lease_token, now } => {
            existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry <= now)
        }
    };
    if !owns {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveToolCallOutcome::LeaseLost);
    }

    let (error_code, error_detail) = resolution_error(resolution);
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.status = Set(resolution.status().as_str().into());
    active.result = Set(Some(resolution.result().to_owned()));
    active.error_code = Set(error_code);
    active.error_detail = Set(error_detail);
    active.client_lease_expires_at = Set(None);
    active.resolved_at = Set(Some(resolved_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ResolveToolCallOutcome::Resolved)
}

#[derive(Clone, Copy)]
enum ResolutionAuthority {
    Server,
    LiveClient {
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
    ExpiredClient {
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
}

impl ResolutionAuthority {
    fn canonicalized(self) -> Result<Self> {
        Ok(match self {
            Self::Server => Self::Server,
            Self::LiveClient { lease_token, now } => Self::LiveClient {
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
            Self::ExpiredClient { lease_token, now } => Self::ExpiredClient {
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
        })
    }

    const fn lease_token(self) -> Option<uuid::Uuid> {
        match self {
            Self::Server => None,
            Self::LiveClient { lease_token, .. } | Self::ExpiredClient { lease_token, .. } => {
                Some(lease_token)
            }
        }
    }
}

fn validate_accept(call: &ToolCallRecord) -> Result<()> {
    let labels_valid = [call.provider_id.as_str(), call.name.as_str()]
        .into_iter()
        .all(|value| {
            !value.is_empty()
                && value.len() <= ToolCallRecord::MAX_LABEL_LEN
                && !value.contains('\0')
        });
    let args_len = serde_json::to_vec(&call.arguments)
        .map_err(|error| AgentError::Store(format!("serialize tool arguments: {error}")))?
        .len();
    if call.id.0.is_nil()
        || !labels_valid
        || args_len > ToolCallRecord::MAX_ARGUMENT_BYTES
        || call.status != ToolCallStatus::Pending
        || call.result.is_some()
        || call.error_code.is_some()
        || call.error_detail.is_some()
        || call.client_executor_id.is_some()
        || call.client_lease_expires_at.is_some()
        || call.resolved_at.is_some()
    {
        return Err(AgentError::Store("invalid accepted tool call".into()));
    }
    Ok(())
}

fn validate_lease(
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<()> {
    if lease_token.is_nil() || lease_expires_at <= now {
        return Err(AgentError::Store("invalid client execution lease".into()));
    }
    Ok(())
}

fn validate_resolution(resolution: &ToolCallResolution) -> Result<()> {
    let (error_code, error_detail) = resolution_error(resolution);
    if resolution.result().len() > ToolCallRecord::MAX_RESULT_BYTES
        || resolution.result().contains('\0')
        || error_code.as_deref().is_some_and(|code| {
            code.is_empty()
                || code.len() > ToolCallRecord::MAX_ERROR_CODE_LEN
                || code.contains('\0')
        })
        || error_detail.as_deref().is_some_and(|detail| {
            detail.is_empty()
                || detail.len() > ToolCallRecord::MAX_ERROR_DETAIL_LEN
                || detail.contains('\0')
        })
    {
        return Err(AgentError::Store("invalid tool call resolution".into()));
    }
    Ok(())
}

fn resolution_error(resolution: &ToolCallResolution) -> (Option<String>, Option<String>) {
    match resolution {
        ToolCallResolution::Failed {
            error_code,
            error_detail,
            ..
        } => (Some(error_code.clone()), error_detail.clone()),
        ToolCallResolution::Completed { .. } | ToolCallResolution::Cancelled { .. } => (None, None),
    }
}

fn immutable_request_matches(
    model: &entities::tool_call::Model,
    call: &ToolCallRecord,
    created_at: DateTime<Utc>,
) -> bool {
    model.chat_id == call.chat_id.0
        && model.turn_id == call.turn_id.0
        && model.provider_id == call.provider_id
        && model.name == call.name
        && model.arguments == call.arguments
        && model.execution == call.execution.as_str()
        && model.created_at == created_at
}

fn terminal_authority_matches(
    model: &entities::tool_call::Model,
    authority: ResolutionAuthority,
) -> bool {
    match authority {
        ResolutionAuthority::Server => {
            model.execution == ToolCallExecution::Server.as_str()
                && model.client_lease_token.is_none()
        }
        ResolutionAuthority::LiveClient { .. } | ResolutionAuthority::ExpiredClient { .. } => {
            model.execution == ToolCallExecution::Client.as_str()
                && model.client_lease_token == authority.lease_token()
        }
    }
}

fn terminal_payload_matches(
    model: &entities::tool_call::Model,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> bool {
    let (error_code, error_detail) = resolution_error(resolution);
    model.status == resolution.status().as_str()
        && model.result.as_deref() == Some(resolution.result())
        && model.error_code == error_code
        && model.error_detail == error_detail
        && model.resolved_at == Some(resolved_at)
}

pub(in crate::db) fn tool_call_from_model(
    model: entities::tool_call::Model,
) -> Result<ToolCallRecord> {
    Ok(ToolCallRecord {
        id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: crate::id::TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        execution: execution_from_db(&model.execution)?,
        status: status_from_db(&model.status)?,
        result: model.result,
        error_code: model.error_code,
        error_detail: model.error_detail,
        client_executor_id: model.client_executor_id,
        client_lease_expires_at: model.client_lease_expires_at,
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn client_claim_from_model(model: entities::tool_call::Model) -> Result<ClientToolCallClaim> {
    let lease_token = model.client_lease_token.ok_or_else(|| {
        AgentError::Store("claimed client tool call is missing its lease token".into())
    })?;
    Ok(ClientToolCallClaim {
        call: tool_call_from_model(model)?,
        lease_token,
    })
}

fn execution_from_db(value: &str) -> Result<ToolCallExecution> {
    match value {
        "server" => Ok(ToolCallExecution::Server),
        "client" => Ok(ToolCallExecution::Client),
        _ => Err(AgentError::Store(format!("invalid tool execution {value}"))),
    }
}

fn status_from_db(value: &str) -> Result<ToolCallStatus> {
    match value {
        "pending" => Ok(ToolCallStatus::Pending),
        "completed" => Ok(ToolCallStatus::Completed),
        "failed" => Ok(ToolCallStatus::Failed),
        "cancelled" => Ok(ToolCallStatus::Cancelled),
        _ => Err(AgentError::Store(format!("invalid tool status {value}"))),
    }
}
