use std::collections::HashSet;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, FromQueryResult, QueryFilter,
    QueryOrder, Set, Statement, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::model::{
    AgentRunExecution, AgentRunInboxStatus, AgentRunWaitCondition, AgentRunWaitSetCandidate,
    TurnAgentRunWaitSet, TurnAgentRunWaitStatus, TurnCheckpointProgress, TurnRunStatus,
    TurnSteerStatus,
};
use crate::storage::{ParkTurnForAgentRunWaitSetOutcome, ResumeTurnForAgentRunWaitSetOutcome};
use crate::{AgentRunId, CallId, TurnId};

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::{
    database_now, ensure_sandbox_result_message_on, load_agent_run_inbox_by_ids_on,
};
use super::super::{acquire_chat_write_lock, acquire_turn_write_lock};
use super::{canonical_db_timestamp, turn_run_from_model};

/// Find ordered child waits that appear ready after a process restart.
///
/// This is intentionally a read-only hint. It performs stricter projection
/// validation than the resume transaction needs for safety so malformed or
/// partially-corrupt ownership rows are excluded instead of being handed to a
/// worker. The resume transition re-locks and rechecks every relationship.
#[derive(Debug, FromQueryResult)]
struct ReadyWaitSetRow {
    wait_id: uuid::Uuid,
    ready_at: chrono::DateTime<Utc>,
}

pub(in crate::db) async fn list_ready_agent_run_wait_set_candidates(
    store: &DbStore,
    limit: u64,
) -> Result<Vec<AgentRunWaitSetCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let now = database_now(&store.conn).await?;
    // The joins are an intentionally strict prefilter: because the joined row
    // count must equal the complete member count, one missing, claimed, or
    // ownership-mismatched member excludes the set before LIMIT is applied.
    // This keeps the scan bounded without allowing corrupt historical rows to
    // starve a later coherent wait.
    let sql = format!(
        r#"
SELECT w.id AS wait_id, MAX(i.delivered_at) AS ready_at
FROM turn_agent_run_wait_set w
JOIN turn_run t
  ON t.id = w.turn_id AND t.chat_id = w.chat_id AND t.agent_run_id = w.parent_run_id
JOIN agent_run p
  ON p.id = w.parent_run_id AND p.chat_id = w.chat_id AND p.depth = 0
JOIN turn_agent_run_wait_member m
  ON m.wait_id = w.id AND m.parent_run_id = w.parent_run_id
 AND m.origin_turn_id = w.turn_id AND m.chat_id = w.chat_id
JOIN sandbox_agent_admission a
  ON a.child_run_id = m.child_run_id AND a.parent_run_id = w.parent_run_id
 AND a.origin_turn_id = w.turn_id AND a.chat_id = w.chat_id
JOIN agent_run c
  ON c.id = m.child_run_id AND c.chat_id = w.chat_id AND c.depth = 1
 AND c.parent_id = w.parent_run_id AND c.parent_depth = 0
 AND c.spawn_call_id = a.spawn_call_id
JOIN agent_run_inbox i
  ON i.child_run_id = m.child_run_id AND i.parent_run_id = w.parent_run_id
 AND i.chat_id = w.chat_id AND i.parent_depth = 0
JOIN agent_run_result r
  ON r.agent_run_id = i.child_run_id AND r.lease_token = i.result_lease_token
 AND r.attempt_count = i.result_attempt_count AND r.claim_count = i.result_claim_count
WHERE w.status = 'waiting' AND w.condition = 'all'
  AND w.closed_at IS NULL AND w.resume_token IS NULL
  AND w.expected_steer_revision >= 0 AND w.attempt_count >= 1
  AND w.claim_count >= w.attempt_count AND w.model_steps > 0
  AND w.input_tokens >= 0 AND w.output_tokens >= 0
  AND w.cache_read_input_tokens >= 0 AND w.cache_creation_input_tokens >= 0
  AND t.status = 'waiting_for_agent_run'
  AND t.attempt_count = w.attempt_count AND t.claim_count = w.claim_count
  AND t.lease_token IS NULL AND t.lease_expires_at IS NULL
  AND t.steer_revision = w.expected_steer_revision AND t.updated_at >= w.parked_at
  AND t.model_steps >= w.model_steps AND t.input_tokens >= w.input_tokens
  AND t.output_tokens >= w.output_tokens
  AND t.cache_read_input_tokens >= w.cache_read_input_tokens
  AND t.cache_creation_input_tokens >= w.cache_creation_input_tokens
  AND p.execution = 'foreground' AND p.status = 'active' AND p.parent_id IS NULL
  AND c.execution = 'sandbox' AND c.status IN ('completed', 'failed', 'cancelled')
  AND i.status = 'pending' AND i.claim_count = 0
  AND i.lease_token IS NULL AND i.lease_expires_at IS NULL
  AND i.consumed_lease_token IS NULL AND i.consumed_at IS NULL
GROUP BY w.id
HAVING COUNT(*) BETWEEN 1 AND {max_children}
   AND COUNT(*) = (SELECT COUNT(*) FROM turn_agent_run_wait_member all_m WHERE all_m.wait_id = w.id)
   AND MIN(m.position) = 0 AND MAX(m.position) = COUNT(*) - 1
   AND COUNT(DISTINCT m.child_run_id) = COUNT(*)
ORDER BY MAX(i.delivered_at) ASC, w.id ASC
LIMIT {limit}
"#,
        max_children = TurnAgentRunWaitSet::MAX_CHILDREN,
        limit = limit.min(i64::MAX as u64),
    );
    let rows = ReadyWaitSetRow::find_by_statement(Statement::from_string(
        store.conn.get_database_backend(),
        sql,
    ))
    .all(&store.conn)
    .await
    .map_err(store_err)?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        if row.wait_id.is_nil() || row.ready_at > now {
            continue;
        }
        candidates.push(AgentRunWaitSetCandidate {
            wait_id: CallId(row.wait_id),
            ready_at: row.ready_at,
        });
    }
    Ok(candidates)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn park_turn_for_agent_run_wait_set(
    store: &DbStore,
    wait_id: CallId,
    turn_id: TurnId,
    child_run_ids: &[AgentRunId],
    condition: AgentRunWaitCondition,
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: TurnCheckpointProgress,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
    validate_park_request(
        wait_id,
        turn_id,
        child_run_ids,
        lease_token,
        expected_steer_revision,
        progress,
    )?;
    canonical_db_timestamp(now)?;
    let Some(scope) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_wait_set_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, crate::ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;

    // Immutable receipt recovery deliberately precedes every mutable lease,
    // steering, parent-liveness, and child-status check.
    if let Some(stored) = entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        if stored.turn_id != turn_id.0 {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
        }
        let members = load_members(&transaction, wait_id).await?;
        let turn = entities::turn_run::Entity::find_by_id(stored.turn_id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store(format!("wait set {wait_id} is missing its turn")))?;
        let exact = stored.turn_id == turn_id.0
            && stored.parent_run_id == turn.agent_run_id
            && stored.chat_id == turn.chat_id
            && stored.condition == condition.as_str()
            && stored.park_lease_token == lease_token
            && stored.expected_steer_revision == expected_steer_revision
            && progress_from_model(&stored)? == progress
            && member_ids(&members) == child_run_ids;
        let outcome = if exact {
            ParkTurnForAgentRunWaitSetOutcome::Existing {
                turn: turn_run_from_model(turn)?,
                wait: wait_from_models(stored, members)?,
            }
        } else {
            ParkTurnForAgentRunWaitSetOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }

    let competing_set = entities::turn_agent_run_wait_set::Entity::find()
        .filter(entities::turn_agent_run_wait_set::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    let legacy_wait = entities::turn_agent_run_wait::Entity::find()
        .filter(entities::turn_agent_run_wait::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::turn_agent_run_wait::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if competing_set || legacy_wait {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
    }
    let reused_member = entities::turn_agent_run_wait_member::Entity::find()
        .filter(
            entities::turn_agent_run_wait_member::Column::ChildRunId
                .is_in(child_run_ids.iter().map(|id| id.0)),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    let reused_legacy_child = entities::turn_agent_run_wait::Entity::find()
        .filter(
            entities::turn_agent_run_wait::Column::ChildRunId
                .is_in(child_run_ids.iter().map(|id| id.0)),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if reused_member || reused_legacy_child {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
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
            ParkTurnForAgentRunWaitSetOutcome::SteerPending(turn)
        } else {
            ParkTurnForAgentRunWaitSetOutcome::OutputSuperseded(turn)
        }));
    }
    for child_run_id in child_run_ids {
        let admission = entities::sandbox_agent_admission::Entity::find_by_id(child_run_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let child = entities::agent_run::Entity::find()
            .filter(entities::agent_run::Column::Id.eq(child_run_id.0))
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let valid = admission.zip(child).is_some_and(|(admission, child)| {
            admission.origin_turn_id == turn_id.0
                && admission.parent_run_id == turn.agent_run_id
                && admission.chat_id == turn.chat_id
                && admission.child_run_id == child.id
                && child.parent_id == Some(turn.agent_run_id)
                && child.chat_id == turn.chat_id
                && child.execution == AgentRunExecution::Sandbox.as_str()
                && child.spawn_call_id == Some(admission.spawn_call_id)
        });
        if !valid {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
        }
    }

    let totals = checked_checkpoint_totals(&turn, progress)?;
    let stored = entities::turn_agent_run_wait_set::ActiveModel {
        id: Set(wait_id.0),
        parent_run_id: Set(turn.agent_run_id),
        turn_id: Set(turn.id),
        chat_id: Set(turn.chat_id),
        condition: Set(condition.as_str().into()),
        park_lease_token: Set(lease_token),
        expected_steer_revision: Set(expected_steer_revision),
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
        resume_token: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    for (position, child_run_id) in child_run_ids.iter().enumerate() {
        entities::turn_agent_run_wait_member::ActiveModel {
            wait_id: Set(wait_id.0),
            position: Set(i16::try_from(position)
                .map_err(|_| AgentError::Store("agent wait position exceeds i16".into()))?),
            child_run_id: Set(child_run_id.0),
            parent_run_id: Set(turn.agent_run_id),
            origin_turn_id: Set(turn.id),
            chat_id: Set(turn.chat_id),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
    }
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
            sea_orm::sea_query::Expr::value(totals.0),
        )
        .col_expr(
            entities::turn_run::Column::InputTokens,
            sea_orm::sea_query::Expr::value(totals.1),
        )
        .col_expr(
            entities::turn_run::Column::OutputTokens,
            sea_orm::sea_query::Expr::value(totals.2),
        )
        .col_expr(
            entities::turn_run::Column::CacheReadInputTokens,
            sea_orm::sea_query::Expr::value(totals.3),
        )
        .col_expr(
            entities::turn_run::Column::CacheCreationInputTokens,
            sea_orm::sea_query::Expr::value(totals.4),
        )
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_run::Column::Id.eq(turn.id))
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::turn_run::Column::LeaseToken.eq(lease_token))
        .filter(entities::turn_run::Column::LeaseExpiresAt.eq(turn.lease_expires_at))
        .filter(entities::turn_run::Column::LeaseExpiresAt.gt(now))
        .filter(entities::turn_run::Column::SteerRevision.eq(expected_steer_revision))
        .filter(entities::turn_run::Column::UpdatedAt.eq(turn.updated_at))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if parked.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let turn = entities::turn_run::Entity::find_by_id(turn.id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("parked turn {turn_id} disappeared")))?;
    let members = load_members(&transaction, wait_id).await?;
    let outcome = ParkTurnForAgentRunWaitSetOutcome::Parked {
        turn: turn_run_from_model(turn)?,
        wait: wait_from_models(stored, members)?,
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

pub(in crate::db) async fn resume_turn_for_agent_run_wait_set(
    store: &DbStore,
    wait_id: CallId,
    resume_token: uuid::Uuid,
) -> Result<Option<ResumeTurnForAgentRunWaitSetOutcome>> {
    if wait_id.0.is_nil() || resume_token.is_nil() {
        return Err(AgentError::Store(
            "agent-run wait resume requires non-nil identities".into(),
        ));
    }
    let Some(scope) = entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let transaction = store.conn.begin().await.map_err(store_err)?;
    acquire_wait_set_lock(&transaction).await?;
    if !acquire_chat_write_lock(&transaction, crate::ChatId(scope.chat_id)).await?
        || !acquire_turn_write_lock(&transaction, TurnId(scope.turn_id)).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;
    let stored = entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked wait set exists");
    let members = load_members(&transaction, wait_id).await?;
    let turn = entities::turn_run::Entity::find_by_id(stored.turn_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("wait set {wait_id} is missing its turn")))?;

    // Exact recovery is evaluated before the foreground coordinator's mutable
    // liveness or current turn eligibility.
    if stored.status == TurnAgentRunWaitStatus::Resumed.as_str() {
        if stored.resume_token != Some(resume_token)
            || turn.id != stored.turn_id
            || turn.chat_id != stored.chat_id
            || turn.agent_run_id != stored.parent_run_id
        {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        let mut results = Vec::with_capacity(members.len());
        for member in &members {
            let Some(entry) = load_agent_run_inbox_by_ids_on(
                &transaction,
                AgentRunId(stored.parent_run_id),
                AgentRunId(member.child_run_id),
            )
            .await?
            else {
                return Err(AgentError::Store(format!(
                    "resumed wait {wait_id} is missing child inbox {}",
                    member.child_run_id
                )));
            };
            if entry.status != AgentRunInboxStatus::Consumed
                || entry.consumed_lease_token != Some(resume_token)
            {
                return Err(AgentError::Store(format!(
                    "resumed wait {wait_id} has mismatched child consumption"
                )));
            }
            results.push(entry);
        }
        let outcome = ResumeTurnForAgentRunWaitSetOutcome::Existing {
            turn: turn_run_from_model(turn)?,
            wait: wait_from_models(stored, members)?,
            results,
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(outcome));
    }
    if stored.status != TurnAgentRunWaitStatus::Waiting.as_str()
        || turn.status != TurnRunStatus::WaitingForAgentRun.as_str()
        || turn.parent_shape_mismatch(&stored)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let parent = entities::agent_run::Entity::find()
        .filter(entities::agent_run::Column::Id.eq(stored.parent_run_id))
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if !parent.is_some_and(|parent| {
        parent.execution == AgentRunExecution::Foreground.as_str()
            && parent.status == crate::model::AgentRunStatus::Active.as_str()
            && parent.chat_id == stored.chat_id
    }) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let mut results = Vec::with_capacity(members.len());
    for member in &members {
        let Some(entry) = load_agent_run_inbox_by_ids_on(
            &transaction,
            AgentRunId(stored.parent_run_id),
            AgentRunId(member.child_run_id),
        )
        .await?
        else {
            let wait = wait_from_models(stored.clone(), members.clone())?;
            let child = entities::agent_run::Entity::find()
                .filter(entities::agent_run::Column::Id.eq(member.child_run_id))
                .one(&transaction)
                .await
                .map_err(store_err)?
                .ok_or_else(|| AgentError::Store("wait member child disappeared".into()))?;
            let terminal_status = match child.status.as_str() {
                "completed" => Some(crate::model::AgentRunStatus::Completed),
                "failed" => Some(crate::model::AgentRunStatus::Failed),
                "cancelled" => Some(crate::model::AgentRunStatus::Cancelled),
                _ => None,
            };
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(if let Some(child_status) = terminal_status {
                ResumeTurnForAgentRunWaitSetOutcome::TerminalDeliveryMissing {
                    wait,
                    child_run_id: AgentRunId(member.child_run_id),
                    child_status,
                }
            } else {
                ResumeTurnForAgentRunWaitSetOutcome::NotReady(wait)
            }));
        };
        if entry.status != AgentRunInboxStatus::Pending || entry.claim_count != 0 {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        results.push(entry);
    }

    let turn_value = turn_run_from_model(turn.clone())?;
    for entry in &results {
        ensure_sandbox_result_message_on(&transaction, entry, &turn_value, now, true).await?;
        let consumed = entities::agent_run_inbox::Entity::update_many()
            .col_expr(
                entities::agent_run_inbox::Column::Status,
                sea_orm::sea_query::Expr::value(AgentRunInboxStatus::Consumed.as_str()),
            )
            .col_expr(
                entities::agent_run_inbox::Column::ClaimCount,
                sea_orm::sea_query::Expr::value(1),
            )
            .col_expr(
                entities::agent_run_inbox::Column::ConsumedLeaseToken,
                sea_orm::sea_query::Expr::value(Some(resume_token)),
            )
            .col_expr(
                entities::agent_run_inbox::Column::ConsumedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(entities::agent_run_inbox::Column::ChildRunId.eq(entry.child_run_id.0))
            .filter(entities::agent_run_inbox::Column::ParentRunId.eq(stored.parent_run_id))
            .filter(
                entities::agent_run_inbox::Column::Status.eq(AgentRunInboxStatus::Pending.as_str()),
            )
            .filter(entities::agent_run_inbox::Column::ClaimCount.eq(0))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if consumed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(None);
        }
    }
    let transition_at = std::cmp::max(now, stored.parked_at);
    let closed = entities::turn_agent_run_wait_set::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_set::Column::Status,
            sea_orm::sea_query::Expr::value(TurnAgentRunWaitStatus::Resumed.as_str()),
        )
        .col_expr(
            entities::turn_agent_run_wait_set::Column::ClosedAt,
            sea_orm::sea_query::Expr::value(Some(transition_at)),
        )
        .col_expr(
            entities::turn_agent_run_wait_set::Column::ResumeToken,
            sea_orm::sea_query::Expr::value(Some(resume_token)),
        )
        .filter(entities::turn_agent_run_wait_set::Column::Id.eq(wait_id.0))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
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
        .filter(entities::turn_run::Column::AttemptCount.eq(stored.attempt_count))
        .filter(entities::turn_run::Column::ClaimCount.eq(stored.claim_count))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if closed.rows_affected != 1 || resumed.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let turn = entities::turn_run::Entity::find_by_id(turn.id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("resumed turn exists");
    let stored = entities::turn_agent_run_wait_set::Entity::find_by_id(wait_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("resumed wait exists");
    let mut consumed_results = Vec::with_capacity(members.len());
    for member in &members {
        consumed_results.push(
            load_agent_run_inbox_by_ids_on(
                &transaction,
                AgentRunId(stored.parent_run_id),
                AgentRunId(member.child_run_id),
            )
            .await?
            .expect("consumed inbox exists"),
        );
    }
    let outcome = ResumeTurnForAgentRunWaitSetOutcome::Resumed {
        turn: turn_run_from_model(turn)?,
        wait: wait_from_models(stored, members)?,
        results: consumed_results,
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(outcome))
}

/// Fence every child owned by the only open wait set and close the set under
/// the caller's foreground cancellation transaction.
pub(in crate::db) async fn cancel_wait_set_for_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    turn: &entities::turn_run::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(wait) = entities::turn_agent_run_wait_set::Entity::find()
        .filter(entities::turn_agent_run_wait_set::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Err(AgentError::Store(format!(
            "waiting turn {turn_id} is missing its child wait receipt"
        )));
    };
    if wait.chat_id != turn.chat_id
        || wait.parent_run_id != turn.agent_run_id
        || wait.attempt_count != turn.attempt_count
        || wait.claim_count != turn.claim_count
        || wait.closed_at.is_some()
        || wait.resume_token.is_some()
    {
        return Err(AgentError::Store(format!(
            "waiting turn {turn_id} has a mismatched child wait receipt"
        )));
    }
    let members = load_members(conn, CallId(wait.id)).await?;
    let _validated = wait_from_models(wait.clone(), members.clone())?;
    let closed = entities::turn_agent_run_wait_set::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_set::Column::Status,
            sea_orm::sea_query::Expr::value(TurnAgentRunWaitStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::turn_agent_run_wait_set::Column::ClosedAt,
            sea_orm::sea_query::Expr::value(Some(std::cmp::max(now, wait.parked_at))),
        )
        .filter(entities::turn_agent_run_wait_set::Column::Id.eq(wait.id))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .filter(entities::turn_agent_run_wait_set::Column::ClosedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(closed.rows_affected == 1)
}

trait TurnShape {
    fn parent_shape_mismatch(&self, wait: &entities::turn_agent_run_wait_set::Model) -> bool;
}

impl TurnShape for entities::turn_run::Model {
    fn parent_shape_mismatch(&self, wait: &entities::turn_agent_run_wait_set::Model) -> bool {
        self.id != wait.turn_id
            || self.chat_id != wait.chat_id
            || self.agent_run_id != wait.parent_run_id
            || self.attempt_count != wait.attempt_count
            || self.claim_count != wait.claim_count
            || self.lease_token.is_some()
            || self.lease_expires_at.is_some()
    }
}

fn validate_park_request(
    wait_id: CallId,
    turn_id: TurnId,
    child_run_ids: &[AgentRunId],
    lease_token: uuid::Uuid,
    expected_steer_revision: i64,
    progress: TurnCheckpointProgress,
) -> Result<()> {
    let unique = child_run_ids.iter().map(|id| id.0).collect::<HashSet<_>>();
    if wait_id.0.is_nil()
        || turn_id.0.is_nil()
        || lease_token.is_nil()
        || expected_steer_revision < 0
        || progress.model_steps <= 0
        || child_run_ids.is_empty()
        || child_run_ids.len() > TurnAgentRunWaitSet::MAX_CHILDREN
        || unique.len() != child_run_ids.len()
        || child_run_ids.iter().any(|id| id.0.is_nil())
    {
        return Err(AgentError::Store(
            "invalid ordered sandbox-child wait request".into(),
        ));
    }
    Ok(())
}

async fn load_members<C>(
    conn: &C,
    wait_id: CallId,
) -> Result<Vec<entities::turn_agent_run_wait_member::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::turn_agent_run_wait_member::Entity::find()
        .filter(entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .order_by_asc(entities::turn_agent_run_wait_member::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)
}

async fn acquire_wait_set_lock<C>(conn: &C) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let locked = entities::turn_agent_run_wait_lock::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_lock::Column::Id,
            sea_orm::sea_query::Expr::col(entities::turn_agent_run_wait_lock::Column::Id).into(),
        )
        .filter(entities::turn_agent_run_wait_lock::Column::Id.eq(1))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected != 1 {
        return Err(AgentError::Store(
            "agent-run wait serialization lock is missing".into(),
        ));
    }
    Ok(())
}

fn member_ids(members: &[entities::turn_agent_run_wait_member::Model]) -> Vec<AgentRunId> {
    members
        .iter()
        .map(|member| AgentRunId(member.child_run_id))
        .collect()
}

fn progress_from_model(
    wait: &entities::turn_agent_run_wait_set::Model,
) -> Result<TurnCheckpointProgress> {
    let token = |value, field| {
        u32::try_from(value)
            .map_err(|_| AgentError::Store(format!("agent wait {field} tokens are invalid")))
    };
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

fn wait_from_models(
    wait: entities::turn_agent_run_wait_set::Model,
    members: Vec<entities::turn_agent_run_wait_member::Model>,
) -> Result<TurnAgentRunWaitSet> {
    if members.is_empty()
        || members.len() > TurnAgentRunWaitSet::MAX_CHILDREN
        || members.iter().enumerate().any(|(position, member)| {
            member.wait_id != wait.id
                || usize::try_from(member.position).ok() != Some(position)
                || member.parent_run_id != wait.parent_run_id
                || member.origin_turn_id != wait.turn_id
                || member.chat_id != wait.chat_id
        })
    {
        return Err(AgentError::Store(
            "invalid stored agent wait members".into(),
        ));
    }
    let condition = match wait.condition.as_str() {
        "all" => AgentRunWaitCondition::All,
        _ => {
            return Err(AgentError::Store(
                "invalid stored agent wait condition".into(),
            ))
        }
    };
    let status = match wait.status.as_str() {
        "waiting" => TurnAgentRunWaitStatus::Waiting,
        "resumed" => TurnAgentRunWaitStatus::Resumed,
        "cancelled" => TurnAgentRunWaitStatus::Cancelled,
        _ => return Err(AgentError::Store("invalid stored agent wait status".into())),
    };
    Ok(TurnAgentRunWaitSet {
        id: CallId(wait.id),
        parent_run_id: AgentRunId(wait.parent_run_id),
        turn_id: TurnId(wait.turn_id),
        chat_id: crate::ChatId(wait.chat_id),
        child_run_ids: member_ids(&members),
        condition,
        park_lease_token: wait.park_lease_token,
        expected_steer_revision: wait.expected_steer_revision,
        attempt_count: wait.attempt_count,
        claim_count: wait.claim_count,
        progress: progress_from_model(&wait)?,
        status,
        parked_at: wait.parked_at,
        closed_at: wait.closed_at,
        resume_token: wait.resume_token,
    })
}

fn checked_checkpoint_totals(
    turn: &entities::turn_run::Model,
    progress: TurnCheckpointProgress,
) -> Result<(i32, i64, i64, i64, i64)> {
    let model_steps = turn
        .model_steps
        .checked_add(progress.model_steps)
        .filter(|total| *total >= 0)
        .ok_or_else(|| AgentError::Store("turn model-step checkpoint overflowed".into()))?;
    let add = |current: i64, delta: u32, field: &str| {
        current
            .checked_add(i64::from(delta))
            .filter(|total| u32::try_from(*total).is_ok())
            .ok_or_else(|| AgentError::Store(format!("turn {field} accounting overflowed")))
    };
    Ok((
        model_steps,
        add(turn.input_tokens, progress.usage.input_tokens, "input")?,
        add(turn.output_tokens, progress.usage.output_tokens, "output")?,
        add(
            turn.cache_read_input_tokens,
            progress.usage.cache_read_input_tokens,
            "cache-read input",
        )?,
        add(
            turn.cache_creation_input_tokens,
            progress.usage.cache_creation_input_tokens,
            "cache-creation input",
        )?,
    ))
}
