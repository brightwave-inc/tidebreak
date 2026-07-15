use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseBackend, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, Statement, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{AgentRunId, CallId, ChatId};
use crate::model::{AgentRun, AgentRunExecution, AgentRunStatus};
use crate::storage::AcceptAgentRunOutcome;

use super::super::{entities, store_err, DbStore};
use super::{acquire_chat_write_lock, turn::canonical_db_timestamp};

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
    let now = database_now(&store.conn).await?;
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
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(heartbeat.rows_affected == 1)
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
    let status = match model.status.as_str() {
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
