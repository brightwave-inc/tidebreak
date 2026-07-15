use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::model::{
    AgentRunExecution, AgentRunStatus, TurnAgentRunWait, TurnAgentRunWaitStatus,
    TurnCheckpointProgress, TurnRunStatus, TurnSteerStatus,
};
use crate::storage::ParkTurnForAgentRunInboxOutcome;
use crate::{AgentRunId, TurnId, TurnRun};

use super::super::super::{entities, store_err, DbStore};
use super::super::{acquire_chat_write_lock, acquire_turn_write_lock};
use super::{canonical_db_timestamp, turn_run_from_model};

pub(in crate::db) async fn park_turn_for_agent_run_inbox(
    store: &DbStore,
    turn_id: TurnId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ParkTurnForAgentRunInboxOutcome>> {
    validate_request(turn_id, child_run_id, lease_token, progress)?;
    let now = canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, crate::ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    if let Some(wait) = entities::turn_agent_run_wait::Entity::find_by_id(child_run_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "agent-run checkpoint for child {child_run_id} is missing its turn"
                ))
            })?;
        let exact = wait.turn_id == turn_id.0
            && wait.parent_run_id == turn.agent_run_id
            && wait.chat_id == turn.chat_id
            && wait.park_lease_token == lease_token
            && progress_from_wait_model(&wait)? == progress;
        let outcome = if exact {
            ParkTurnForAgentRunInboxOutcome::Existing {
                turn: turn_run_from_model(turn)?,
                wait: wait_from_model(wait)?,
            }
        } else {
            ParkTurnForAgentRunInboxOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }

    let turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let steer_pending = entities::turn_steer::Entity::find()
        .filter(entities::turn_steer::Column::TurnId.eq(turn_id.0))
        .filter(entities::turn_steer::Column::Status.eq(TurnSteerStatus::Pending.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if steer_pending || turn.steer_revision != expected_steer_revision {
        let turn = turn_run_from_model(turn)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(if steer_pending {
            ParkTurnForAgentRunInboxOutcome::SteerPending(turn)
        } else {
            ParkTurnForAgentRunInboxOutcome::OutputSuperseded(turn)
        }));
    }
    let child = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(child_run_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    let valid_child = child.is_some_and(|child| {
        child.chat_id == turn.chat_id
            && child.parent_id == Some(turn.agent_run_id)
            && child.depth == 1
            && child.execution == AgentRunExecution::Sandbox.as_str()
            && !matches!(
                child.status.as_str(),
                status if status == AgentRunStatus::Failed.as_str()
                    || status == AgentRunStatus::Cancelled.as_str()
            )
    });
    if !valid_child {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let totals = checked_checkpoint_totals(&turn, progress)?;
    let wait = entities::turn_agent_run_wait::ActiveModel {
        child_run_id: Set(child_run_id.0),
        parent_run_id: Set(turn.agent_run_id),
        turn_id: Set(turn_id.0),
        chat_id: Set(turn.chat_id),
        park_lease_token: Set(lease_token),
        attempt_count: Set(turn.attempt_count),
        claim_count: Set(turn.claim_count),
        model_steps: Set(progress.model_steps),
        input_tokens: Set(i64::from(progress.usage.input_tokens)),
        output_tokens: Set(i64::from(progress.usage.output_tokens)),
        cache_read_input_tokens: Set(i64::from(progress.usage.cache_read_input_tokens)),
        cache_creation_input_tokens: Set(i64::from(progress.usage.cache_creation_input_tokens)),
        status: Set(TurnAgentRunWaitStatus::Waiting.as_str().into()),
        parked_at: Set(now),
        closed_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let parked = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::WaitingForAgentRun.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
        )
        .col_expr(
            entities::turn_run::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::turn_run::Column::ModelSteps,
            sea_orm::sea_query::Expr::value(totals.model_steps),
        )
        .col_expr(
            entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(totals.input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(totals.output_tokens),
        )
        .col_expr(
            entities::turn_run::Column::CacheReadInputTokens,
            sea_orm::sea_query::Expr::value(totals.cache_read_input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::CacheCreationInputTokens,
            sea_orm::sea_query::Expr::value(totals.cache_creation_input_tokens),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::SteerRevision.eq(expected_steer_revision))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .filter(entities::turn_run::Column::UpdatedAt.lte(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if parked.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let parked_turn = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("parked turn {turn_id} disappeared")))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(ParkTurnForAgentRunInboxOutcome::Parked {
        turn: turn_run_from_model(parked_turn)?,
        wait: wait_from_model(wait)?,
    }))
}

/// Close a matching foreground checkpoint and make its turn claimable again.
/// The caller owns the encompassing transaction and only invokes this after it
/// has verified the exact live child-inbox continuation lease.
pub(in crate::db) async fn resume_turn_after_agent_run_inbox_consumption_on<C>(
    conn: &C,
    parent_run_id: AgentRunId,
    child_run_id: AgentRunId,
    now: chrono::DateTime<Utc>,
) -> Result<Option<TurnRun>>
where
    C: ConnectionTrait,
{
    let Some(wait) = entities::turn_agent_run_wait::Entity::find_by_id(child_run_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if wait.parent_run_id != parent_run_id.0 {
        return Ok(None);
    }
    let turn_id = TurnId(wait.turn_id);
    if !acquire_turn_write_lock(conn, turn_id).await? {
        return Err(AgentError::Store(format!(
            "agent-run checkpoint for child {child_run_id} references missing turn {turn_id}"
        )));
    }
    let turn = entities::turn_run::Entity::find_by_id(wait.turn_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .expect("locked checkpoint turn exists");
    if wait.status == TurnAgentRunWaitStatus::Resumed.as_str() {
        let valid = turn.chat_id == wait.chat_id
            && turn.agent_run_id == wait.parent_run_id
            && turn.status == TurnRunStatus::Resuming.as_str()
            && turn.attempt_count == wait.attempt_count
            && turn.claim_count == wait.claim_count
            && turn.lease_token.is_none()
            && turn.lease_expires_at.is_none()
            && wait.closed_at.is_some();
        return valid.then(|| turn_run_from_model(turn)).transpose();
    }
    if wait.status != TurnAgentRunWaitStatus::Waiting.as_str()
        || turn.chat_id != wait.chat_id
        || turn.agent_run_id != wait.parent_run_id
        || turn.status != TurnRunStatus::WaitingForAgentRun.as_str()
        || turn.attempt_count != wait.attempt_count
        || turn.claim_count != wait.claim_count
        || turn.lease_token.is_some()
        || turn.lease_expires_at.is_some()
    {
        return Ok(None);
    }
    // SQLite's statement clock is second-granularity while a foreground
    // checkpoint preserves microseconds. Never let that representation detail
    // make a later durable wake appear to precede its own checkpoint.
    let transition_at = std::cmp::max(now, wait.parked_at);
    let closed = entities::turn_agent_run_wait::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait::Column::Status,
            sea_orm::sea_query::Expr::value(TurnAgentRunWaitStatus::Resumed.as_str()),
        )
        .col_expr(
            entities::turn_agent_run_wait::Column::ClosedAt,
            sea_orm::sea_query::Expr::value(Some(transition_at)),
        )
        .filter(entities::turn_agent_run_wait::Column::ChildRunId.eq(child_run_id.0))
        .filter(
            entities::turn_agent_run_wait::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .filter(entities::turn_agent_run_wait::Column::ClosedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    if closed.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "agent-run checkpoint for child {child_run_id} changed while resuming"
        )));
    }
    let resumed = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Resuming.as_str()),
        )
        .col_expr(
            entities::turn_run::Column::AvailableAt,
            sea_orm::sea_query::Expr::value(transition_at),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(transition_at),
        )
        .filter(entities::turn_run::Column::Id.eq(turn.id))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::WaitingForAgentRun.as_str()))
        .filter(entities::turn_run::Column::AttemptCount.eq(turn.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(turn.claim_count))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if resumed.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "agent-run checkpoint for child {child_run_id} lost its turn wake"
        )));
    }
    let turn = entities::turn_run::Entity::find_by_id(turn.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("resumed turn {turn_id} disappeared")))?;
    turn_run_from_model(turn).map(Some)
}

fn validate_request(
    turn_id: TurnId,
    child_run_id: AgentRunId,
    lease_token: uuid::Uuid,
    progress: TurnCheckpointProgress,
) -> Result<()> {
    if turn_id.0.is_nil()
        || child_run_id.0.is_nil()
        || lease_token.is_nil()
        || progress.model_steps <= 0
    {
        return Err(AgentError::Store(
            "invalid sandbox-child turn checkpoint request".into(),
        ));
    }
    Ok(())
}

fn checked_checkpoint_totals(
    turn: &entities::turn_run::Model,
    progress: TurnCheckpointProgress,
) -> Result<CheckpointTotals> {
    let model_steps = turn
        .model_steps
        .checked_add(progress.model_steps)
        .filter(|total| *total >= 0)
        .ok_or_else(|| AgentError::Store("turn model-step checkpoint overflowed".into()))?;
    Ok(CheckpointTotals {
        model_steps,
        input_tokens: checked_token_total("input", turn.input_tokens, progress.usage.input_tokens)?,
        output_tokens: checked_token_total(
            "output",
            turn.output_tokens,
            progress.usage.output_tokens,
        )?,
        cache_read_input_tokens: checked_token_total(
            "cache-read input",
            turn.cache_read_input_tokens,
            progress.usage.cache_read_input_tokens,
        )?,
        cache_creation_input_tokens: checked_token_total(
            "cache-creation input",
            turn.cache_creation_input_tokens,
            progress.usage.cache_creation_input_tokens,
        )?,
    })
}

struct CheckpointTotals {
    model_steps: i32,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_input_tokens: i64,
    cache_creation_input_tokens: i64,
}

fn checked_token_total(field: &str, current: i64, delta: u32) -> Result<i64> {
    current
        .checked_add(i64::from(delta))
        .filter(|total| u32::try_from(*total).is_ok())
        .ok_or_else(|| AgentError::Store(format!("turn {field} accounting overflowed")))
}

fn progress_from_wait_model(
    wait: &entities::turn_agent_run_wait::Model,
) -> Result<TurnCheckpointProgress> {
    fn token(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value).map_err(|_| {
            AgentError::Store(format!(
                "agent-run checkpoint {field} token count is invalid"
            ))
        })
    }
    Ok(TurnCheckpointProgress {
        model_steps: wait.model_steps,
        usage: crate::Usage {
            input_tokens: token(wait.input_tokens, "input")?,
            output_tokens: token(wait.output_tokens, "output")?,
            cache_read_input_tokens: token(wait.cache_read_input_tokens, "cache-read input")?,
            cache_creation_input_tokens: token(
                wait.cache_creation_input_tokens,
                "cache-creation input",
            )?,
        },
    })
}

pub(in crate::db) fn wait_from_model(
    wait: entities::turn_agent_run_wait::Model,
) -> Result<TurnAgentRunWait> {
    let status = match wait.status.as_str() {
        "waiting" => TurnAgentRunWaitStatus::Waiting,
        "resumed" => TurnAgentRunWaitStatus::Resumed,
        "cancelled" => TurnAgentRunWaitStatus::Cancelled,
        _ => {
            return Err(AgentError::Store(
                "invalid agent-run checkpoint status".into(),
            ))
        }
    };
    if wait.attempt_count < 1
        || wait.claim_count < wait.attempt_count
        || wait.model_steps <= 0
        || (status == TurnAgentRunWaitStatus::Waiting) != wait.closed_at.is_none()
    {
        return Err(AgentError::Store(
            "invalid stored agent-run checkpoint".into(),
        ));
    }
    Ok(TurnAgentRunWait {
        child_run_id: AgentRunId(wait.child_run_id),
        parent_run_id: AgentRunId(wait.parent_run_id),
        turn_id: TurnId(wait.turn_id),
        chat_id: crate::ChatId(wait.chat_id),
        park_lease_token: wait.park_lease_token,
        attempt_count: wait.attempt_count,
        claim_count: wait.claim_count,
        progress: progress_from_wait_model(&wait)?,
        status,
        parked_at: wait.parked_at,
        closed_at: wait.closed_at,
    })
}
