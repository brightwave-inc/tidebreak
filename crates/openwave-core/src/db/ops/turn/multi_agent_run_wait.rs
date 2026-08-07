use std::collections::HashSet;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, FromQueryResult, QueryFilter, QueryOrder, Set,
    Statement, TransactionTrait,
};

use crate::agent_tools::{
    canonical_wait_for_agents_result, WaitForAgentsArgs, WAIT_CANCELLED_WITH_TURN_RESULT,
    WAIT_FOR_AGENTS_TOOL, WAIT_INTERRUPTED_BY_STEER_RESULT,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::model::{
    AgentRunInboxStatus, AgentRunTier, AgentRunWaitCondition, AgentRunWaitSetCandidate,
    AgentRunWaitSetCheckpointRequest, ToolCallExecution, ToolCallRecord, ToolCallStatus,
    TurnAgentRunWaitSet, TurnAgentRunWaitStatus, TurnCheckpointProgress, TurnRunStatus,
    TurnSteerStatus,
};
use crate::storage::{ParkTurnForAgentRunWaitSetOutcome, ResumeTurnForAgentRunWaitSetOutcome};
use crate::{AgentRunId, CallId, TurnId};

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::{database_now, load_agent_run_inbox_by_ids_on};
use super::super::{acquire_chat_write_lock, acquire_turn_write_lock};
use super::super::{
    client_execution::tool_call_from_model, conversation::append_event_on,
    next_tool_history_order_on,
};
use super::{canonical_db_timestamp, turn_run_from_model};

mod tool_receipt;

use tool_receipt::{
    exact_pending_wait_call_model, exact_terminal_wait_call, exact_wait_call_request,
    exact_wait_lifecycle_on,
};

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
JOIN tool_call tc
  ON tc.id = w.id AND tc.chat_id = w.chat_id AND tc.turn_id = w.turn_id
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
  AND w.event_seq IS NULL
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
  AND p.tier = 'foreground' AND p.status = 'active' AND p.parent_id IS NULL
  AND tc.name = 'wait_for_agents' AND tc.execution = 'orchestration'
  AND tc.status = 'pending' AND tc.result IS NULL AND tc.error_code IS NULL
  AND tc.error_detail IS NULL AND tc.resolved_at IS NULL
  AND tc.client_executor_id IS NULL AND tc.client_lease_token IS NULL
  AND tc.client_lease_expires_at IS NULL AND tc.created_at = w.parked_at
  AND c.tier = 'background' AND c.status IN ('completed', 'failed', 'cancelled')
  AND m.open = TRUE
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

pub(in crate::db) async fn park_turn_for_agent_run_wait_set(
    store: &DbStore,
    request: &AgentRunWaitSetCheckpointRequest,
    now: chrono::DateTime<Utc>,
) -> Result<Option<ParkTurnForAgentRunWaitSetOutcome>> {
    validate_park_request(request)?;
    canonical_db_timestamp(now)?;
    let wait_id = request.call_id;
    let turn_id = request.origin_turn_id;
    let child_run_ids = request.child_run_ids.as_slice();
    let condition = request.condition;
    let lease_token = request.lease_token;
    let expected_steer_revision = request.expected_steer_revision;
    let progress = request.progress;
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
        let call_model = entities::tool_call::Entity::find_by_id(wait_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("wait set {wait_id} is missing its tool call"))
            })?;
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
            && stored.event_ordinal == request.event_ordinal
            && progress_from_model(&stored)? == progress
            && member_ids(&members) == child_run_ids
            && exact_wait_call_request(&call_model, &stored, request)
            && exact_wait_lifecycle_on(&transaction, &call_model, &stored).await?;
        let outcome = if exact {
            ParkTurnForAgentRunWaitSetOutcome::Existing {
                turn: turn_run_from_model(turn)?,
                call: tool_call_from_model(call_model)?,
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
    if competing_set {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
    }
    let reused_member = entities::turn_agent_run_wait_member::Entity::find()
        .filter(
            entities::turn_agent_run_wait_member::Column::ChildRunId
                .is_in(child_run_ids.iter().map(|id| id.0)),
        )
        .filter(entities::turn_agent_run_wait_member::Column::Open.eq(true))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if reused_member {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
    }

    if entities::tool_call::Entity::find_by_id(wait_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
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
                && child.tier == AgentRunTier::Background.as_str()
                && child.spawn_call_id == Some(admission.spawn_call_id)
        });
        if !valid {
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
        }
        if let Some(inbox) = entities::agent_run_inbox::Entity::find_by_id(child_run_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let pending_unclaimed = inbox.parent_run_id == turn.agent_run_id
                && inbox.chat_id == turn.chat_id
                && inbox.status == AgentRunInboxStatus::Pending.as_str()
                && inbox.claim_count == 0
                && inbox.lease_token.is_none()
                && inbox.lease_expires_at.is_none()
                && inbox.consumed_lease_token.is_none()
                && inbox.consumed_at.is_none();
            if !pending_unclaimed {
                transaction.commit().await.map_err(store_err)?;
                return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
            }
        }
    }

    let last_attempt_event = entities::event::Entity::find()
        .filter(entities::event::Column::LeaseToken.eq(lease_token))
        .order_by_desc(entities::event::Column::AttemptEventOrdinal)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    let Some(last_ordinal) = last_attempt_event.and_then(|event| event.attempt_event_ordinal)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
    };
    if last_ordinal.checked_add(1) != Some(request.event_ordinal) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(ParkTurnForAgentRunWaitSetOutcome::IdentityConflict));
    }

    let totals = checked_checkpoint_totals(&turn, progress)?;
    let history_order =
        next_tool_history_order_on(&transaction, crate::ChatId(turn.chat_id)).await?;
    let call_model = entities::tool_call::ActiveModel {
        id: Set(wait_id.0),
        chat_id: Set(turn.chat_id),
        turn_id: Set(turn.id),
        provider_id: Set(request.provider_id.clone()),
        history_order: Set(history_order),
        name: Set(WAIT_FOR_AGENTS_TOOL.into()),
        arguments: Set(request.arguments.clone()),
        raw_arguments: Set(None),
        execution: Set(ToolCallExecution::Orchestration.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        result_preview: Set(None),
        provider_replay: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        approval_status: Set(None),
        approval_class: Set(None),
        approval_kind: Set(None),
        approval_reason: Set(None),
        approval_requested_at: Set(None),
        approval_decided_at: Set(None),
        approval_event_seq: Set(None),
        approval_grant_source_call_id: Set(None),
        auto_judge_status: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        turn_lease_token: Set(Some(lease_token)),
        resolution_turn_lease_token: Set(None),
        created_at: Set(now),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
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
        event_ordinal: Set(request.event_ordinal),
        event_seq: Set(None),
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
            open: Set(true),
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
        call: tool_call_from_model(call_model)?,
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
        let result = canonical_wait_for_agents_result(&results)?;
        let call_model = entities::tool_call::Entity::find_by_id(wait_id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("resumed wait {wait_id} lost its tool call"))
            })?;
        if !exact_terminal_wait_call(&call_model, &stored, ToolCallStatus::Completed, &result) {
            return Err(AgentError::Store(format!(
                "resumed wait {wait_id} has an inconsistent tool receipt"
            )));
        }
        let event_model = entities::event::Entity::find_by_id((
            stored.chat_id,
            stored.event_seq.ok_or_else(|| {
                AgentError::Store(format!("resumed wait {wait_id} lost its event receipt"))
            })?,
        ))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("resumed wait {wait_id} lost its event")))?;
        let expected_event = AgentEvent::ToolCallCompleted {
            call_id: wait_id,
            output: crate::ToolOutput::text(result),
            action: None,
            result: None,
        };
        let stored_event: AgentEvent = serde_json::from_value(event_model.payload)?;
        if event_model.turn_id != Some(stored.turn_id)
            || event_model.lease_token != Some(stored.park_lease_token)
            || event_model.attempt_event_ordinal != Some(stored.event_ordinal)
            || event_model.terminal
            || stored_event != expected_event
        {
            return Err(AgentError::Store(format!(
                "resumed wait {wait_id} has an inconsistent event receipt"
            )));
        }
        let outcome = ResumeTurnForAgentRunWaitSetOutcome::Existing {
            turn: turn_run_from_model(turn)?,
            call: tool_call_from_model(call_model)?,
            wait: wait_from_models(stored, members)?,
            results,
            event: SequencedEvent {
                seq: event_model.seq,
                event: stored_event,
            },
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
        parent.tier == AgentRunTier::Foreground.as_str()
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

    let canonical_result = canonical_wait_for_agents_result(&results)?;
    for entry in &results {
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
    let resolved_call = entities::tool_call::Entity::update_many()
        .col_expr(
            entities::tool_call::Column::Status,
            sea_orm::sea_query::Expr::value(ToolCallStatus::Completed.as_str()),
        )
        .col_expr(
            entities::tool_call::Column::Result,
            sea_orm::sea_query::Expr::value(Some(canonical_result.clone())),
        )
        .col_expr(
            entities::tool_call::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(transition_at)),
        )
        .filter(entities::tool_call::Column::Id.eq(wait_id.0))
        .filter(entities::tool_call::Column::ChatId.eq(stored.chat_id))
        .filter(entities::tool_call::Column::TurnId.eq(stored.turn_id))
        .filter(
            entities::tool_call::Column::Execution.eq(ToolCallExecution::Orchestration.as_str()),
        )
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .filter(entities::tool_call::Column::Result.is_null())
        .filter(entities::tool_call::Column::ResolvedAt.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if resolved_call.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let payload = AgentEvent::ToolCallCompleted {
        call_id: wait_id,
        output: crate::ToolOutput::text(canonical_result),
        action: None,
        result: None,
    };
    let event_seq = append_event_on(
        &transaction,
        crate::ChatId(stored.chat_id),
        Some(TurnId(stored.turn_id)),
        Some(stored.park_lease_token),
        Some(stored.event_ordinal),
        None,
        &payload,
    )
    .await?;
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
        .col_expr(
            entities::turn_agent_run_wait_set::Column::EventSeq,
            sea_orm::sea_query::Expr::value(Some(event_seq)),
        )
        .filter(entities::turn_agent_run_wait_set::Column::Id.eq(wait_id.0))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    let closed_members = entities::turn_agent_run_wait_member::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_member::Column::Open,
            sea_orm::sea_query::Expr::value(false),
        )
        .filter(entities::turn_agent_run_wait_member::Column::WaitId.eq(wait_id.0))
        .filter(entities::turn_agent_run_wait_member::Column::Open.eq(true))
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
    if closed.rows_affected != 1
        || closed_members.rows_affected != members.len() as u64
        || resumed.rows_affected != 1
    {
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
    let members = load_members(&transaction, wait_id).await?;
    let call_model = entities::tool_call::Entity::find_by_id(wait_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("resumed wait tool exists");
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
        call: tool_call_from_model(call_model)?,
        wait: wait_from_models(stored, members)?,
        results: consumed_results,
        event: SequencedEvent {
            seq: event_seq,
            event: payload,
        },
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
    close_wait_set_on(conn, &wait, &members, WAIT_CANCELLED_WITH_TURN_RESULT, now).await
}

/// Interrupt only the new ordered wait path when a pending steer wakes it.
/// Children and delivered inboxes deliberately remain untouched so a later
/// model call may wait on the same still-owned children again.
pub(in crate::db) async fn interrupt_wait_set_for_steer_on<C>(
    conn: &C,
    turn: &entities::turn_run::Model,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(wait) = entities::turn_agent_run_wait_set::Entity::find()
        .filter(entities::turn_agent_run_wait_set::Column::TurnId.eq(turn.id))
        .filter(
            entities::turn_agent_run_wait_set::Column::Status
                .eq(TurnAgentRunWaitStatus::Waiting.as_str()),
        )
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if wait.chat_id != turn.chat_id
        || wait.parent_run_id != turn.agent_run_id
        || wait.attempt_count != turn.attempt_count
        || wait.claim_count != turn.claim_count
        || wait.closed_at.is_some()
        || wait.resume_token.is_some()
        || wait.event_seq.is_some()
    {
        return Err(AgentError::Store(format!(
            "waiting turn {} has a mismatched ordered wait receipt",
            TurnId(turn.id)
        )));
    }
    let members = load_members(conn, CallId(wait.id)).await?;
    let _validated = wait_from_models(wait.clone(), members.clone())?;
    close_wait_set_on(conn, &wait, &members, WAIT_INTERRUPTED_BY_STEER_RESULT, now).await
}

async fn close_wait_set_on<C>(
    conn: &C,
    wait: &entities::turn_agent_run_wait_set::Model,
    members: &[entities::turn_agent_run_wait_member::Model],
    result: &str,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: sea_orm::ConnectionTrait,
{
    let call = entities::tool_call::Entity::find_by_id(wait.id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("wait {} lost its tool call", CallId(wait.id))))?;
    if !exact_pending_wait_call_model(&call, wait) {
        return Err(AgentError::Store(format!(
            "wait {} has an invalid pending tool call",
            CallId(wait.id)
        )));
    }
    let transition_at = std::cmp::max(now, wait.parked_at);
    let terminalized = entities::tool_call::Entity::update_many()
        .col_expr(
            entities::tool_call::Column::Status,
            sea_orm::sea_query::Expr::value(ToolCallStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::tool_call::Column::Result,
            sea_orm::sea_query::Expr::value(Some(result.to_owned())),
        )
        .col_expr(
            entities::tool_call::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(transition_at)),
        )
        .filter(entities::tool_call::Column::Id.eq(wait.id))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if terminalized.rows_affected != 1 {
        return Ok(false);
    }
    let payload = AgentEvent::ToolCallCompleted {
        call_id: CallId(wait.id),
        output: crate::ToolOutput::error(result),
        action: None,
        result: None,
    };
    let event_seq = append_event_on(
        conn,
        crate::ChatId(wait.chat_id),
        Some(TurnId(wait.turn_id)),
        Some(wait.park_lease_token),
        Some(wait.event_ordinal),
        None,
        &payload,
    )
    .await?;
    let closed = entities::turn_agent_run_wait_set::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_set::Column::Status,
            sea_orm::sea_query::Expr::value(TurnAgentRunWaitStatus::Cancelled.as_str()),
        )
        .col_expr(
            entities::turn_agent_run_wait_set::Column::ClosedAt,
            sea_orm::sea_query::Expr::value(Some(std::cmp::max(now, wait.parked_at))),
        )
        .col_expr(
            entities::turn_agent_run_wait_set::Column::EventSeq,
            sea_orm::sea_query::Expr::value(Some(event_seq)),
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
    let closed_members = entities::turn_agent_run_wait_member::Entity::update_many()
        .col_expr(
            entities::turn_agent_run_wait_member::Column::Open,
            sea_orm::sea_query::Expr::value(false),
        )
        .filter(entities::turn_agent_run_wait_member::Column::WaitId.eq(wait.id))
        .filter(entities::turn_agent_run_wait_member::Column::Open.eq(true))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if closed.rows_affected != 1 || closed_members.rows_affected != members.len() as u64 {
        // The tool call and its journal event have already been written at
        // this point. Returning `false` would let callers commit those writes
        // while leaving the wait open, so force the surrounding transaction
        // to roll the entire close attempt back instead.
        return Err(AgentError::Store(format!(
            "wait {} changed while closing its ordered receipt",
            CallId(wait.id)
        )));
    }
    Ok(true)
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

fn validate_park_request(request: &AgentRunWaitSetCheckpointRequest) -> Result<()> {
    let wait_id = request.call_id;
    let turn_id = request.origin_turn_id;
    let child_run_ids = request.child_run_ids.as_slice();
    let lease_token = request.lease_token;
    let expected_steer_revision = request.expected_steer_revision;
    let progress = request.progress;
    let unique = child_run_ids.iter().map(|id| id.0).collect::<HashSet<_>>();
    let labels_valid = !request.provider_id.is_empty()
        && request.provider_id.len() <= ToolCallRecord::MAX_LABEL_LEN
        && !request.provider_id.contains('\0');
    let arguments = serde_json::from_value::<WaitForAgentsArgs>(request.arguments.clone())
        .ok()
        .filter(WaitForAgentsArgs::is_well_formed);
    if wait_id.0.is_nil()
        || turn_id.0.is_nil()
        || lease_token.is_nil()
        || expected_steer_revision < 0
        || progress.model_steps <= 0
        || child_run_ids.is_empty()
        || child_run_ids.len() > TurnAgentRunWaitSet::MAX_CHILDREN
        || unique.len() != child_run_ids.len()
        || child_run_ids.iter().any(|id| id.0.is_nil())
        || request.condition != AgentRunWaitCondition::All
        || !labels_valid
        || !(2..i32::MAX).contains(&request.event_ordinal)
        || !serde_json::to_vec(&request.arguments)
            .is_ok_and(|bytes| bytes.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES)
        || arguments.as_ref().is_none_or(|arguments| {
            arguments.agent_ids != child_run_ids
                || serde_json::to_value(arguments).ok().as_ref() != Some(&request.arguments)
        })
    {
        return Err(AgentError::Store(
            "invalid ordered sandbox-child wait request".into(),
        ));
    }
    Ok(())
}

pub(super) async fn load_members<C>(
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

pub(super) async fn acquire_wait_set_lock<C>(conn: &C) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    crate::db::ops::acquire_advisory_lock(conn, crate::db::ops::AdvisoryLockName::TurnAgentRunWait)
        .await
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
                || member.open != (wait.status == TurnAgentRunWaitStatus::Waiting.as_str())
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
        event_ordinal: wait.event_ordinal,
        event_seq: wait.event_seq,
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
