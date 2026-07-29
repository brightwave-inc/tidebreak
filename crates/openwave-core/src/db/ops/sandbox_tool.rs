use chrono::Duration;
use sea_orm::{
    sea_query::ExprTrait, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::agent_tools::{
    validate_sandbox_read_delegated_file_arguments, SandboxAgentFileResource,
    SANDBOX_READ_DELEGATED_FILE_TOOL,
};
use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId, HostRootId};
use crate::model::{
    AgentRunStatus, AgentRunTier, DelegatedFileReadClaim, SandboxToolCall, SandboxToolCallReceipt,
    SandboxToolCallRequest, SandboxToolCallStatus, ToolCallRecord, ToolCallResolution,
};
use crate::storage::{
    ClaimDelegatedFileReadOutcome, ClaimSandboxToolCallOutcome, ParkSandboxToolCallOutcome,
    ResolveSandboxToolCallOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::agent_run::{
    acquire_agent_run_claim_lock, agent_run_from_model, database_now, find_by_id_on,
};

pub(in crate::db) async fn park_agent_run_for_sandbox_tool_call(
    store: &DbStore,
    agent_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    call: &SandboxToolCallRequest,
) -> Result<ParkSandboxToolCallOutcome> {
    if lease_token.is_nil()
        || !call.is_well_formed()
        || call.agent_run_id != agent_run_id
        || (call.name == SANDBOX_READ_DELEGATED_FILE_TOOL
            && !validate_sandbox_read_delegated_file_arguments(&call.arguments))
    {
        return Err(AgentError::Store(
            "invalid sandbox tool checkpoint request".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    if call.name == SANDBOX_READ_DELEGATED_FILE_TOOL
        && !acquire_chat_write_lock(&transaction, call.chat_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::DelegatedResourceUnavailable);
    }
    let now = database_now(&transaction).await?;

    if let Some(existing) = entities::sandbox_tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let exact = request_matches(&existing, call) && existing.park_lease_token == lease_token;
        let run = find_by_id_on(&transaction, agent_run_id).await?;
        transaction.commit().await.map_err(store_err)?;
        return if exact {
            let run = run.ok_or_else(|| {
                AgentError::Store("sandbox tool checkpoint references missing run".into())
            })?;
            Ok(ParkSandboxToolCallOutcome::Existing {
                run: agent_run_from_model(run)?,
                call: call_from_model(existing)?,
            })
        } else {
            Ok(ParkSandboxToolCallOutcome::IdentityConflict)
        };
    }

    let Some(run) = find_by_id_on(&transaction, agent_run_id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::LeaseLost);
    };
    if call.name == SANDBOX_READ_DELEGATED_FILE_TOOL
        && delegated_file_resource_on(&transaction, agent_run_id, call.chat_id)
            .await?
            .is_none()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::DelegatedResourceUnavailable);
    }
    if run.chat_id != call.chat_id.0
        || run.tier != AgentRunTier::Background.as_str()
        || run.status != AgentRunStatus::Running.as_str()
        || run.lease_token != Some(lease_token)
        || run.lease_expires_at.is_none_or(|expiry| expiry <= now)
        || run.deadline_at.is_none_or(|deadline| deadline <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::LeaseLost);
    }
    let existing_count = entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(agent_run_id.0))
        .count(&transaction)
        .await
        .map_err(store_err)?;
    if existing_count != 0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::IdentityConflict);
    }
    entities::sandbox_tool_call::ActiveModel {
        id: Set(call.id.0),
        agent_run_id: Set(agent_run_id.0),
        chat_id: Set(call.chat_id.0),
        agent_run_depth: Set(1),
        provider_id: Set(call.provider_id.clone()),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        status: Set(SandboxToolCallStatus::Accepted.as_str().into()),
        park_lease_token: Set(lease_token),
        park_attempt_count: Set(run.attempt_count),
        park_claim_count: Set(run.claim_count),
        executor_lease_token: Set(None),
        executor_lease_expires_at: Set(None),
        created_at: Set(now),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Waiting.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(agent_run_id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.eq(run.deadline_at))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::LeaseLost);
    }
    let run = find_by_id_on(&transaction, agent_run_id)
        .await?
        .ok_or_else(|| AgentError::Store("parked sandbox run disappeared".into()))?;
    let call = entities::sandbox_tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("sandbox tool checkpoint disappeared".into()))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ParkSandboxToolCallOutcome::Parked {
        run: agent_run_from_model(run)?,
        call: call_from_model(call)?,
    })
}

pub(in crate::db) async fn claim_sandbox_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<ClaimSandboxToolCallOutcome> {
    claim_sandbox_tool_call_matching(store, id, None, lease_token, lease_duration).await
}

pub(in crate::db) async fn claim_sandbox_tool_call_named(
    store: &DbStore,
    id: CallId,
    name: &str,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<ClaimSandboxToolCallOutcome> {
    if !valid_tool_name(name) {
        return Err(AgentError::Store("invalid sandbox tool name".into()));
    }
    claim_sandbox_tool_call_matching(store, id, Some(name), lease_token, lease_duration).await
}

pub(in crate::db) async fn claim_delegated_file_read(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<ClaimDelegatedFileReadOutcome> {
    if id.0.is_nil() || lease_token.is_nil() || lease_duration <= Duration::zero() {
        return Err(AgentError::Store(
            "invalid delegated file executor lease".into(),
        ));
    }
    let Some(initial) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    };
    if initial.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Attachment mutation and sandbox scheduling share this established order.
    acquire_agent_run_claim_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, ChatId(initial.chat_id)).await? {
        if let Some(current) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .filter(|call| call.name == SANDBOX_READ_DELEGATED_FILE_TOOL)
        {
            terminalize_active_delegated_read_on(
                &transaction,
                current,
                "delegated_file_unavailable",
            )
            .await?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
    let now = database_now(&transaction).await?;
    let requested_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("delegated file executor lease overflows database time".into())
    })?;
    let Some(existing) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    };
    if existing.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
    let Some(resource) = delegated_file_resource_on(
        &transaction,
        AgentRunId(existing.agent_run_id),
        ChatId(existing.chat_id),
    )
    .await?
    .filter(|_| validate_sandbox_read_delegated_file_arguments(&existing.arguments)) else {
        terminalize_active_delegated_read_on(&transaction, existing, "delegated_file_unavailable")
            .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    };
    if existing.status == SandboxToolCallStatus::Claimed.as_str()
        && existing.executor_lease_token == Some(lease_token)
        && existing
            .executor_lease_expires_at
            .is_some_and(|expiry| expiry > now)
    {
        let claim = delegated_file_claim(existing, resource)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Existing(claim));
    }
    if existing.status == SandboxToolCallStatus::Claimed.as_str()
        && existing
            .executor_lease_expires_at
            .is_some_and(|expiry| expiry <= now)
    {
        terminalize_on(&transaction, existing, "executor_lease_expired", now).await?;
        resume_waiting_run_on(&transaction, id, now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
    if existing.status != SandboxToolCallStatus::Accepted.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(existing.agent_run_id)).await? else {
        terminalize_on(&transaction, existing, "delegated_file_unavailable", now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    };
    if run.chat_id != existing.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        terminalize_on(&transaction, existing, "delegated_file_unavailable", now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
    let Some(deadline) = run.deadline_at.filter(|deadline| *deadline > now) else {
        terminalize_on(&transaction, existing, "deadline_exceeded", now).await?;
        resume_waiting_run_on(&transaction, id, now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    };
    let expires_at = Ord::min(requested_expires_at, deadline);
    let mut active: entities::sandbox_tool_call::ActiveModel = existing.into();
    active.status = Set(SandboxToolCallStatus::Claimed.as_str().into());
    active.executor_lease_token = Set(Some(lease_token));
    active.executor_lease_expires_at = Set(Some(expires_at));
    let claimed = active.update(&transaction).await.map_err(store_err)?;
    let claim = delegated_file_claim(claimed, resource)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ClaimDelegatedFileReadOutcome::Claimed(claim))
}

async fn claim_sandbox_tool_call_matching(
    store: &DbStore,
    id: CallId,
    expected_name: Option<&str>,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<ClaimSandboxToolCallOutcome> {
    if id.0.is_nil() || lease_token.is_nil() || lease_duration <= Duration::zero() {
        return Err(AgentError::Store(
            "invalid sandbox tool executor lease".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let requested_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("sandbox tool executor lease overflows database time".into())
    })?;
    let Some(existing) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    };
    // Check the immutable dispatch key before handling an expired lease. A
    // web-search worker must never terminalize a future tool type merely
    // because it happened to observe that call in a generic recovery scan.
    if expected_name.is_some_and(|name| existing.name != name) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    }
    if existing.status == SandboxToolCallStatus::Claimed.as_str()
        && existing.executor_lease_token == Some(lease_token)
        && existing
            .executor_lease_expires_at
            .is_some_and(|expiry| expiry > now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Existing(call_from_model(
            existing,
        )?));
    }
    if existing.status == SandboxToolCallStatus::Claimed.as_str()
        && existing
            .executor_lease_expires_at
            .is_some_and(|expiry| expiry <= now)
    {
        terminalize_on(&transaction, existing, "executor_lease_expired", now).await?;
        resume_waiting_run_on(&transaction, id, now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    }
    if existing.status != SandboxToolCallStatus::Accepted.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(existing.agent_run_id)).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    };
    if run.chat_id != existing.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    }
    let Some(deadline) = run.deadline_at.filter(|deadline| *deadline > now) else {
        terminalize_on(&transaction, existing, "deadline_exceeded", now).await?;
        resume_waiting_run_on(&transaction, id, now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    };
    let expires_at = Ord::min(requested_expires_at, deadline);
    let mut active: entities::sandbox_tool_call::ActiveModel = existing.into();
    active.status = Set(SandboxToolCallStatus::Claimed.as_str().into());
    active.executor_lease_token = Set(Some(lease_token));
    active.executor_lease_expires_at = Set(Some(expires_at));
    let claimed = active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ClaimSandboxToolCallOutcome::Claimed(call_from_model(
        claimed,
    )?))
}

/// Extend an exact sandbox-tool executor lease after rechecking the waiting
/// sandbox run and its database-clock deadline. This is deliberately separate
/// from the model-worker lease: a parked run has released that lease, and only
/// this executor capability can authorize its outbound continuation.
pub(in crate::db) async fn heartbeat_sandbox_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<Option<Duration>> {
    if id.0.is_nil() || lease_token.is_nil() || lease_duration <= Duration::zero() {
        return Err(AgentError::Store(
            "invalid sandbox tool executor heartbeat".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let requested_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("sandbox tool executor lease overflows database time".into())
    })?;
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if call.status != SandboxToolCallStatus::Claimed.as_str()
        || call.executor_lease_token != Some(lease_token)
        || call
            .executor_lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(call.agent_run_id)).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(deadline) = run.deadline_at.filter(|deadline| *deadline > now) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.chat_id != call.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let expires_at = Ord::min(requested_expires_at, deadline);
    if call
        .executor_lease_expires_at
        .is_some_and(|current| current >= expires_at)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(call.executor_lease_expires_at.map(|expiry| expiry - now));
    }
    let mut active: entities::sandbox_tool_call::ActiveModel = call.into();
    active.executor_lease_expires_at = Set(Some(expires_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(expires_at - now))
}

pub(in crate::db) async fn heartbeat_delegated_file_read(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    lease_duration: Duration,
) -> Result<Option<Duration>> {
    if id.0.is_nil() || lease_token.is_nil() || lease_duration <= Duration::zero() {
        return Err(AgentError::Store(
            "invalid delegated file executor heartbeat".into(),
        ));
    }
    let Some(initial) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if initial.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        return Ok(None);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, ChatId(initial.chat_id)).await? {
        if let Some(current) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .filter(|call| call.name == SANDBOX_READ_DELEGATED_FILE_TOOL)
        {
            terminalize_active_delegated_read_on(
                &transaction,
                current,
                "delegated_file_unavailable",
            )
            .await?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;
    let requested_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("delegated file executor lease overflows database time".into())
    })?;
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if call.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if delegated_file_resource_on(
        &transaction,
        AgentRunId(call.agent_run_id),
        ChatId(call.chat_id),
    )
    .await?
    .filter(|_| validate_sandbox_read_delegated_file_arguments(&call.arguments))
    .is_none()
    {
        terminalize_active_delegated_read_on(&transaction, call, "delegated_file_unavailable")
            .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if call.status != SandboxToolCallStatus::Claimed.as_str()
        || call.executor_lease_token != Some(lease_token)
        || call
            .executor_lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(call.agent_run_id)).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(deadline) = run.deadline_at.filter(|deadline| *deadline > now) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.chat_id != call.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let expires_at = Ord::min(requested_expires_at, deadline);
    if call
        .executor_lease_expires_at
        .is_some_and(|current| current >= expires_at)
    {
        let remaining = call.executor_lease_expires_at.map(|expiry| expiry - now);
        transaction.commit().await.map_err(store_err)?;
        return Ok(remaining);
    }
    let mut active: entities::sandbox_tool_call::ActiveModel = call.into();
    active.executor_lease_expires_at = Set(Some(expires_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(expires_at - now))
}

pub(in crate::db) async fn resolve_sandbox_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    resolution: &ToolCallResolution,
) -> Result<ResolveSandboxToolCallOutcome> {
    validate_resolution(resolution)?;
    if id.0.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "invalid sandbox tool resolution lease".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if let Some(receipt) = entities::sandbox_tool_call_receipt::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let exact =
            receipt.executor_lease_token == lease_token && receipt_matches(&receipt, resolution);
        transaction.commit().await.map_err(store_err)?;
        return Ok(if exact {
            ResolveSandboxToolCallOutcome::Existing
        } else {
            ResolveSandboxToolCallOutcome::AlreadyTerminal
        });
    }
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    };
    if call.status != SandboxToolCallStatus::Claimed.as_str()
        || call.executor_lease_token != Some(lease_token)
        || call
            .executor_lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(call.agent_run_id)).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    };
    if run.chat_id != call.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    let status = resolution_status(resolution);
    let (error_code, error_detail) = resolution_error(resolution);
    entities::sandbox_tool_call_receipt::ActiveModel {
        call_id: Set(id.0),
        executor_lease_token: Set(lease_token),
        status: Set(status.as_str().into()),
        result: Set(resolution.result().to_owned()),
        error_code: Set(error_code),
        error_detail: Set(error_detail),
        resolved_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let mut call_active: entities::sandbox_tool_call::ActiveModel = call.into();
    call_active.status = Set(status.as_str().into());
    call_active.executor_lease_token = Set(None);
    call_active.executor_lease_expires_at = Set(None);
    call_active.resolved_at = Set(Some(now));
    call_active.update(&transaction).await.map_err(store_err)?;
    let resumed = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::RetryWait.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some("tool_checkpoint_resolved".to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(
                "sandbox tool result receipt committed".to_owned(),
            )),
        )
        .col_expr(
            entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(run.id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Waiting.as_str()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if resumed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(ResolveSandboxToolCallOutcome::Resolved)
}

pub(in crate::db) async fn resolve_delegated_file_read(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    resolution: &ToolCallResolution,
) -> Result<ResolveSandboxToolCallOutcome> {
    validate_resolution(resolution)?;
    if id.0.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "invalid delegated file resolution lease".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let Some(scope) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    };
    if scope.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    }
    // Exact terminal recovery precedes mutable attachment state. A committed
    // response stays recoverable even if the root is detached afterwards.
    if let Some(receipt) = entities::sandbox_tool_call_receipt::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let exact =
            receipt.executor_lease_token == lease_token && receipt_matches(&receipt, resolution);
        transaction.commit().await.map_err(store_err)?;
        return Ok(if exact {
            ResolveSandboxToolCallOutcome::Existing
        } else {
            ResolveSandboxToolCallOutcome::AlreadyTerminal
        });
    }
    if !acquire_chat_write_lock(&transaction, ChatId(scope.chat_id)).await? {
        if let Some(current) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .filter(|call| call.name == SANDBOX_READ_DELEGATED_FILE_TOOL)
        {
            terminalize_active_delegated_read_on(
                &transaction,
                current,
                "delegated_file_unavailable",
            )
            .await?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    let now = database_now(&transaction).await?;
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    };
    if call.name != SANDBOX_READ_DELEGATED_FILE_TOOL {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    }
    if let Some(receipt) = entities::sandbox_tool_call_receipt::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let exact =
            receipt.executor_lease_token == lease_token && receipt_matches(&receipt, resolution);
        transaction.commit().await.map_err(store_err)?;
        return Ok(if exact {
            ResolveSandboxToolCallOutcome::Existing
        } else {
            ResolveSandboxToolCallOutcome::AlreadyTerminal
        });
    }
    if delegated_file_resource_on(
        &transaction,
        AgentRunId(call.agent_run_id),
        ChatId(call.chat_id),
    )
    .await?
    .filter(|_| validate_sandbox_read_delegated_file_arguments(&call.arguments))
    .is_none()
    {
        terminalize_active_delegated_read_on(&transaction, call, "delegated_file_unavailable")
            .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    if call.status != SandboxToolCallStatus::Claimed.as_str()
        || call.executor_lease_token != Some(lease_token)
        || call
            .executor_lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(call.agent_run_id)).await? else {
        terminalize_active_delegated_read_on(&transaction, call, "delegated_file_unavailable")
            .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    };
    if run.chat_id != call.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        terminalize_active_delegated_read_on(&transaction, call, "delegated_file_unavailable")
            .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    if run.deadline_at.is_none_or(|deadline| deadline <= now) {
        terminalize_on(&transaction, call, "deadline_exceeded", now).await?;
        resume_waiting_run_on(&transaction, id, now).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    let status = resolution_status(resolution);
    let (error_code, error_detail) = resolution_error(resolution);
    entities::sandbox_tool_call_receipt::ActiveModel {
        call_id: Set(id.0),
        executor_lease_token: Set(lease_token),
        status: Set(status.as_str().into()),
        result: Set(resolution.result().to_owned()),
        error_code: Set(error_code),
        error_detail: Set(error_detail),
        resolved_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let mut call_active: entities::sandbox_tool_call::ActiveModel = call.into();
    call_active.status = Set(status.as_str().into());
    call_active.executor_lease_token = Set(None);
    call_active.executor_lease_expires_at = Set(None);
    call_active.resolved_at = Set(Some(now));
    call_active.update(&transaction).await.map_err(store_err)?;
    let resumed = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::RetryWait.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some("tool_checkpoint_resolved".to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(
                "sandbox tool result receipt committed".to_owned(),
            )),
        )
        .col_expr(
            entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(run.id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Waiting.as_str()))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if resumed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(ResolveSandboxToolCallOutcome::Resolved)
}

pub(in crate::db) async fn get_sandbox_tool_call(
    store: &DbStore,
    id: CallId,
) -> Result<Option<SandboxToolCall>> {
    entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(call_from_model)
        .transpose()
}

pub(in crate::db) async fn get_sandbox_tool_call_receipt(
    store: &DbStore,
    id: CallId,
) -> Result<Option<SandboxToolCallReceipt>> {
    entities::sandbox_tool_call_receipt::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(receipt_from_model)
        .transpose()
}

pub(in crate::db) async fn list_sandbox_tool_calls_for_agent_run(
    store: &DbStore,
    agent_run_id: AgentRunId,
) -> Result<Vec<SandboxToolCall>> {
    entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(agent_run_id.0))
        // The worker claim count is monotonic within the isolated run, unlike
        // SQLite timestamps and random IDs, so it is the replay order.
        .order_by_asc(entities::sandbox_tool_call::Column::ParkAttemptCount)
        .order_by_asc(entities::sandbox_tool_call::Column::ParkClaimCount)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(call_from_model)
        .collect()
}

pub(in crate::db) async fn list_sandbox_tool_call_candidates(
    store: &DbStore,
    limit: u64,
) -> Result<Vec<SandboxToolCall>> {
    if limit == 0 || limit > 256 {
        return Err(AgentError::Store(
            "invalid sandbox tool candidate limit".into(),
        ));
    }
    let now = database_now(&store.conn).await?;
    entities::sandbox_tool_call::Entity::find()
        .filter(
            sea_orm::Condition::any()
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::Accepted.as_str()),
                )
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::Claimed.as_str())
                        .and(entities::sandbox_tool_call::Column::ExecutorLeaseExpiresAt.lte(now)),
                ),
        )
        .order_by_asc(entities::sandbox_tool_call::Column::CreatedAt)
        .order_by_asc(entities::sandbox_tool_call::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(call_from_model)
        .collect()
}

pub(in crate::db) async fn list_sandbox_tool_call_candidates_named(
    store: &DbStore,
    name: &str,
    limit: u64,
) -> Result<Vec<SandboxToolCall>> {
    if !valid_tool_name(name) {
        return Err(AgentError::Store("invalid sandbox tool name".into()));
    }
    if limit == 0 || limit > 256 {
        return Err(AgentError::Store(
            "invalid sandbox tool candidate limit".into(),
        ));
    }
    let now = database_now(&store.conn).await?;
    entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::Name.eq(name))
        .filter(
            sea_orm::Condition::any()
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::Accepted.as_str()),
                )
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::Claimed.as_str())
                        .and(entities::sandbox_tool_call::Column::ExecutorLeaseExpiresAt.lte(now)),
                ),
        )
        .order_by_asc(entities::sandbox_tool_call::Column::CreatedAt)
        .order_by_asc(entities::sandbox_tool_call::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(call_from_model)
        .collect()
}

/// Terminalize accepted or expired claimed work in the same transaction that
/// fences its waiting sandbox. A late executor cannot overwrite this receipt.
pub(in crate::db) async fn cancel_sandbox_tool_call_for_run_on<C>(
    conn: &C,
    agent_run_id: AgentRunId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    terminalize_sandbox_tool_call_for_run_on(conn, agent_run_id, "cancelled", now).await
}

pub(in crate::db) async fn terminalize_sandbox_tool_call_for_run_on<C>(
    conn: &C,
    agent_run_id: AgentRunId,
    error_code: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let calls = entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(agent_run_id.0))
        .filter(entities::sandbox_tool_call::Column::Status.is_in([
            SandboxToolCallStatus::Accepted.as_str(),
            SandboxToolCallStatus::Claimed.as_str(),
        ]))
        .all(conn)
        .await
        .map_err(store_err)?;
    if calls.len() > 1 {
        return Err(AgentError::Store(
            "sandbox run has multiple live tool calls".into(),
        ));
    }
    if let Some(call) = calls.into_iter().next() {
        terminalize_on(conn, call, error_code, now).await?;
    }
    Ok(())
}

async fn terminalize_on<C>(
    conn: &C,
    call: entities::sandbox_tool_call::Model,
    error_code: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let executor_lease_token = call.executor_lease_token.unwrap_or(call.park_lease_token);
    let result = if error_code == "cancelled" {
        "Sandbox tool call cancelled.".to_owned()
    } else {
        format!("Sandbox tool call failed ({error_code}).")
    };
    entities::sandbox_tool_call_receipt::ActiveModel {
        call_id: Set(call.id),
        executor_lease_token: Set(executor_lease_token),
        status: Set(if error_code == "cancelled" {
            SandboxToolCallStatus::Cancelled.as_str().into()
        } else {
            SandboxToolCallStatus::Failed.as_str().into()
        }),
        result: Set(result),
        error_code: Set((error_code != "cancelled").then(|| error_code.to_owned())),
        error_detail: Set(None),
        resolved_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    let mut active: entities::sandbox_tool_call::ActiveModel = call.into();
    active.status = Set(if error_code == "cancelled" {
        SandboxToolCallStatus::Cancelled.as_str().into()
    } else {
        SandboxToolCallStatus::Failed.as_str().into()
    });
    active.executor_lease_token = Set(None);
    active.executor_lease_expires_at = Set(None);
    active.resolved_at = Set(Some(now));
    active.update(conn).await.map_err(store_err)?;
    Ok(())
}

async fn resume_waiting_run_on<C>(
    conn: &C,
    call_id: CallId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let call = entities::sandbox_tool_call::Entity::find_by_id(call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("terminal sandbox tool call disappeared".into()))?;
    entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::RetryWait.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some("tool_checkpoint_resolved".to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(
                "sandbox tool result receipt committed".to_owned(),
            )),
        )
        .col_expr(
            entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(call.agent_run_id))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Waiting.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

async fn terminalize_active_delegated_read_on<C>(
    conn: &C,
    call: entities::sandbox_tool_call::Model,
    error_code: &str,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if !matches!(call.status.as_str(), "accepted" | "claimed") {
        return Ok(());
    }
    let now = database_now(conn).await?;
    let call_id = CallId(call.id);
    terminalize_on(conn, call, error_code, now).await?;
    resume_waiting_run_on(conn, call_id, now).await
}

async fn delegated_file_resource_on<C>(
    conn: &C,
    agent_run_id: AgentRunId,
    chat_id: ChatId,
) -> Result<Option<SandboxAgentFileResource>>
where
    C: ConnectionTrait,
{
    let Some(admission) = entities::sandbox_agent_admission::Entity::find_by_id(agent_run_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if admission.child_run_id != agent_run_id.0
        || admission.chat_id != chat_id.0
        || admission.parent_run_id.is_nil()
        || admission.origin_turn_id.is_nil()
        || AgentRunId::sandbox_for_spawn_call(CallId(admission.spawn_call_id)) != agent_run_id
    {
        return Ok(None);
    }
    let (Some(root_uuid), Some(relative_path)) = (
        admission.delegated_root_id,
        admission.delegated_relative_path,
    ) else {
        return Ok(None);
    };
    let Ok(root_id) = HostRootId::from_uuid(root_uuid) else {
        return Ok(None);
    };
    let resource = SandboxAgentFileResource {
        root_id,
        relative_path,
    };
    if !resource.is_well_formed() {
        return Ok(None);
    }
    let attached = entities::chat_root_attachment::Entity::find_by_id((chat_id.0, root_uuid))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some();
    Ok(attached.then_some(resource))
}

fn delegated_file_claim(
    call: entities::sandbox_tool_call::Model,
    resource: SandboxAgentFileResource,
) -> Result<DelegatedFileReadClaim> {
    Ok(DelegatedFileReadClaim {
        call: call_from_model(call)?,
        root_id: resource.root_id,
        relative_path: resource.relative_path,
    })
}

fn request_matches(
    model: &entities::sandbox_tool_call::Model,
    call: &SandboxToolCallRequest,
) -> bool {
    model.agent_run_id == call.agent_run_id.0
        && model.chat_id == call.chat_id.0
        && model.provider_id == call.provider_id
        && model.name == call.name
        && model.arguments == call.arguments
}

fn resolution_status(resolution: &ToolCallResolution) -> SandboxToolCallStatus {
    match resolution {
        ToolCallResolution::Completed { .. } => SandboxToolCallStatus::Completed,
        ToolCallResolution::Failed { .. } => SandboxToolCallStatus::Failed,
        ToolCallResolution::Cancelled { .. } => SandboxToolCallStatus::Cancelled,
    }
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

fn validate_resolution(resolution: &ToolCallResolution) -> Result<()> {
    let (error_code, error_detail) = resolution_error(resolution);
    if resolution.result().is_empty()
        || resolution.result().len() > SandboxToolCall::MAX_RESULT_BYTES
        || resolution.result().contains('\0')
        || error_code.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.len() > ToolCallRecord::MAX_ERROR_CODE_LEN
                || value.contains('\0')
        })
        || error_detail.as_deref().is_some_and(|value| {
            value.is_empty()
                || value.len() > ToolCallRecord::MAX_ERROR_DETAIL_LEN
                || value.contains('\0')
        })
    {
        return Err(AgentError::Store("invalid sandbox tool resolution".into()));
    }
    Ok(())
}

fn receipt_matches(
    receipt: &entities::sandbox_tool_call_receipt::Model,
    resolution: &ToolCallResolution,
) -> bool {
    let (error_code, error_detail) = resolution_error(resolution);
    receipt.status == resolution_status(resolution).as_str()
        && receipt.result == resolution.result()
        && receipt.error_code == error_code
        && receipt.error_detail == error_detail
}

fn call_from_model(model: entities::sandbox_tool_call::Model) -> Result<SandboxToolCall> {
    Ok(SandboxToolCall {
        id: CallId(model.id),
        agent_run_id: AgentRunId(model.agent_run_id),
        chat_id: crate::id::ChatId(model.chat_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        status: status_from_db(&model.status)?,
        park_lease_token: model.park_lease_token,
        park_attempt_count: model.park_attempt_count,
        park_claim_count: model.park_claim_count,
        executor_lease_token: model.executor_lease_token,
        executor_lease_expires_at: model.executor_lease_expires_at,
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn receipt_from_model(
    model: entities::sandbox_tool_call_receipt::Model,
) -> Result<SandboxToolCallReceipt> {
    Ok(SandboxToolCallReceipt {
        call_id: CallId(model.call_id),
        executor_lease_token: model.executor_lease_token,
        status: status_from_db(&model.status)?,
        result: model.result,
        error_code: model.error_code,
        error_detail: model.error_detail,
        resolved_at: model.resolved_at,
    })
}

fn status_from_db(value: &str) -> Result<SandboxToolCallStatus> {
    match value {
        "accepted" => Ok(SandboxToolCallStatus::Accepted),
        "claimed" => Ok(SandboxToolCallStatus::Claimed),
        "completed" => Ok(SandboxToolCallStatus::Completed),
        "failed" => Ok(SandboxToolCallStatus::Failed),
        "cancelled" => Ok(SandboxToolCallStatus::Cancelled),
        _ => Err(AgentError::Store(format!(
            "invalid sandbox tool status {value}"
        ))),
    }
}

fn valid_tool_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= ToolCallRecord::MAX_LABEL_LEN && !value.contains('\0')
}
