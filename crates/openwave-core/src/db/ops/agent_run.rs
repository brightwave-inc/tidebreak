use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId, MessageId, TurnId};
use crate::model::{
    AgentRun, AgentRunExecution, AgentRunInboxEntry, AgentRunInboxStatus, AgentRunResult,
    AgentRunStatus, TurnRun,
};
use crate::storage::{
    AcceptAgentRunOutcome, AcceptSandboxAgentRunAndParkTurnOutcome, ClaimAgentRunInboxOutcome,
    ConsumeAgentRunInboxAndResumeTurnOutcome, ConsumeAgentRunInboxOutcome, FailAgentRunOutcome,
    FinishAgentRunCancellationOutcome, ParkTurnForAgentRunInboxOutcome,
    RequestAgentRunCancellationOutcome, SubmitAgentRunResultOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::{
    acquire_chat_write_lock, acquire_turn_write_lock,
    conversation::{
        next_message_seq_on, reserve_message_identity_on, MESSAGE_IDENTITY_OWNER_MESSAGE,
    },
    turn::{
        canonical_db_timestamp, park_turn_for_agent_run_inbox_on,
        validate_agent_run_inbox_park_request,
    },
};

pub(in crate::db) async fn insert_foreground_agent_run_on<C>(
    conn: &C,
    chat_id: ChatId,
    created_at: chrono::DateTime<Utc>,
) -> Result<AgentRunId>
where
    C: sea_orm::ConnectionTrait,
{
    let id = AgentRunId::foreground_for_chat(chat_id);
    let created_at = canonical_db_timestamp(created_at)?;
    entities::agent_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(None),
        parent_depth: Set(None),
        spawn_call_id: Set(None),
        execution: Set(AgentRunExecution::Foreground.as_str().into()),
        depth: Set(0),
        status: Set(AgentRunStatus::Active.as_str().into()),
        input: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(0),
        claim_count: Set(0),
        available_at: Set(created_at),
        deadline_at: Set(None),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(id)
}

pub(in crate::db) async fn find_foreground_agent_run_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<AgentRun>>
where
    C: sea_orm::ConnectionTrait,
{
    find_foreground_on(conn, chat_id)
        .await?
        .map(agent_run_from_model)
        .transpose()
}

pub(in crate::db) async fn accept_agent_run(
    store: &DbStore,
    id: AgentRunId,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    validate_request(id, parent_id, spawn_call_id, execution, input)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }

    if let Some(existing) = find_by_id_on(&transaction, id).await? {
        let outcome = existing_request_outcome(
            existing,
            chat_id,
            parent_id,
            spawn_call_id,
            execution,
            input,
        )?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if let Some(spawn_call_id) = spawn_call_id {
        if let Some(existing) = find_by_spawn_call_on(&transaction, spawn_call_id).await? {
            let outcome = existing_request_outcome(
                existing,
                chat_id,
                parent_id,
                Some(spawn_call_id),
                execution,
                input,
            )?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(outcome);
        }
    }

    let (depth, status) = match execution {
        AgentRunExecution::Foreground => {
            if let Some(existing) = entities::agent_run::Entity::find()
                .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
                .filter(
                    entities::agent_run::Column::Execution
                        .eq(AgentRunExecution::Foreground.as_str()),
                )
                .one(&transaction)
                .await
                .map_err(store_err)?
            {
                let existing = agent_run_from_model(existing)?;
                transaction.commit().await.map_err(store_err)?;
                return Ok(AcceptAgentRunOutcome::ForegroundExists(existing));
            }
            (0_i16, AgentRunStatus::Active)
        }
        AgentRunExecution::Sandbox => {
            let Some(parent_id) = parent_id else {
                unreachable!("validated sandbox parent")
            };
            let parent = find_by_id_on(&transaction, parent_id).await?;
            let available = parent.is_some_and(|parent| {
                parent.chat_id == chat_id.0
                    && parent.parent_id.is_none()
                    && parent.depth == 0
                    && parent.execution == AgentRunExecution::Foreground.as_str()
                    && parent.status == AgentRunStatus::Active.as_str()
            });
            if !available {
                transaction.commit().await.map_err(store_err)?;
                return Ok(AcceptAgentRunOutcome::ParentUnavailable);
            }
            (i16::from(AgentRun::MAX_DEPTH), AgentRunStatus::Queued)
        }
    };

    let now = database_now(&transaction).await?;
    let model = entities::agent_run::ActiveModel {
        id: Set(id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(parent_id.map(|parent| parent.0)),
        parent_depth: Set(parent_id.map(|_| 0)),
        spawn_call_id: Set(spawn_call_id.map(|call| call.0)),
        execution: Set(execution.as_str().into()),
        depth: Set(depth),
        status: Set(status.as_str().into()),
        input: Set(input.map(ToOwned::to_owned)),
        attempt_count: Set(0),
        max_attempts: Set(match execution {
            AgentRunExecution::Foreground => 0,
            AgentRunExecution::Sandbox => AgentRun::DEFAULT_MAX_ATTEMPTS,
        }),
        claim_count: Set(0),
        available_at: Set(now),
        deadline_at: Set(match execution {
            AgentRunExecution::Foreground => None,
            AgentRunExecution::Sandbox => Some(now + AgentRun::DEFAULT_MAX_DURATION),
        }),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let model = match model.insert(&transaction).await {
        Ok(model) => model,
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(existing) = find_by_id_on(&store.conn, id).await? {
                return existing_request_outcome(
                    existing,
                    chat_id,
                    parent_id,
                    spawn_call_id,
                    execution,
                    input,
                );
            }
            if let Some(spawn_call_id) = spawn_call_id {
                if let Some(existing) = find_by_spawn_call_on(&store.conn, spawn_call_id).await? {
                    return existing_request_outcome(
                        existing,
                        chat_id,
                        parent_id,
                        Some(spawn_call_id),
                        execution,
                        input,
                    );
                }
            }
            if execution == AgentRunExecution::Foreground {
                if let Some(existing) = find_foreground_on(&store.conn, chat_id).await? {
                    return Ok(AcceptAgentRunOutcome::ForegroundExists(
                        agent_run_from_model(existing)?,
                    ));
                }
            }
            return Err(store_err(error));
        }
    };
    let run = agent_run_from_model(model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptAgentRunOutcome::Accepted(run))
}

/// Accept one sandbox child and park its exact foreground turn in the same
/// transaction. The sandbox parent comes from the locked turn, which binds
/// child hierarchy and checkpoint scope without trusting a caller-supplied
/// parent id.
pub(in crate::db) async fn accept_sandbox_agent_run_and_park_turn(
    store: &DbStore,
    child_run_id: AgentRunId,
    turn_id: TurnId,
    spawn_call_id: CallId,
    input: &str,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: crate::TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> Result<Option<AcceptSandboxAgentRunAndParkTurnOutcome>> {
    validate_agent_run_inbox_park_request(turn_id, child_run_id, lease_token, progress)?;
    let now = canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    let chat_id = ChatId(turn.chat_id);
    let parent_id = AgentRunId(turn.agent_run_id);
    validate_request(
        child_run_id,
        Some(parent_id),
        Some(spawn_call_id),
        AgentRunExecution::Sandbox,
        Some(input),
    )?;

    let existing = match find_by_id_on(&transaction, child_run_id).await? {
        Some(existing) => Some(existing),
        None => find_by_spawn_call_on(&transaction, spawn_call_id).await?,
    };
    if let Some(existing) = existing {
        let canonical_child_run_id = AgentRunId(existing.id);
        let exact = matches!(
            existing_request_outcome(
                existing.clone(),
                chat_id,
                Some(parent_id),
                Some(spawn_call_id),
                AgentRunExecution::Sandbox,
                Some(input),
            )?,
            AcceptAgentRunOutcome::Existing(_)
        );
        if !exact {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
        // A child accepted outside this operation must never be retrofitted
        // into a foreground checkpoint. A committed checkpoint is the only
        // durable proof that this is an exact retry of the combined action.
        let atomic_wait =
            entities::turn_agent_run_wait::Entity::find_by_id(canonical_child_run_id.0)
                .one(&transaction)
                .await
                .map_err(store_err)?;
        if !atomic_wait.is_some_and(|wait| wait.atomic_admission) {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
        let outcome = park_turn_for_agent_run_inbox_on(
            &transaction,
            turn_id,
            canonical_child_run_id,
            lease_token,
            expected_steer_revision,
            progress,
            now,
            true,
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(match outcome {
            Some(ParkTurnForAgentRunInboxOutcome::Existing { turn, wait }) => {
                AcceptSandboxAgentRunAndParkTurnOutcome::Existing {
                    child: agent_run_from_model(existing)?,
                    turn,
                    wait,
                }
            }
            _ => AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
        }));
    }

    let parent = find_by_id_on(&transaction, parent_id).await?;
    let parent_available = parent.is_some_and(|parent| {
        parent.chat_id == chat_id.0
            && parent.parent_id.is_none()
            && parent.depth == 0
            && parent.execution == AgentRunExecution::Foreground.as_str()
            && parent.status == AgentRunStatus::Active.as_str()
    });
    if !parent_available {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(
            AcceptSandboxAgentRunAndParkTurnOutcome::ParentUnavailable,
        ));
    }

    let created_at = database_now(&transaction).await?;
    let model = entities::agent_run::ActiveModel {
        id: Set(child_run_id.0),
        chat_id: Set(chat_id.0),
        parent_id: Set(Some(parent_id.0)),
        parent_depth: Set(Some(0)),
        spawn_call_id: Set(Some(spawn_call_id.0)),
        execution: Set(AgentRunExecution::Sandbox.as_str().into()),
        depth: Set(i16::from(AgentRun::MAX_DEPTH)),
        status: Set(AgentRunStatus::Queued.as_str().into()),
        input: Set(Some(input.into())),
        attempt_count: Set(0),
        max_attempts: Set(AgentRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        available_at: Set(created_at),
        deadline_at: Set(Some(created_at + AgentRun::DEFAULT_MAX_DURATION)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        started_at: Set(None),
        finished_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let child = agent_run_from_model(model)?;

    let outcome = park_turn_for_agent_run_inbox_on(
        &transaction,
        turn_id,
        child_run_id,
        lease_token,
        expected_steer_revision,
        progress,
        now,
        true,
    )
    .await?;
    let Some(outcome) = outcome else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    let outcome = match outcome {
        ParkTurnForAgentRunInboxOutcome::Parked { turn, wait } => {
            AcceptSandboxAgentRunAndParkTurnOutcome::Parked { child, turn, wait }
        }
        ParkTurnForAgentRunInboxOutcome::SteerPending(turn) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(AcceptSandboxAgentRunAndParkTurnOutcome::SteerPending(
                turn,
            )));
        }
        ParkTurnForAgentRunInboxOutcome::OutputSuperseded(turn) => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::OutputSuperseded(turn),
            ));
        }
        ParkTurnForAgentRunInboxOutcome::Existing { .. }
        | ParkTurnForAgentRunInboxOutcome::IdentityConflict => {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(Some(
                AcceptSandboxAgentRunAndParkTurnOutcome::IdentityConflict,
            ));
        }
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

pub(in crate::db) async fn get_agent_run(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<AgentRun>> {
    find_by_id_on(&store.conn, id)
        .await?
        .map(agent_run_from_model)
        .transpose()
}

pub(in crate::db) async fn claim_agent_run(
    store: &DbStore,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
    max_running_global: u32,
    max_running_per_chat: u32,
) -> Result<Option<AgentRun>> {
    validate_claim_request(
        lease_token,
        lease_duration,
        max_running_global,
        max_running_per_chat,
    )?;

    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_agent_run_claim_lock(&transaction).await?;
        let now = database_now(&transaction).await?;
        let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
            AgentError::Store(
                "agent-run lease duration overflows the database timestamp range".into(),
            )
        })?;

        if let Some(receipt) = entities::agent_run_claim::Entity::find_by_id(lease_token)
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let Some(agent_run_id) = receipt.agent_run_id else {
                transaction.commit().await.map_err(store_err)?;
                return Ok(None);
            };
            let existing = find_by_id_on(&transaction, AgentRunId(agent_run_id))
                .await?
                .filter(|run| {
                    run.execution == AgentRunExecution::Sandbox.as_str()
                        && matches!(run.status.as_str(), "running" | "cancelling")
                        && Some(run.attempt_count) == receipt.attempt_count
                        && Some(run.claim_count) == receipt.claim_count
                        && run.lease_token == Some(lease_token)
                        && run.lease_expires_at.is_some_and(|expiry| expiry > now)
                        && run.deadline_at.is_some_and(|deadline| deadline > now)
                })
                .map(agent_run_from_model)
                .transpose()?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(existing);
        }

        if let Some(expired) = find_expired_deadline_on(&transaction, now).await? {
            let updated = fail_candidate_on(
                &transaction,
                &expired,
                now,
                "deadline_exceeded",
                "sandbox agent exceeded its wall-clock deadline",
            )
            .await?;
            if !updated {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }

        // Reaping does not consume a scheduler slot. Do it before capacity
        // admission so an expired cancellation or exhausted worker cannot be
        // stranded indefinitely behind unrelated live work.
        if let Some(expired) = find_expired_lease_reaper_on(&transaction, now).await? {
            let (error_code, error_detail) =
                if expired.status == AgentRunStatus::Cancelling.as_str() {
                    (
                        "cancelled",
                        "sandbox agent lease expired while cancellation was pending",
                    )
                } else {
                    (
                        "lease_expired",
                        "sandbox agent exhausted its attempt budget after lease expiry",
                    )
                };
            let updated =
                fail_candidate_on(&transaction, &expired, now, error_code, error_detail).await?;
            if !updated {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            transaction.commit().await.map_err(store_err)?;
            continue;
        }

        let live = entities::agent_run::Entity::find()
            .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Sandbox.as_str()))
            .filter(entities::agent_run::Column::Status.is_in([
                AgentRunStatus::Running.as_str(),
                AgentRunStatus::Cancelling.as_str(),
            ]))
            .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
            .all(&transaction)
            .await
            .map_err(store_err)?;
        if live.len() >= max_running_global as usize {
            record_empty_claim_scan_on(&transaction, lease_token, now).await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        let mut live_by_chat = std::collections::HashMap::<uuid::Uuid, usize>::new();
        for run in live {
            *live_by_chat.entry(run.chat_id).or_default() += 1;
        }

        let Some(candidate) =
            find_claim_candidate_on(&transaction, now, max_running_per_chat, &live_by_chat).await?
        else {
            record_empty_claim_scan_on(&transaction, lease_token, now).await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        };

        let attempt_count = candidate.attempt_count.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("agent run {} attempt count overflow", candidate.id))
        })?;
        let claim_count = candidate.claim_count.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("agent run {} claim count overflow", candidate.id))
        })?;
        let effective_lease_expires_at = candidate
            .deadline_at
            .ok_or_else(|| AgentError::Store("sandbox agent run is missing its deadline".into()))?
            .min(lease_expires_at);
        entities::agent_run_claim::ActiveModel {
            token: Set(lease_token),
            agent_run_id: Set(Some(candidate.id)),
            attempt_count: Set(Some(attempt_count)),
            claim_count: Set(Some(claim_count)),
            claimed_at: Set(now),
            lease_expires_at: Set(Some(effective_lease_expires_at)),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;

        let reclaiming = candidate.status == AgentRunStatus::Running.as_str();
        let claim = entities::agent_run::Entity::update_many()
            .col_expr(
                entities::agent_run::Column::Status,
                sea_orm::sea_query::Expr::value(AgentRunStatus::Running.as_str()),
            )
            .col_expr(
                entities::agent_run::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(attempt_count),
            )
            .col_expr(
                entities::agent_run::Column::ClaimCount,
                sea_orm::sea_query::Expr::value(claim_count),
            )
            .col_expr(
                entities::agent_run::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::agent_run::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(effective_lease_expires_at)),
            )
            .col_expr(
                entities::agent_run::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(candidate.started_at.unwrap_or(now))),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::agent_run::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::agent_run::Column::Id.eq(candidate.id))
            .filter(entities::agent_run::Column::Status.eq(&candidate.status))
            .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
            .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
            .filter(entities::agent_run::Column::UpdatedAt.lte(now))
            .filter(entities::agent_run::Column::DeadlineAt.gt(now));
        let claim = if reclaiming {
            claim
                .filter(entities::agent_run::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::agent_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        } else {
            claim
                .filter(entities::agent_run::Column::LeaseToken.is_null())
                .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
                .filter(entities::agent_run::Column::AvailableAt.lte(now))
        };
        let claimed = claim.exec(&transaction).await.map_err(store_err)?;
        if claimed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }
        let claimed = find_by_id_on(&transaction, AgentRunId(candidate.id))
            .await?
            .ok_or_else(|| AgentError::Store("claimed agent run disappeared".into()))?;
        let claimed = agent_run_from_model(claimed)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(claimed));
    }
}

async fn record_empty_claim_scan_on<C>(
    conn: &C,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run_claim::ActiveModel {
        token: Set(lease_token),
        agent_run_id: Set(None),
        attempt_count: Set(None),
        claim_count: Set(None),
        claimed_at: Set(now),
        lease_expires_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn heartbeat_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<bool> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run heartbeat requires a non-nil token and positive duration".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Serialize lease extension with claim, cancellation, and terminal
    // resolution. Otherwise a racing heartbeat could make a one-shot durable
    // cancellation request lose its compare-and-swap.
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store("agent-run lease duration overflows the database timestamp range".into())
    })?;
    let heartbeat = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Sandbox.as_str()))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::LeaseExpiresAt.lt(lease_expires_at))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gte(lease_expires_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(heartbeat.rows_affected == 1)
}

pub(in crate::db) async fn request_agent_run_cancellation(
    store: &DbStore,
    id: AgentRunId,
) -> Result<Option<RequestAgentRunCancellationOutcome>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.execution != AgentRunExecution::Sandbox.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let status = agent_run_status_from_db(&run.status)?;
    let existing = match status {
        AgentRunStatus::Cancelling | AgentRunStatus::Cancelled => Some(
            RequestAgentRunCancellationOutcome::Existing(agent_run_from_model(run.clone())?),
        ),
        AgentRunStatus::Completed | AgentRunStatus::Failed => Some(
            RequestAgentRunCancellationOutcome::AlreadyTerminal(agent_run_from_model(run.clone())?),
        ),
        AgentRunStatus::Queued | AgentRunStatus::Waiting | AgentRunStatus::RetryWait => None,
        AgentRunStatus::Running | AgentRunStatus::Active => None,
    };
    if let Some(outcome) = existing {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }
    if run.updated_at > now {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let immediate = status != AgentRunStatus::Running
        || !run.lease_expires_at.is_some_and(|expiry| expiry > now)
        || !run.deadline_at.is_some_and(|deadline| deadline > now);
    let next_status = if immediate {
        AgentRunStatus::Cancelled
    } else {
        AgentRunStatus::Cancelling
    };
    let mut update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(next_status.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(&run.status))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now));
    if immediate {
        update = update
            .col_expr(
                entities::agent_run::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
            )
            .col_expr(
                entities::agent_run::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .col_expr(
                entities::agent_run::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::agent_run::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            );
    } else {
        update = update
            .filter(entities::agent_run::Column::LeaseToken.eq(run.lease_token))
            .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
            .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now));
    }
    let updated = update.exec(&transaction).await.map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = find_by_id_on(&transaction, id)
        .await?
        .ok_or_else(|| AgentError::Store(format!("cancelled agent run {id} disappeared")))
        .and_then(agent_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(if immediate {
        RequestAgentRunCancellationOutcome::Cancelled(updated)
    } else {
        RequestAgentRunCancellationOutcome::Requested(updated)
    }))
}

/// Fence-cascade a cancelled foreground checkpoint to its owned sandbox child.
///
/// This runs under the parent turn's chat/turn transaction.  It deliberately
/// uses the child row's exact lifecycle fields as a CAS rather than taking the
/// global sandbox scheduler lock: a concurrent claim either loses its queued
/// predicate or becomes `cancelling`, and can never produce a second result.
pub(in crate::db) async fn cancel_sandbox_child_for_parked_turn_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    chat_id: ChatId,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(child) = find_by_id_on(conn, child_run_id).await? else {
        return Err(AgentError::Store(format!(
            "waiting turn references missing sandbox child {child_run_id}"
        )));
    };
    if child.chat_id != chat_id.0
        || child.parent_id != Some(parent_run_id.0)
        || child.execution != AgentRunExecution::Sandbox.as_str()
        || child.depth != 1
    {
        return Err(AgentError::Store(format!(
            "waiting turn child {child_run_id} is not owned by its foreground parent"
        )));
    }
    let status = agent_run_status_from_db(&child.status)?;
    match status {
        AgentRunStatus::Cancelled | AgentRunStatus::Cancelling => return Ok(true),
        AgentRunStatus::Completed | AgentRunStatus::Failed => {
            let retired = entities::agent_run_inbox::Entity::update_many()
                .col_expr(
                    entities::agent_run_inbox::Column::Status,
                    sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Cancelled.as_str()),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
                .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
                .filter(
                    Condition::any()
                        .add(
                            entities::agent_run_inbox::Column::Status
                                .eq(AgentRunInboxStatus::Pending.as_str()),
                        )
                        .add(
                            entities::agent_run_inbox::Column::Status
                                .eq(AgentRunInboxStatus::Claimed.as_str()),
                        ),
                )
                .exec(conn)
                .await
                .map_err(store_err)?;
            if retired.rows_affected == 1 {
                return Ok(true);
            }
            let inbox = find_agent_run_inbox_on(conn, parent_run_id, child_run_id).await?;
            return match inbox
                .as_ref()
                .and_then(|entry| AgentRunInboxStatus::parse(&entry.status))
            {
                Some(AgentRunInboxStatus::Cancelled) => Ok(true),
                Some(AgentRunInboxStatus::Consumed) => Err(AgentError::Store(format!(
                    "waiting turn has already-consumed child delivery {child_run_id}"
                ))),
                Some(_) => Ok(false),
                None => Err(AgentError::Store(format!(
                    "terminal sandbox child {child_run_id} is missing its inbox delivery"
                ))),
            };
        }
        AgentRunStatus::Active => {
            return Err(AgentError::Store(format!(
                "waiting turn child {child_run_id} unexpectedly has foreground status"
            )));
        }
        AgentRunStatus::Queued
        | AgentRunStatus::Waiting
        | AgentRunStatus::RetryWait
        | AgentRunStatus::Running => {}
    }
    if child.updated_at > now {
        return Ok(false);
    }
    let immediate = status != AgentRunStatus::Running
        || !child.lease_expires_at.is_some_and(|expiry| expiry > now)
        || !child.deadline_at.is_some_and(|deadline| deadline > now);
    let next = if immediate {
        AgentRunStatus::Cancelled
    } else {
        AgentRunStatus::Cancelling
    };
    let mut update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(next.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(child_run_id.0))
        .filter(entities::agent_run::Column::Status.eq(status.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(child.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(child.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(child.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now));
    if immediate {
        update = update
            .col_expr(
                entities::agent_run::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
            )
            .col_expr(
                entities::agent_run::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .col_expr(
                entities::agent_run::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            );
    } else {
        update = update
            .filter(entities::agent_run::Column::LeaseToken.eq(child.lease_token))
            .filter(entities::agent_run::Column::LeaseExpiresAt.eq(child.lease_expires_at))
            .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now));
    }
    Ok(update.exec(conn).await.map_err(store_err)?.rows_affected == 1)
}

pub(in crate::db) async fn finish_agent_run_cancellation(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<FinishAgentRunCancellationOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "agent-run cancellation requires a non-nil lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if let Some(receipt) = entities::agent_run_cancellation::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        if receipt.lease_token != lease_token {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        let Some(run) = find_by_id_on(&transaction, id).await? else {
            return Err(AgentError::Store(format!(
                "agent-run cancellation receipt references missing run {id}"
            )));
        };
        if run.status != AgentRunStatus::Cancelled.as_str()
            || run.attempt_count != receipt.attempt_count
            || run.claim_count != receipt.claim_count
        {
            return Err(AgentError::Store(format!(
                "agent-run cancellation receipt does not match terminal run {id}"
            )));
        }
        let run = agent_run_from_model(run)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(FinishAgentRunCancellationOutcome::Existing(run)));
    }
    let Some(claim) = entities::agent_run_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.agent_run_id == Some(id.0))
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.execution != AgentRunExecution::Sandbox.as_str()
        || Some(run.attempt_count) != claim.attempt_count
        || Some(run.claim_count) != claim.claim_count
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if run.status != AgentRunStatus::Cancelling.as_str()
        || run.lease_token != Some(lease_token)
        || !run.lease_expires_at.is_some_and(|expiry| expiry > now)
        || !run.deadline_at.is_some_and(|deadline| deadline > now)
        || run.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    entities::agent_run_cancellation::ActiveModel {
        agent_run_id: Set(id.0),
        lease_token: Set(lease_token),
        attempt_count: Set(run.attempt_count),
        claim_count: Set(run.claim_count),
        cancelled_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Cancelling.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = find_by_id_on(&transaction, id)
        .await?
        .ok_or_else(|| AgentError::Store(format!("cancelled agent run {id} disappeared")))
        .and_then(agent_run_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(FinishAgentRunCancellationOutcome::Cancelled(updated)))
}

/// Resolve an exact live sandbox lease after a provider/executor failure.
///
/// Intermediate failures release only the current lease and remain replay-safe.
/// The final attempt also writes the same immutable parent inbox receipt used
/// by successful completion, but transitions the child to `failed`.
pub(in crate::db) async fn fail_agent_run(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    error_code: &str,
    error_detail: &str,
    retry_delay: chrono::Duration,
) -> Result<Option<FailAgentRunOutcome>> {
    if lease_token.is_nil()
        || error_code.is_empty()
        || error_code.chars().count() > AgentRun::MAX_ERROR_CODE_LEN
        || error_detail.chars().count() > AgentRun::MAX_ERROR_DETAIL_LEN
        || retry_delay <= chrono::Duration::zero()
    {
        return Err(AgentError::Store(
            "invalid sandbox agent failure resolution".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    // A completed final receipt lets an ambiguous retry recover exactly; a
    // later different terminal outcome never overwrites it.
    if let Some(result) = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let result = agent_run_result_from_model(result)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok((result.lease_token == lease_token
            && run.status == AgentRunStatus::Failed.as_str())
        .then_some(FailAgentRunOutcome::ExistingFailed(result)));
    }
    let valid = run.execution == AgentRunExecution::Sandbox.as_str()
        && run.status == AgentRunStatus::Running.as_str()
        && run.lease_token == Some(lease_token)
        && run.lease_expires_at.is_some_and(|expiry| expiry > now)
        && run.deadline_at.is_some_and(|deadline| deadline > now)
        && run.updated_at <= now;
    if !valid {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let terminal = run.attempt_count >= run.max_attempts;
    let available_at = now.checked_add_signed(retry_delay).ok_or_else(|| {
        AgentError::Store("agent-run retry delay overflows timestamp range".into())
    })?;
    let status = if terminal {
        AgentRunStatus::Failed
    } else {
        AgentRunStatus::RetryWait
    };
    let mut update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(status.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Some(error_code.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Some(error_detail.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at));
    if terminal {
        update = update.col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        );
    } else {
        update = update.col_expr(
            entities::agent_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(available_at),
        );
    }
    if update
        .exec(&transaction)
        .await
        .map_err(store_err)?
        .rows_affected
        != 1
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = find_by_id_on(&transaction, id)
        .await?
        .expect("updated agent run exists");
    if !terminal {
        let updated = agent_run_from_model(updated)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(FailAgentRunOutcome::RetryScheduled(updated)));
    }
    let Some(parent_id) = run.parent_id.map(AgentRunId) else {
        return Err(AgentError::Store("sandbox run missing parent".into()));
    };
    let text = format!("Sandbox task failed ({error_code}): {error_detail}");
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(id.0),
        lease_token: Set(lease_token),
        attempt_count: Set(run.attempt_count),
        claim_count: Set(run.claim_count),
        text: Set(text),
        submitted_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(id.0),
        parent_run_id: Set(parent_id.0),
        chat_id: Set(run.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(lease_token),
        result_attempt_count: Set(run.attempt_count),
        result_claim_count: Set(run.claim_count),
        status: Set(AgentRunInboxStatus::Pending.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let result = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("inserted result exists");
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(FailAgentRunOutcome::Failed(
        agent_run_result_from_model(result)?,
    )))
}

pub(in crate::db) async fn submit_agent_run_result(
    store: &DbStore,
    id: AgentRunId,
    lease_token: uuid::Uuid,
    text: &str,
) -> Result<Option<SubmitAgentRunResultOutcome>> {
    if lease_token.is_nil() || text.is_empty() || text.chars().count() > AgentRun::MAX_RESULT_LEN {
        return Err(AgentError::Store(format!(
            "agent-run result requires a non-nil lease token and 1..={} characters",
            AgentRun::MAX_RESULT_LEN
        )));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if let Some(result) = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let result = agent_run_result_from_model(result)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok((result.lease_token == lease_token && result.text == text)
            .then_some(SubmitAgentRunResultOutcome::Existing(result)));
    }
    let Some(run) = find_by_id_on(&transaction, id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if run.execution != AgentRunExecution::Sandbox.as_str()
        || run.status != AgentRunStatus::Running.as_str()
        || run.lease_token != Some(lease_token)
        || !run.lease_expires_at.is_some_and(|expiry| expiry > now)
        || !run.deadline_at.is_some_and(|deadline| deadline > now)
        || run.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(parent_id) = run.parent_id.map(AgentRunId) else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "sandbox agent run {id} is missing its parent"
        )));
    };
    let Some(parent) = find_by_id_on(&transaction, parent_id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "sandbox agent run {id} references missing parent {parent_id}"
        )));
    };
    if parent.chat_id != run.chat_id
        || parent.depth != 0
        || parent.execution != AgentRunExecution::Foreground.as_str()
        || parent.status != AgentRunStatus::Active.as_str()
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    entities::agent_run_result::ActiveModel {
        agent_run_id: Set(id.0),
        lease_token: Set(lease_token),
        attempt_count: Set(run.attempt_count),
        claim_count: Set(run.claim_count),
        text: Set(text.to_owned()),
        submitted_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    entities::agent_run_inbox::ActiveModel {
        child_run_id: Set(id.0),
        parent_run_id: Set(parent_id.0),
        chat_id: Set(run.chat_id),
        parent_depth: Set(0),
        result_lease_token: Set(lease_token),
        result_attempt_count: Set(run.attempt_count),
        result_claim_count: Set(run.claim_count),
        status: Set(AgentRunInboxStatus::Pending.as_str().into()),
        claim_count: Set(0),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        consumed_lease_token: Set(None),
        consumed_at: Set(None),
        delivered_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunStatus::Completed.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .filter(entities::agent_run::Column::Status.eq(AgentRunStatus::Running.as_str()))
        .filter(entities::agent_run::Column::AttemptCount.eq(run.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(run.claim_count))
        .filter(entities::agent_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run::Column::LeaseExpiresAt.eq(run.lease_expires_at))
        .filter(entities::agent_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::agent_run::Column::DeadlineAt.gt(now))
        .filter(entities::agent_run::Column::UpdatedAt.eq(run.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let result = entities::agent_run_result::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("completed agent run {id} is missing its result")))
        .and_then(agent_run_result_from_model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(SubmitAgentRunResultOutcome::Completed(result)))
}

pub(in crate::db) async fn list_agent_run_inbox(
    store: &DbStore,
    parent_run_id: AgentRunId,
) -> Result<Vec<AgentRunInboxEntry>> {
    let entries = entities::agent_run_inbox::Entity::find()
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .order_by_asc(entities::agent_run_inbox::Column::DeliveredAt)
        .order_by_asc(entities::agent_run_inbox::Column::ChildRunId)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut inbox = Vec::with_capacity(entries.len());
    for entry in entries {
        inbox.push(load_agent_run_inbox_entry_on(&store.conn, entry).await?);
    }
    Ok(inbox)
}

/// Find deliveries that need a foreground continuation attempt.
///
/// Selection is deliberately only a bounded recovery hint. The exact claim
/// below serializes ownership against every other worker and rechecks the
/// parent checkpoint with the database clock.
pub(in crate::db) async fn list_agent_run_inbox_candidates(
    store: &DbStore,
    limit: u64,
) -> Result<Vec<AgentRunInboxEntry>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let now = database_now(&store.conn).await?;
    // Do not let an old result for a completed or cancelled parent consume the
    // bounded recovery scan.  The worker still rechecks this relationship when
    // claiming, but filtering it here gives a live parked turn a fair chance to
    // run even if historical inbox receipts remain in the database.
    let waiting_checkpoint = sea_orm::sea_query::Query::select()
        .expr(sea_orm::sea_query::Expr::value(1))
        .from(entities::turn_agent_run_wait::Entity)
        .inner_join(
            entities::turn_run::Entity,
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::TurnId,
            ))
            .equals((entities::turn_run::Entity, entities::turn_run::Column::Id)),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::Status,
            ))
            .eq(crate::model::TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ClosedAt,
            ))
            .is_null(),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::Status,
            ))
            .eq(crate::model::TurnRunStatus::WaitingForAgentRun.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChildRunId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ChildRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ParentRunId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ParentRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChatId,
            ))
            .equals((
                entities::agent_run_inbox::Entity,
                entities::agent_run_inbox::Column::ChatId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::ChatId,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ChatId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::AgentRunId,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ParentRunId,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::AttemptCount,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::AttemptCount,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::ClaimCount,
            ))
            .equals((
                entities::turn_agent_run_wait::Entity,
                entities::turn_agent_run_wait::Column::ClaimCount,
            )),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::LeaseToken,
            ))
            .is_null(),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::turn_run::Entity,
                entities::turn_run::Column::LeaseExpiresAt,
            ))
            .is_null(),
        )
        .to_owned();
    let active_parents = sea_orm::sea_query::Query::select()
        .column((entities::agent_run::Entity, entities::agent_run::Column::Id))
        .from(entities::agent_run::Entity)
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::agent_run::Entity,
                entities::agent_run::Column::Status,
            ))
            .eq(AgentRunStatus::Active.as_str()),
        )
        .and_where(
            sea_orm::sea_query::Expr::col((
                entities::agent_run::Entity,
                entities::agent_run::Column::Execution,
            ))
            .eq(AgentRunExecution::Foreground.as_str()),
        )
        .to_owned();
    let entries = entities::agent_run_inbox::Entity::find()
        .filter(
            Condition::any()
                .add(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Pending.as_str()),
                )
                .add(
                    Condition::all()
                        .add(
                            entities::agent_run_inbox::Column::Status
                                .eq(AgentRunInboxStatus::Claimed.as_str()),
                        )
                        .add(entities::agent_run_inbox::Column::LeaseExpiresAt.lte(now)),
                ),
        )
        .filter(sea_orm::sea_query::Expr::exists(waiting_checkpoint))
        .filter(entities::agent_run_inbox::Column::ParentRunId.in_subquery(active_parents))
        .order_by_asc(entities::agent_run_inbox::Column::DeliveredAt)
        .order_by_asc(entities::agent_run_inbox::Column::ChildRunId)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut inbox = Vec::with_capacity(entries.len());
    for entry in entries {
        inbox.push(load_agent_run_inbox_entry_on(&store.conn, entry).await?);
    }
    Ok(inbox)
}

/// Claim one exact immutable delivery as the next parent continuation boundary.
///
/// The shared scheduler lock deliberately serializes this with sandbox claims:
/// a continuation must observe one database-clock ordering, and an expired
/// continuation owner must never race a replacement owner into consumption.
pub(in crate::db) async fn claim_agent_run_inbox_entry(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<Option<ClaimAgentRunInboxOutcome>> {
    validate_inbox_claim_request(lease_token, lease_duration)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    let lease_expires_at = now.checked_add_signed(lease_duration).ok_or_else(|| {
        AgentError::Store(
            "agent-run inbox lease duration overflows the database timestamp range".into(),
        )
    })?;
    if !parent_is_active_on(&transaction, parent_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(model) = find_agent_run_inbox_on(&transaction, parent_run_id, child_run_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    // A completed child may arrive before its foreground worker reaches the
    // checkpoint boundary. Keep that delivery pending until the exact waiting
    // turn exists: otherwise a continuation lease could expire or be consumed
    // before there is a durable parent state to wake.
    if !parent_turn_checkpoint_is_waiting_on(&transaction, parent_run_id, child_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let entry = load_agent_run_inbox_entry_on(&transaction, model.clone()).await?;
    match entry.status {
        AgentRunInboxStatus::Pending => {
            let updated = entities::agent_run_inbox::Entity::update_many()
                .col_expr(
                    entities::agent_run_inbox::Column::Status,
                    sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Claimed.as_str()),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::ClaimCount,
                    sea_orm::sea_query::Expr::value(1),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Some(lease_token)),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
                )
                .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
                .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
                .filter(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Pending.as_str()),
                )
                .filter(entities::agent_run_inbox::Column::ClaimCount.eq(0))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if updated.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            }
            let claimed = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
                .await?
                .ok_or_else(|| {
                    AgentError::Store("claimed agent-run inbox entry disappeared".into())
                })?;
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Claimed(claimed)))
        }
        AgentRunInboxStatus::Claimed
            if entry.lease_token == Some(lease_token)
                && entry
                    .lease_expires_at
                    .is_some_and(|expires_at| expires_at > now) =>
        {
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Existing(entry)))
        }
        AgentRunInboxStatus::Claimed
            if entry
                .lease_expires_at
                .is_some_and(|expires_at| expires_at <= now) =>
        {
            // A lease token is a capability for one continuation attempt, not
            // a retry identity. Once it expires, its former owner must not
            // revive itself into a new attempt; only a fresh token may reclaim
            // the immutable delivery.
            if entry.lease_token == Some(lease_token) {
                transaction.commit().await.map_err(store_err)?;
                return Ok(None);
            }
            let next_claim_count = entry
                .claim_count
                .checked_add(1)
                .ok_or_else(|| AgentError::Store("agent-run inbox claim count exhausted".into()))?;
            let updated = entities::agent_run_inbox::Entity::update_many()
                .col_expr(
                    entities::agent_run_inbox::Column::ClaimCount,
                    sea_orm::sea_query::Expr::value(next_claim_count),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Some(lease_token)),
                )
                .col_expr(
                    entities::agent_run_inbox::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
                )
                .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
                .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
                .filter(
                    entities::agent_run_inbox::Column::Status
                        .eq(AgentRunInboxStatus::Claimed.as_str()),
                )
                .filter(entities::agent_run_inbox::Column::ClaimCount.eq(entry.claim_count))
                .filter(entities::agent_run_inbox::Column::LeaseToken.eq(entry.lease_token))
                .filter(
                    entities::agent_run_inbox::Column::LeaseExpiresAt.eq(entry.lease_expires_at),
                )
                .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.lte(now))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if updated.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                return Ok(None);
            }
            let claimed = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
                .await?
                .ok_or_else(|| {
                    AgentError::Store("reclaimed agent-run inbox entry disappeared".into())
                })?;
            transaction.commit().await.map_err(store_err)?;
            Ok(Some(ClaimAgentRunInboxOutcome::Claimed(claimed)))
        }
        AgentRunInboxStatus::Claimed
        | AgentRunInboxStatus::Consumed
        | AgentRunInboxStatus::Cancelled => {
            transaction.commit().await.map_err(store_err)?;
            Ok(None)
        }
    }
}

/// Mark one exact inbox delivery consumed under its active continuation lease.
///
/// Consumption is the commit point for a future parent resume transition. The
/// resulting receipt is retained so a worker can recover an ambiguous commit
/// without re-consuming the child result.
pub(in crate::db) async fn consume_agent_run_inbox_entry(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<ConsumeAgentRunInboxOutcome>> {
    Ok(consume_agent_run_inbox_entry_and_resume_turn(
        store,
        parent_run_id,
        child_run_id,
        lease_token,
    )
    .await?
    .map(|outcome| match outcome {
        ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed { inbox, .. } => {
            ConsumeAgentRunInboxOutcome::Consumed(inbox)
        }
        ConsumeAgentRunInboxAndResumeTurnOutcome::Existing { inbox, .. } => {
            ConsumeAgentRunInboxOutcome::Existing(inbox)
        }
    }))
}

/// Consume one exact child result and queue its foreground turn to resume in
/// the same transaction. The turn's durable `resuming` state is the wake
/// signal; callers can restart after commit and use ordinary turn claiming.
pub(in crate::db) async fn consume_agent_run_inbox_entry_and_resume_turn(
    store: &DbStore,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
) -> Result<Option<ConsumeAgentRunInboxAndResumeTurnOutcome>> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "agent-run inbox consumption requires a non-nil lease token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_agent_run_claim_lock(&transaction).await?;
    let now = database_now(&transaction).await?;
    if !parent_is_active_on(&transaction, parent_run_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(model) = find_agent_run_inbox_on(&transaction, parent_run_id, child_run_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let entry = load_agent_run_inbox_entry_on(&transaction, model).await?;
    if entry.status == AgentRunInboxStatus::Consumed {
        let turn = if entry.consumed_lease_token == Some(lease_token) {
            super::turn::resume_turn_after_agent_run_inbox_consumption_on(
                &transaction,
                parent_run_id,
                child_run_id,
                now,
            )
            .await?
        } else {
            None
        };
        if let Some(turn) = turn.as_ref() {
            ensure_sandbox_result_message_on(&transaction, &entry, turn, now, false).await?;
        }
        transaction.commit().await.map_err(store_err)?;
        return Ok(
            turn.map(|turn| ConsumeAgentRunInboxAndResumeTurnOutcome::Existing {
                inbox: entry,
                turn,
            }),
        );
    }
    if entry.status != AgentRunInboxStatus::Claimed
        || entry.lease_token != Some(lease_token)
        || !entry
            .lease_expires_at
            .is_some_and(|expires_at| expires_at > now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(turn) = super::turn::resume_turn_after_agent_run_inbox_consumption_on(
        &transaction,
        parent_run_id,
        child_run_id,
        now,
    )
    .await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    ensure_sandbox_result_message_on(&transaction, &entry, &turn, now, true).await?;
    let updated = entities::agent_run_inbox::Entity::update_many()
        .col_expr(
            entities::agent_run_inbox::Column::Status,
            sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Consumed.as_str()),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run_inbox::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run_inbox::Column::ConsumedLeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::agent_run_inbox::Column::ConsumedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::agent_run_inbox::Column::ChildRunId.eq(child_run_id.0))
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .filter(entities::agent_run_inbox::Column::Status.eq(AgentRunInboxStatus::Claimed.as_str()))
        .filter(entities::agent_run_inbox::Column::LeaseToken.eq(lease_token))
        .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.eq(entry.lease_expires_at))
        .filter(entities::agent_run_inbox::Column::LeaseExpiresAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let inbox = load_agent_run_inbox_by_ids_on(&transaction, parent_run_id, child_run_id)
        .await?
        .ok_or_else(|| AgentError::Store("consumed agent-run inbox entry disappeared".into()))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(ConsumeAgentRunInboxAndResumeTurnOutcome::Resumed {
        inbox,
        turn,
    }))
}

fn validate_claim_request(
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
    max_running_global: u32,
    max_running_per_chat: u32,
) -> Result<()> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run claim requires a non-nil token and positive duration".into(),
        ));
    }
    if !(1..=AgentRun::MAX_CONCURRENCY_LIMIT).contains(&max_running_global)
        || !(1..=AgentRun::MAX_CONCURRENCY_LIMIT).contains(&max_running_per_chat)
        || max_running_per_chat > max_running_global
    {
        return Err(AgentError::Store(format!(
            "agent-run concurrency limits must satisfy 1 <= per-chat <= global <= {}",
            AgentRun::MAX_CONCURRENCY_LIMIT
        )));
    }
    Ok(())
}

fn validate_inbox_claim_request(
    lease_token: uuid::Uuid,
    lease_duration: chrono::Duration,
) -> Result<()> {
    if lease_token.is_nil() || lease_duration <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "agent-run inbox claim requires a non-nil token and positive duration".into(),
        ));
    }
    Ok(())
}

async fn database_now<C>(conn: &C) -> Result<chrono::DateTime<Utc>>
where
    C: sea_orm::ConnectionTrait,
{
    let backend = conn.get_database_backend();
    // PostgreSQL's CURRENT_TIMESTAMP is fixed at transaction start. Claimers
    // can wait on the scheduler lock, so use its statement-time clock there;
    // SQLite's CURRENT_TIMESTAMP already has the desired statement semantics.
    let clock_sql = match backend {
        DatabaseBackend::Postgres => "SELECT clock_timestamp() AS now",
        _ => "SELECT CURRENT_TIMESTAMP AS now",
    };
    let statement = Statement::from_string(backend, clock_sql);
    let row = conn
        .query_one(statement)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("database clock query returned no row".into()))?;
    let now = row
        .try_get::<chrono::DateTime<Utc>>("", "now")
        .map_err(store_err)?;
    canonical_db_timestamp(now)
}

async fn acquire_agent_run_claim_lock<C>(conn: &C) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let locked = entities::agent_run_claim_lock::Entity::update_many()
        .col_expr(
            entities::agent_run_claim_lock::Column::Id,
            sea_orm::sea_query::Expr::col(entities::agent_run_claim_lock::Column::Id).into(),
        )
        .filter(entities::agent_run_claim_lock::Column::Id.eq(1))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(AgentError::Store(
            "durable agent-run claim lock is missing".into(),
        ));
    }
    Ok(())
}

async fn find_expired_deadline_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Sandbox.as_str()))
        .filter(entities::agent_run::Column::Status.is_in([
            AgentRunStatus::Queued.as_str(),
            AgentRunStatus::Running.as_str(),
            AgentRunStatus::Cancelling.as_str(),
            AgentRunStatus::Waiting.as_str(),
            AgentRunStatus::RetryWait.as_str(),
        ]))
        .filter(entities::agent_run::Column::DeadlineAt.lte(now))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .order_by_asc(entities::agent_run::Column::DeadlineAt)
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_claim_candidate_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
    max_running_per_chat: u32,
    live_by_chat: &std::collections::HashMap<uuid::Uuid, usize>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    const PAGE_SIZE: u64 = 64;
    let mut offset = 0;
    loop {
        let page = entities::agent_run::Entity::find()
            .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Sandbox.as_str()))
            .filter(
                sea_orm::Condition::any()
                    .add(
                        sea_orm::Condition::all()
                            .add(entities::agent_run::Column::Status.is_in([
                                AgentRunStatus::Queued.as_str(),
                                AgentRunStatus::RetryWait.as_str(),
                            ]))
                            .add(entities::agent_run::Column::AvailableAt.lte(now)),
                    )
                    .add(
                        sea_orm::Condition::all()
                            .add(
                                entities::agent_run::Column::Status
                                    .is_in([AgentRunStatus::Running.as_str()]),
                            )
                            .add(entities::agent_run::Column::LeaseExpiresAt.lte(now)),
                    ),
            )
            .filter(entities::agent_run::Column::UpdatedAt.lte(now))
            .filter(entities::agent_run::Column::DeadlineAt.gt(now))
            .order_by_asc(entities::agent_run::Column::AvailableAt)
            .order_by_asc(entities::agent_run::Column::CreatedAt)
            .order_by_asc(entities::agent_run::Column::Id)
            .offset(offset)
            .limit(PAGE_SIZE)
            .all(conn)
            .await
            .map_err(store_err)?;
        if page.is_empty() {
            return Ok(None);
        }
        let page_len = page.len();
        if let Some(candidate) = page.into_iter().find(|candidate| {
            live_by_chat.get(&candidate.chat_id).copied().unwrap_or(0)
                < max_running_per_chat as usize
        }) {
            return Ok(Some(candidate));
        }
        if page_len < PAGE_SIZE as usize {
            return Ok(None);
        }
        offset += PAGE_SIZE;
    }
}

async fn find_expired_lease_reaper_on<C>(
    conn: &C,
    now: chrono::DateTime<Utc>,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Sandbox.as_str()))
        .filter(
            sea_orm::Condition::any()
                .add(entities::agent_run::Column::Status.eq(AgentRunStatus::Cancelling.as_str()))
                .add(
                    sea_orm::Condition::all()
                        .add(
                            entities::agent_run::Column::Status
                                .eq(AgentRunStatus::Running.as_str()),
                        )
                        .add(
                            sea_orm::sea_query::Expr::col(
                                entities::agent_run::Column::AttemptCount,
                            )
                            .gte(sea_orm::sea_query::Expr::col(
                                entities::agent_run::Column::MaxAttempts,
                            )),
                        ),
                ),
        )
        .filter(entities::agent_run::Column::LeaseExpiresAt.lte(now))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now))
        .order_by_asc(entities::agent_run::Column::LeaseExpiresAt)
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .one(conn)
        .await
        .map_err(store_err)
}

async fn fail_candidate_on<C>(
    conn: &C,
    candidate: &entities::agent_run::Model,
    now: chrono::DateTime<Utc>,
    error_code: &str,
    error_detail: &str,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let cancelling = candidate.status == AgentRunStatus::Cancelling.as_str();
    let terminal_status = if cancelling {
        AgentRunStatus::Cancelled
    } else {
        AgentRunStatus::Failed
    };
    let update = entities::agent_run::Entity::update_many()
        .col_expr(
            entities::agent_run::Column::Status,
            sea_orm::sea_query::Expr::value(terminal_status.as_str()),
        )
        .col_expr(
            entities::agent_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::agent_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::agent_run::Column::FinishedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value((!cancelling).then(|| error_code.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value((!cancelling).then(|| error_detail.to_owned())),
        )
        .col_expr(
            entities::agent_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::agent_run::Column::Id.eq(candidate.id))
        .filter(entities::agent_run::Column::Status.eq(candidate.status.clone()))
        .filter(entities::agent_run::Column::AttemptCount.eq(candidate.attempt_count))
        .filter(entities::agent_run::Column::ClaimCount.eq(candidate.claim_count))
        .filter(entities::agent_run::Column::UpdatedAt.eq(candidate.updated_at))
        .filter(entities::agent_run::Column::UpdatedAt.lte(now));
    let update = if candidate.lease_token.is_some() {
        update
            .filter(entities::agent_run::Column::LeaseToken.eq(candidate.lease_token))
            .filter(entities::agent_run::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
    } else {
        update
            .filter(entities::agent_run::Column::LeaseToken.is_null())
            .filter(entities::agent_run::Column::LeaseExpiresAt.is_null())
    };
    let updated = update.exec(conn).await.map_err(store_err)?;
    Ok(updated.rows_affected == 1)
}

pub(in crate::db) async fn list_agent_runs(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<AgentRun>> {
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::agent_run::Column::CreatedAt)
        .order_by_asc(entities::agent_run::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(agent_run_from_model)
        .collect()
}

fn validate_request(
    id: AgentRunId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<()> {
    if id.0.is_nil() {
        return Err(AgentError::Store("agent-run id must not be nil".into()));
    }
    match execution {
        AgentRunExecution::Foreground
            if parent_id.is_some() || spawn_call_id.is_some() || input.is_some() =>
        {
            Err(AgentError::Store(
                "foreground agent runs cannot have a parent, spawn call, or delegated task".into(),
            ))
        }
        AgentRunExecution::Sandbox if parent_id.is_none() || spawn_call_id.is_none() => {
            Err(AgentError::Store(
                "sandbox agent runs require a foreground parent and spawn-call identity".into(),
            ))
        }
        AgentRunExecution::Sandbox => {
            if parent_id.is_some_and(|parent| parent.0.is_nil())
                || spawn_call_id.is_some_and(|call| call.0.is_nil())
            {
                return Err(AgentError::Store(
                    "sandbox parent and spawn-call identities must not be nil".into(),
                ));
            }
            let Some(input) = input else {
                return Err(AgentError::Store(
                    "sandbox agent runs require a delegated task".into(),
                ));
            };
            let input_len = input.chars().count();
            if input_len == 0 || input_len > AgentRun::MAX_INPUT_LEN {
                return Err(AgentError::Store(format!(
                    "sandbox agent-run task must contain 1..={} characters",
                    AgentRun::MAX_INPUT_LEN
                )));
            }
            Ok(())
        }
        AgentRunExecution::Foreground => Ok(()),
    }
}

fn agent_run_from_model(model: entities::agent_run::Model) -> Result<AgentRun> {
    let execution = match model.execution.as_str() {
        "foreground" => AgentRunExecution::Foreground,
        "sandbox" => AgentRunExecution::Sandbox,
        value => {
            return Err(AgentError::Store(format!(
                "invalid agent-run execution {value}"
            )))
        }
    };
    let status = agent_run_status_from_db(&model.status)?;
    validate_stored_shape(&model, execution, status)?;
    Ok(AgentRun {
        id: AgentRunId(model.id),
        chat_id: ChatId(model.chat_id),
        parent_id: model.parent_id.map(AgentRunId),
        spawn_call_id: model.spawn_call_id.map(CallId),
        execution,
        depth: u8::try_from(model.depth)
            .map_err(|_| AgentError::Store("invalid negative agent-run depth".into()))?,
        status,
        input: model.input,
        attempt_count: model.attempt_count,
        max_attempts: model.max_attempts,
        claim_count: model.claim_count,
        available_at: model.available_at,
        deadline_at: model.deadline_at,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        started_at: model.started_at,
        finished_at: model.finished_at,
        last_error_code: model.last_error_code,
        last_error_detail: model.last_error_detail,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn agent_run_status_from_db(value: &str) -> Result<AgentRunStatus> {
    let status = match value {
        "active" => AgentRunStatus::Active,
        "queued" => AgentRunStatus::Queued,
        "running" => AgentRunStatus::Running,
        "cancelling" => AgentRunStatus::Cancelling,
        "waiting" => AgentRunStatus::Waiting,
        "retry_wait" => AgentRunStatus::RetryWait,
        "completed" => AgentRunStatus::Completed,
        "failed" => AgentRunStatus::Failed,
        "cancelled" => AgentRunStatus::Cancelled,
        value => {
            return Err(AgentError::Store(format!(
                "invalid agent-run status {value}"
            )))
        }
    };
    Ok(status)
}

fn agent_run_result_from_model(model: entities::agent_run_result::Model) -> Result<AgentRunResult> {
    if model.lease_token.is_nil()
        || model.attempt_count < 1
        || model.claim_count < model.attempt_count
        || model.text.is_empty()
        || model.text.chars().count() > AgentRun::MAX_RESULT_LEN
    {
        return Err(AgentError::Store("invalid stored agent-run result".into()));
    }
    Ok(AgentRunResult {
        agent_run_id: AgentRunId(model.agent_run_id),
        lease_token: model.lease_token,
        attempt_count: model.attempt_count,
        claim_count: model.claim_count,
        text: model.text,
        submitted_at: model.submitted_at,
    })
}

fn agent_run_inbox_from_models(
    model: entities::agent_run_inbox::Model,
    result: entities::agent_run_result::Model,
) -> Result<AgentRunInboxEntry> {
    let status = AgentRunInboxStatus::parse(&model.status).ok_or_else(|| {
        AgentError::Store("invalid stored agent-run inbox continuation status".into())
    })?;
    let valid_continuation = match status {
        AgentRunInboxStatus::Pending => {
            model.claim_count == 0
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
        AgentRunInboxStatus::Claimed => {
            model.claim_count >= 1
                && model.lease_token.is_some_and(|token| !token.is_nil())
                && model.lease_expires_at.is_some()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
        AgentRunInboxStatus::Consumed => {
            model.claim_count >= 1
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model
                    .consumed_lease_token
                    .is_some_and(|token| !token.is_nil())
                && model.consumed_at.is_some()
        }
        AgentRunInboxStatus::Cancelled => {
            model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.consumed_lease_token.is_none()
                && model.consumed_at.is_none()
        }
    };
    if model.parent_depth != 0
        || model.result_lease_token.is_nil()
        || model.result_attempt_count < 1
        || model.result_claim_count < model.result_attempt_count
        || !valid_continuation
    {
        return Err(AgentError::Store(
            "invalid stored agent-run inbox entry".into(),
        ));
    }
    let result = agent_run_result_from_model(result)?;
    if result.agent_run_id != AgentRunId(model.child_run_id)
        || result.lease_token != model.result_lease_token
        || result.attempt_count != model.result_attempt_count
        || result.claim_count != model.result_claim_count
    {
        return Err(AgentError::Store(
            "agent-run inbox does not match its result receipt".into(),
        ));
    }
    Ok(AgentRunInboxEntry {
        parent_run_id: AgentRunId(model.parent_run_id),
        child_run_id: AgentRunId(model.child_run_id),
        chat_id: ChatId(model.chat_id),
        result,
        status,
        claim_count: model.claim_count,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        consumed_lease_token: model.consumed_lease_token,
        consumed_at: model.consumed_at,
        delivered_at: model.delivered_at,
    })
}

async fn find_agent_run_inbox_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<Option<entities::agent_run_inbox::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run_inbox::Entity::find_by_id(child_run_id.0)
        .filter(entities::agent_run_inbox::Column::ParentRunId.eq(parent_run_id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn load_agent_run_inbox_by_ids_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<Option<AgentRunInboxEntry>>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(model) = find_agent_run_inbox_on(conn, parent_run_id, child_run_id).await? else {
        return Ok(None);
    };
    load_agent_run_inbox_entry_on(conn, model).await.map(Some)
}

async fn load_agent_run_inbox_entry_on<C>(
    conn: &C,
    model: entities::agent_run_inbox::Model,
) -> Result<AgentRunInboxEntry>
where
    C: sea_orm::ConnectionTrait,
{
    let result = entities::agent_run_result::Entity::find_by_id(model.child_run_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "agent-run inbox child {} is missing its result receipt",
                model.child_run_id
            ))
        })?;
    agent_run_inbox_from_models(model, result)
}

async fn parent_is_active_on<C>(conn: &C, parent_run_id: AgentRunId) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let parent = find_by_id_on(conn, parent_run_id).await?;
    Ok(parent.is_some_and(|parent| {
        parent.depth == 0
            && parent.execution == AgentRunExecution::Foreground.as_str()
            && parent.status == AgentRunStatus::Active.as_str()
    }))
}

/// Persist the exact terminal child result into the foreground transcript at
/// the same durable boundary that consumes its inbox receipt.  A deterministic
/// message id makes an ambiguous commit retry safe without relying on worker
/// memory: the next foreground provider request rebuilds this message from the
/// ordinary transcript after any restart.
async fn ensure_sandbox_result_message_on<C>(
    conn: &C,
    entry: &AgentRunInboxEntry,
    turn: &TurnRun,
    now: chrono::DateTime<Utc>,
    insert_if_missing: bool,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let child = find_by_id_on(conn, entry.child_run_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "agent-run inbox child {} disappeared before transcript delivery",
                entry.child_run_id
            ))
        })?;
    if child.chat_id != entry.chat_id.0
        || child.parent_id != Some(entry.parent_run_id.0)
        || child.execution != AgentRunExecution::Sandbox.as_str()
        || child.depth != 1
    {
        return Err(AgentError::Store(format!(
            "agent-run inbox child {} does not match its durable parent delivery",
            entry.child_run_id
        )));
    }
    let disposition = match agent_run_status_from_db(&child.status)? {
        AgentRunStatus::Completed => "completed",
        AgentRunStatus::Failed => "failed",
        status => {
            return Err(AgentError::Store(format!(
                "agent-run inbox child {} is not terminal ({})",
                entry.child_run_id,
                status.as_str()
            )));
        }
    };
    let message_id = MessageId::sandbox_result_for_child(entry.child_run_id);
    let content = format!(
        "Sandbox agent {disposition}. Its exact final result follows:\n{}",
        entry.result.text
    );
    let existing = entities::message::Entity::find_by_id(message_id.0)
        .one(conn)
        .await
        .map_err(store_err)?;
    if let Some(existing) = existing {
        if existing.chat_id == entry.chat_id.0
            && existing.turn_id == turn.id.0
            && existing.role == "system"
            && existing.content == content
        {
            return Ok(());
        }
        return Err(AgentError::Store(format!(
            "sandbox result message identity {message_id} is already bound to different content"
        )));
    }
    if !insert_if_missing {
        return Err(AgentError::Store(format!(
            "consumed agent-run inbox entry for child {} is missing its transcript result",
            entry.child_run_id
        )));
    }
    if !reserve_message_identity_on(
        conn,
        message_id,
        entry.chat_id,
        turn.id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        return Err(AgentError::Store(format!(
            "sandbox result message identity {message_id} was reserved without a message"
        )));
    }
    entities::message::ActiveModel {
        id: Set(message_id.0),
        chat_id: Set(entry.chat_id.0),
        turn_id: Set(turn.id.0),
        seq: Set(next_message_seq_on(conn, entry.chat_id).await?),
        role: Set("system".to_owned()),
        content: Set(content),
        created_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Confirm the parent has durably yielded the foreground turn that this exact
/// child result will wake. Holding the turn lock through the inbox claim keeps
/// cancellation or another wake from leaving a live continuation lease with no
/// checkpoint to consume.
async fn parent_turn_checkpoint_is_waiting_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(wait) = entities::turn_agent_run_wait::Entity::find_by_id(child_run_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if wait.parent_run_id != parent_run_id.0
        || wait.status != crate::model::TurnAgentRunWaitStatus::Waiting.as_str()
        || wait.closed_at.is_some()
    {
        return Ok(false);
    }
    let turn_id = crate::TurnId(wait.turn_id);
    if !acquire_turn_write_lock(conn, turn_id).await? {
        return Err(AgentError::Store(format!(
            "agent-run checkpoint for child {child_run_id} references missing turn {turn_id}"
        )));
    }
    let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .expect("locked agent-run checkpoint turn exists");
    Ok(turn.chat_id == wait.chat_id
        && turn.agent_run_id == parent_run_id.0
        && turn.status == crate::TurnRunStatus::WaitingForAgentRun.as_str()
        && turn.attempt_count == wait.attempt_count
        && turn.claim_count == wait.claim_count
        && turn.lease_token.is_none()
        && turn.lease_expires_at.is_none())
}

async fn find_by_id_on<C>(conn: &C, id: AgentRunId) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_by_spawn_call_on<C>(
    conn: &C,
    id: CallId,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::SpawnCallId.eq(id.0))
        .one(conn)
        .await
        .map_err(store_err)
}

async fn find_foreground_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<entities::agent_run::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::ChatId.eq(chat_id.0))
        .filter(entities::agent_run::Column::Execution.eq(AgentRunExecution::Foreground.as_str()))
        .one(conn)
        .await
        .map_err(store_err)
}

fn existing_request_outcome(
    existing: entities::agent_run::Model,
    chat_id: ChatId,
    parent_id: Option<AgentRunId>,
    spawn_call_id: Option<CallId>,
    execution: AgentRunExecution,
    input: Option<&str>,
) -> Result<AcceptAgentRunOutcome> {
    let exact = existing.chat_id == chat_id.0
        && existing.parent_id == parent_id.map(|parent| parent.0)
        && existing.spawn_call_id == spawn_call_id.map(|call| call.0)
        && existing.execution == execution.as_str()
        && existing.input.as_deref() == input;
    Ok(if exact {
        AcceptAgentRunOutcome::Existing(agent_run_from_model(existing)?)
    } else {
        AcceptAgentRunOutcome::IdentityConflict
    })
}

fn validate_stored_shape(
    model: &entities::agent_run::Model,
    execution: AgentRunExecution,
    status: AgentRunStatus,
) -> Result<()> {
    if model.id.is_nil() || model.updated_at < model.created_at {
        return Err(AgentError::Store(
            "invalid persisted agent-run identity or timestamp".into(),
        ));
    }
    let valid = match execution {
        AgentRunExecution::Foreground => {
            model.depth == 0
                && model.parent_id.is_none()
                && model.parent_depth.is_none()
                && model.spawn_call_id.is_none()
                && model.input.is_none()
                && model.attempt_count == 0
                && model.max_attempts == 0
                && model.claim_count == 0
                && model.available_at == model.created_at
                && model.deadline_at.is_none()
                && model.lease_token.is_none()
                && model.lease_expires_at.is_none()
                && model.started_at.is_none()
                && matches!(
                    status,
                    AgentRunStatus::Active
                        | AgentRunStatus::Completed
                        | AgentRunStatus::Failed
                        | AgentRunStatus::Cancelled
                )
        }
        AgentRunExecution::Sandbox => {
            model.depth == i16::from(AgentRun::MAX_DEPTH)
                && model.parent_id.is_some()
                && model.parent_depth == Some(0)
                && model.spawn_call_id.is_some_and(|call| !call.is_nil())
                && model.input.as_ref().is_some_and(|input| {
                    let len = input.chars().count();
                    len > 0 && len <= AgentRun::MAX_INPUT_LEN
                })
                && model.max_attempts >= 1
                && model.attempt_count >= 0
                && model.attempt_count <= model.max_attempts
                && model.claim_count >= model.attempt_count
                && model.available_at >= model.created_at
                && model
                    .deadline_at
                    .is_some_and(|deadline| deadline > model.created_at)
                && status != AgentRunStatus::Active
        }
    };
    if valid {
        Ok(())
    } else {
        Err(AgentError::Store(
            "invalid persisted agent-run shape".into(),
        ))
    }
}
