use chrono::Duration;
use sea_orm::{
    sea_query::ExprTrait, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::agent_tools::{
    sandbox_call_is_parallel_eligible, validate_sandbox_exec_arguments,
    validate_sandbox_read_delegated_file_arguments, SandboxAgentFileResource,
    MAX_SANDBOX_TASK_PLAN_CALLS, MAX_SANDBOX_TOOL_CALLS, SANDBOX_EXEC_TOOL,
    SANDBOX_READ_DELEGATED_FILE_TOOL,
};
use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId, HostRootId};
use crate::model::{
    AgentRunStatus, AgentRunTier, DelegatedFileReadClaim, SandboxToolCall,
    SandboxToolCallParkEntry, SandboxToolCallReceipt, SandboxToolCallRequest,
    SandboxToolCallStatus, ToolCallRecord, ToolCallResolution,
};
use crate::storage::{
    ClaimDelegatedFileReadOutcome, ClaimSandboxToolCallOutcome, ParkSandboxToolCallOutcome,
    ResolveSandboxToolCallOutcome, RetrySandboxToolCallOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::agent_run::{
    acquire_agent_run_claim_lock, agent_run_from_model, database_now, find_by_id_on,
};

/// Park every tool call one model step emitted as a single durable batch.
///
/// The step is the unit: its calls are inserted together under one park lease,
/// carry their emission order as `batch_ordinal`, and are replayed together as
/// one assistant message and one tool-result message. The run resumes when the
/// last of them lands terminal, not when the first does.
pub(in crate::db) async fn park_agent_run_for_sandbox_tool_calls(
    store: &DbStore,
    agent_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    entries: &[SandboxToolCallParkEntry],
) -> Result<ParkSandboxToolCallOutcome> {
    if entries.is_empty() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "invalid sandbox tool checkpoint request".into(),
        ));
    }
    // Both are the batch's own identity keys: the ids key the rows, and the
    // provider ids key the replayed tool-use blocks the model
    // reads back. A repeat of either would make the transcript ambiguous.
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_provider_ids = std::collections::HashSet::new();
    for entry in entries {
        if !seen_ids.insert(entry.call.id) || !seen_provider_ids.insert(&entry.call.provider_id) {
            return Err(AgentError::Store(
                "duplicate sandbox tool checkpoint in one step".into(),
            ));
        }
    }
    for entry in entries {
        let SandboxToolCallParkEntry { call, resolution } = entry;
        // A synthetic entry carries the model's own arguments verbatim so the
        // replayed transcript shows what it actually sent. Only the identity
        // fields are checked for it; the per-tool argument rules exist to keep
        // an executor lane from inheriting work it can only fail on, and no
        // lane will ever see this row.
        let dispatchable = resolution.is_none();
        if !call.is_well_formed()
            || call.agent_run_id != agent_run_id
            || call.chat_id != entries[0].call.chat_id
            || (dispatchable
                && call.name == SANDBOX_READ_DELEGATED_FILE_TOOL
                && !validate_sandbox_read_delegated_file_arguments(&call.arguments))
            // Exec arguments are checked here as well as in the executor: the
            // checkpoint is immutable once parked, so an out-of-bounds command
            // must never become a durable row the lane can only fail on.
            || (dispatchable
                && call.name == SANDBOX_EXEC_TOOL
                && !validate_sandbox_exec_arguments(&call.arguments))
        {
            return Err(AgentError::Store(
                "invalid sandbox tool checkpoint request".into(),
            ));
        }
        if let Some(resolution) = resolution {
            validate_resolution(resolution)?;
        }
    }
    let chat_id = entries[0].call.chat_id;
    let needs_delegated_file = entries.iter().any(|entry| {
        entry.resolution.is_none() && entry.call.name == SANDBOX_READ_DELEGATED_FILE_TOOL
    });
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    if needs_delegated_file && !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::DelegatedResourceUnavailable);
    }
    let now = database_now(&transaction).await?;

    if entities::sandbox_tool_call::Entity::find_by_id(entries[0].call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        // The batch commits as one transaction, so a recovered commit is this
        // exact park only when every row — including the outcome any
        // already-resolved entry carried in with it — is present and matches.
        let mut recovered = Vec::with_capacity(entries.len());
        let mut exact = true;
        for (ordinal, entry) in entries.iter().enumerate() {
            let Some(existing) = entities::sandbox_tool_call::Entity::find_by_id(entry.call.id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?
            else {
                exact = false;
                break;
            };
            if !request_matches(&existing, &entry.call)
                || existing.park_lease_token != lease_token
                || existing.batch_ordinal != ordinal_of(ordinal)?
                || !entry
                    .resolution
                    .as_ref()
                    .is_none_or(|resolution| resolution_matches(&existing, resolution))
            {
                exact = false;
                break;
            }
            recovered.push(existing);
        }
        let run = find_by_id_on(&transaction, agent_run_id).await?;
        transaction.commit().await.map_err(store_err)?;
        return if exact {
            let run = run.ok_or_else(|| {
                AgentError::Store("sandbox tool checkpoint references missing run".into())
            })?;
            Ok(ParkSandboxToolCallOutcome::Existing {
                run: agent_run_from_model(run)?,
                calls: recovered
                    .into_iter()
                    .map(call_from_model)
                    .collect::<Result<Vec<_>>>()?,
            })
        } else {
            Ok(ParkSandboxToolCallOutcome::IdentityConflict)
        };
    }

    let Some(run) = find_by_id_on(&transaction, agent_run_id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::LeaseLost);
    };
    if needs_delegated_file
        && delegated_file_resource_on(&transaction, agent_run_id, chat_id)
            .await?
            .is_none()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::DelegatedResourceUnavailable);
    }
    if run.chat_id != chat_id.0
        || run.tier != AgentRunTier::Background.as_str()
        || run.status != AgentRunStatus::Running.as_str()
        || run.lease_token != Some(lease_token)
        || run.lease_expires_at.is_none_or(|expiry| expiry <= now)
        || run.deadline_at.is_none_or(|deadline| deadline <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::LeaseLost);
    }
    // A run parks one step's calls at a time: parking releases the lease, and
    // the worker only regains it once every call in the step resolves. So an
    // unresolved sibling means this park came from a worker running on a stale
    // view, and the chain is bounded because the worker replays all of it on
    // every claim. The caller truncates an oversized step before it gets here;
    // this keeps the durable bound regardless of what the caller computed.
    let siblings = entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(agent_run_id.0))
        .select_only()
        .column(entities::sandbox_tool_call::Column::Status)
        .column(entities::sandbox_tool_call::Column::Name)
        .into_tuple::<(String, String)>()
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let all_resolved = siblings
        .iter()
        .all(|(status, _)| status_from_db(status).is_ok_and(SandboxToolCallStatus::is_terminal));
    // Two budgets, counted by tool name. Plan bookkeeping is deliberately not
    // charged to the work budget — a run told to keep its checklist current
    // would otherwise spend the allowance for the exec and search calls the
    // task is for — so the durable bound has to know which row is which. The
    // caller trims an oversized step before it gets here; this keeps both
    // bounds regardless of what the caller computed.
    let plan_row = |name: &str| name == crate::UPDATE_TASK_PLAN_TOOL;
    let existing_plan_rows = siblings.iter().filter(|(_, name)| plan_row(name)).count();
    let added_plan_rows = entries
        .iter()
        .filter(|entry| plan_row(&entry.call.name))
        .count();
    let over_budget = existing_plan_rows.saturating_add(added_plan_rows)
        > MAX_SANDBOX_TASK_PLAN_CALLS
        || (siblings.len() - existing_plan_rows).saturating_add(entries.len() - added_plan_rows)
            > MAX_SANDBOX_TOOL_CALLS;
    if !all_resolved || over_budget {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ParkSandboxToolCallOutcome::IdentityConflict);
    }
    let mut any_live = false;
    for (ordinal, entry) in entries.iter().enumerate() {
        let SandboxToolCallParkEntry { call, resolution } = entry;
        let status = resolution
            .as_ref()
            .map_or(SandboxToolCallStatus::Accepted, |resolution| {
                resolution_status(resolution)
            });
        any_live |= resolution.is_none();
        let (error_code, error_detail) = resolution.as_ref().map_or((None, None), resolution_error);
        entities::sandbox_tool_call::ActiveModel {
            id: Set(call.id.0),
            agent_run_id: Set(agent_run_id.0),
            chat_id: Set(call.chat_id.0),
            agent_run_depth: Set(1),
            provider_id: Set(call.provider_id.clone()),
            name: Set(call.name.clone()),
            arguments: Set(call.arguments.clone()),
            status: Set(status.as_str().into()),
            park_lease_token: Set(lease_token),
            park_attempt_count: Set(run.attempt_count),
            park_claim_count: Set(run.claim_count),
            batch_ordinal: Set(ordinal_of(ordinal)?),
            executor_lease_token: Set(None),
            executor_lease_expires_at: Set(None),
            retry_at: Set(None),
            // No executor ever held a call that arrives already resolved, so
            // its own park lease stands in as the producing authority — the
            // convention terminalization on the host side already uses.
            resolution_lease_token: Set(resolution.as_ref().map(|_| lease_token)),
            result: Set(resolution
                .as_ref()
                .map(|resolution| resolution.result().to_owned())),
            error_code: Set(error_code),
            error_detail: Set(error_detail),
            created_at: Set(now),
            resolved_at: Set(resolution.as_ref().map(|_| now)),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    }
    let mut update = entities::agent_run::Entity::update_many();
    if any_live {
        update = update.col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Waiting.as_str()),
        );
    } else {
        // Nothing will resolve these rows later, so parking into `waiting`
        // would strand the run there forever. It goes straight back on the
        // schedule to consume the receipts it just committed.
        update = update
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
            );
    }
    let updated = update
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
    let mut calls = Vec::with_capacity(entries.len());
    for entry in entries {
        let call = entities::sandbox_tool_call::Entity::find_by_id(entry.call.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("sandbox tool checkpoint disappeared".into()))?;
        calls.push(call_from_model(call)?);
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(ParkSandboxToolCallOutcome::Parked {
        run: agent_run_from_model(run)?,
        calls,
    })
}

fn ordinal_of(index: usize) -> Result<i16> {
    i16::try_from(index)
        .map_err(|_| AgentError::Store("sandbox tool checkpoint batch is too large".into()))
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
    if exec_predecessor_is_live_on(&transaction, &existing).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimDelegatedFileReadOutcome::Unavailable);
    }
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
    let due_retry = existing.status == SandboxToolCallStatus::RetryWait.as_str()
        && existing.retry_at.is_some_and(|retry_at| retry_at <= now);
    if existing.status != SandboxToolCallStatus::Accepted.as_str() && !due_retry {
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
    // Not claimable *yet* rather than unusable: an earlier exec in the same
    // step still owns the workspace. The lane rescans on its idle loop and
    // picks this one up once its predecessor lands terminal.
    if exec_predecessor_is_live_on(&transaction, &existing).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimSandboxToolCallOutcome::Unavailable);
    }
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
    resolve_sandbox_tool_call_with_plan(store, id, lease_token, resolution, None).await
}

/// Resolve one `update_task_plan` checkpoint and commit the plan it recorded in
/// the same transaction.
///
/// The plan write and the receipt the model reads back must land together. If
/// they were two transactions, an interrupted executor could hand the run
/// "Task plan updated" for a plan nobody stored, or store a plan whose
/// checkpoint never settled — and the run replays its whole chain on every
/// claim, so it would keep reading the lie.
pub(in crate::db) async fn resolve_sandbox_task_plan_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    steps: &[crate::TaskPlanStep],
    resolution: &ToolCallResolution,
) -> Result<ResolveSandboxToolCallOutcome> {
    resolve_sandbox_tool_call_with_plan(store, id, lease_token, resolution, Some(steps)).await
}

async fn resolve_sandbox_tool_call_with_plan(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    resolution: &ToolCallResolution,
    plan_steps: Option<&[crate::TaskPlanStep]>,
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
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::NotFound);
    };
    if call.result.is_some() {
        let exact = call.resolution_lease_token == Some(lease_token)
            && resolution_matches(&call, resolution);
        transaction.commit().await.map_err(store_err)?;
        return Ok(if exact {
            ResolveSandboxToolCallOutcome::Existing
        } else {
            ResolveSandboxToolCallOutcome::AlreadyTerminal
        });
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
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    };
    if run.chat_id != call.chat_id || run.status != AgentRunStatus::Waiting.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
    }
    if let Some(steps) = plan_steps {
        // The name is proven from the durable row, not from what the executor
        // lane believed it claimed: only an `update_task_plan` checkpoint may
        // move a run's plan.
        if call.name != crate::UPDATE_TASK_PLAN_TOOL {
            transaction.commit().await.map_err(store_err)?;
            return Ok(ResolveSandboxToolCallOutcome::LeaseLost);
        }
        super::task_plan::upsert_for_agent_run_on(
            &transaction,
            AgentRunId(call.agent_run_id),
            id,
            steps,
            now,
        )
        .await?;
    }
    let status = resolution_status(resolution);
    let (error_code, error_detail) = resolution_error(resolution);
    let mut call_active: entities::sandbox_tool_call::ActiveModel = call.into();
    call_active.status = Set(status.as_str().into());
    call_active.executor_lease_token = Set(None);
    call_active.executor_lease_expires_at = Set(None);
    call_active.resolution_lease_token = Set(Some(lease_token));
    call_active.result = Set(Some(resolution.result().to_owned()));
    call_active.error_code = Set(error_code);
    call_active.error_detail = Set(error_detail);
    call_active.resolved_at = Set(Some(now));
    call_active.update(&transaction).await.map_err(store_err)?;
    // The call's own transition commits either way: a step whose other calls
    // are still running simply has nothing to resume yet.
    resume_waiting_run_if_batch_settled_on(&transaction, AgentRunId(run.id), now).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ResolveSandboxToolCallOutcome::Resolved)
}

/// Park a claimed sandbox tool call for its single bounded retry (#1224).
///
/// The call moves to `retry_wait` under the exact live executor lease, keeps
/// no lease of its own, and becomes claimable again once `retry_at` passes.
/// Its waiting sandbox run is untouched — no receipt exists yet, so nothing
/// resumes. `retry_at` doubles as the spent-retry marker: it is set exactly
/// once, and a call that already carries one cannot be parked again.
pub(in crate::db) async fn retry_sandbox_tool_call(
    store: &DbStore,
    id: CallId,
    lease_token: uuid::Uuid,
    delay: Duration,
) -> Result<RetrySandboxToolCallOutcome> {
    if id.0.is_nil() || lease_token.is_nil() || delay < Duration::zero() {
        return Err(AgentError::Store(
            "invalid sandbox tool retry request".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RetrySandboxToolCallOutcome::LeaseLost);
    };
    if call.result.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RetrySandboxToolCallOutcome::LeaseLost);
    }
    if call.status != SandboxToolCallStatus::Claimed.as_str()
        || call.executor_lease_token != Some(lease_token)
        || call
            .executor_lease_expires_at
            .is_none_or(|expiry| expiry <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RetrySandboxToolCallOutcome::LeaseLost);
    }
    if call.retry_at.is_some() {
        // The single retry is already spent. Executors decide terminal versus
        // retry from the claimed call's own retry marker, so reaching this is
        // an invariant breach worth surfacing, not a schedulable state.
        return Err(AgentError::Store(
            "sandbox tool call retry budget exhausted".into(),
        ));
    }
    let Some(run) = find_by_id_on(&transaction, AgentRunId(call.agent_run_id)).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RetrySandboxToolCallOutcome::LeaseLost);
    };
    if run.chat_id != call.chat_id
        || run.status != AgentRunStatus::Waiting.as_str()
        || run.deadline_at.is_none_or(|deadline| deadline <= now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RetrySandboxToolCallOutcome::LeaseLost);
    }
    let retry_at = now
        .checked_add_signed(delay)
        .ok_or_else(|| AgentError::Store("sandbox tool retry overflows database time".into()))?;
    let mut active: entities::sandbox_tool_call::ActiveModel = call.into();
    active.status = Set(SandboxToolCallStatus::RetryWait.as_str().into());
    active.executor_lease_token = Set(None);
    active.executor_lease_expires_at = Set(None);
    active.retry_at = Set(Some(retry_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(RetrySandboxToolCallOutcome::Scheduled)
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
    if let Some(resolved) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|call| call.result.is_some())
    {
        let exact = resolved.resolution_lease_token == Some(lease_token)
            && resolution_matches(&resolved, resolution);
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
    if call.result.is_some() {
        let exact = call.resolution_lease_token == Some(lease_token)
            && resolution_matches(&call, resolution);
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
    let mut call_active: entities::sandbox_tool_call::ActiveModel = call.into();
    call_active.status = Set(status.as_str().into());
    call_active.executor_lease_token = Set(None);
    call_active.executor_lease_expires_at = Set(None);
    call_active.resolution_lease_token = Set(Some(lease_token));
    call_active.result = Set(Some(resolution.result().to_owned()));
    call_active.error_code = Set(error_code);
    call_active.error_detail = Set(error_detail);
    call_active.resolved_at = Set(Some(now));
    call_active.update(&transaction).await.map_err(store_err)?;
    // The call's own transition commits either way: a step whose other calls
    // are still running simply has nothing to resume yet.
    resume_waiting_run_if_batch_settled_on(&transaction, AgentRunId(run.id), now).await?;
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
    let Some(call) = entities::sandbox_tool_call::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    receipt_from_model(call)
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
        // Within one parked step, the ordinal is the order the model emitted
        // the calls in, and the order its transcript replays them in.
        .order_by_asc(entities::sandbox_tool_call::Column::BatchOrdinal)
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
                )
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::RetryWait.as_str())
                        .and(entities::sandbox_tool_call::Column::RetryAt.lte(now)),
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
                )
                .add(
                    entities::sandbox_tool_call::Column::Status
                        .eq(SandboxToolCallStatus::RetryWait.as_str())
                        .and(entities::sandbox_tool_call::Column::RetryAt.lte(now)),
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
/// fences its waiting sandbox. A late executor cannot overwrite this outcome.
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
            SandboxToolCallStatus::RetryWait.as_str(),
        ]))
        .all(conn)
        .await
        .map_err(store_err)?;
    // A step parks its calls together, so a run being fenced can have several
    // live at once; every one of them gets the same terminal answer here.
    for call in calls {
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
    let status = if error_code == "cancelled" {
        SandboxToolCallStatus::Cancelled
    } else {
        SandboxToolCallStatus::Failed
    };
    let mut active: entities::sandbox_tool_call::ActiveModel = call.into();
    active.status = Set(status.as_str().into());
    active.executor_lease_token = Set(None);
    active.executor_lease_expires_at = Set(None);
    active.resolution_lease_token = Set(Some(executor_lease_token));
    active.result = Set(Some(result));
    active.error_code = Set((error_code != "cancelled").then(|| error_code.to_owned()));
    active.error_detail = Set(None);
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
    resume_waiting_run_if_batch_settled_on(conn, AgentRunId(call.agent_run_id), now).await?;
    Ok(())
}

/// Put a waiting sandbox run back on the schedule once its whole parked step
/// has settled, and report whether it moved.
///
/// A step's calls resolve independently and in any order, so the first receipt
/// to land is usually not the one that finishes the step. The run only resumes
/// when no live call is left — the park gate keeps at most one step outstanding
/// at a time, so counting the run's own non-terminal rows is the whole test.
/// A caller that has just committed a receipt keeps it either way: not being
/// ready to resume is the ordinary case, not a lost lease.
async fn resume_waiting_run_if_batch_settled_on<C>(
    conn: &C,
    agent_run_id: AgentRunId,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let live = entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(agent_run_id.0))
        .filter(entities::sandbox_tool_call::Column::Status.is_in([
            SandboxToolCallStatus::Accepted.as_str(),
            SandboxToolCallStatus::Claimed.as_str(),
            SandboxToolCallStatus::RetryWait.as_str(),
        ]))
        .count(conn)
        .await
        .map_err(store_err)?;
    if live > 0 {
        return Ok(false);
    }
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
        .filter(entities::agent_run::Column::Id.eq(agent_run_id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Waiting.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(resumed.rows_affected == 1)
}

/// Whether an earlier `exec` in this call's own parked step is still live.
///
/// Only execs gate each other, and only within one step: they share the run's
/// single workspace, and the model emitted them expecting each to see the
/// previous one's effects.
async fn exec_predecessor_is_live_on<C>(
    conn: &C,
    call: &entities::sandbox_tool_call::Model,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if sandbox_call_is_parallel_eligible(&call.name) {
        return Ok(false);
    }
    let pending = entities::sandbox_tool_call::Entity::find()
        .filter(entities::sandbox_tool_call::Column::AgentRunId.eq(call.agent_run_id))
        .filter(entities::sandbox_tool_call::Column::ParkAttemptCount.eq(call.park_attempt_count))
        .filter(entities::sandbox_tool_call::Column::ParkClaimCount.eq(call.park_claim_count))
        .filter(entities::sandbox_tool_call::Column::BatchOrdinal.lt(call.batch_ordinal))
        .filter(entities::sandbox_tool_call::Column::Name.eq(SANDBOX_EXEC_TOOL))
        .filter(entities::sandbox_tool_call::Column::Status.is_in([
            SandboxToolCallStatus::Accepted.as_str(),
            SandboxToolCallStatus::Claimed.as_str(),
            SandboxToolCallStatus::RetryWait.as_str(),
        ]))
        .count(conn)
        .await
        .map_err(store_err)?;
    Ok(pending > 0)
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
    let Some(run) = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(agent_run_id.0))
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if run.admitted_at.is_none()
        || run.chat_id != chat_id.0
        || run.parent_id.is_none_or(|id| id.is_nil())
        || run.origin_turn_id.is_none_or(|id| id.is_nil())
        || run.spawn_call_id.is_none_or(|spawn_call_id| {
            AgentRunId::sandbox_for_spawn_call(CallId(spawn_call_id)) != agent_run_id
        })
    {
        return Ok(None);
    }
    let (Some(root_uuid), Some(relative_path)) =
        (run.delegated_root_id, run.delegated_relative_path)
    else {
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

/// Whether a call's recorded outcome is the one this resolution would write.
///
/// A resolved call carries its whole outcome inline, so a replayed resolution
/// is the same one exactly when every recorded field already agrees.
fn resolution_matches(
    call: &entities::sandbox_tool_call::Model,
    resolution: &ToolCallResolution,
) -> bool {
    let (error_code, error_detail) = resolution_error(resolution);
    call.status == resolution_status(resolution).as_str()
        && call.result.as_deref() == Some(resolution.result())
        && call.error_code == error_code
        && call.error_detail == error_detail
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
        batch_ordinal: model.batch_ordinal,
        executor_lease_token: model.executor_lease_token,
        executor_lease_expires_at: model.executor_lease_expires_at,
        retry_at: model.retry_at,
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn receipt_from_model(
    model: entities::sandbox_tool_call::Model,
) -> Result<Option<SandboxToolCallReceipt>> {
    let (Some(executor_lease_token), Some(result), Some(resolved_at)) = (
        model.resolution_lease_token,
        model.result,
        model.resolved_at,
    ) else {
        return Ok(None);
    };
    Ok(Some(SandboxToolCallReceipt {
        call_id: CallId(model.id),
        executor_lease_token,
        status: status_from_db(&model.status)?,
        result,
        error_code: model.error_code,
        error_detail: model.error_detail,
        resolved_at,
    }))
}

fn status_from_db(value: &str) -> Result<SandboxToolCallStatus> {
    match value {
        "accepted" => Ok(SandboxToolCallStatus::Accepted),
        "claimed" => Ok(SandboxToolCallStatus::Claimed),
        "retry_wait" => Ok(SandboxToolCallStatus::RetryWait),
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
