use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::storage::TurnLeaseFence;
use crate::{
    AgentRunId, AgentRunTaskPlan, CallId, SessionId, TaskPlan, TaskPlanStep, TurnId,
    UPDATE_TASK_PLAN_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::turn::{canonical_db_timestamp, turn_lease_is_current_on};
use super::{acquire_chat_write_lock, acquire_turn_write_lock, conversation::append_event_on};

/// Replace a chat's task plan and journal the refresh hint in one transaction.
///
/// `Ok(None)` means the write was declined rather than that it failed: the
/// call's attempt no longer owns its turn. That is an ordinary outcome on a
/// retry path and not a reason to fail anything.
///
/// The owning turn and its lease are read from the tool call rather than passed
/// in: the call's row is admitted before the tool runs, so it is already the
/// authority on which attempt is speaking, and taking the caller's word for it
/// would let a stalled attempt overwrite a newer one's plan.
///
/// The journaled event carries no steps. It only tells a connected renderer to
/// re-read the route, the same way a proposed plan does; a plan rewritten
/// twenty times in a turn must not write twenty copies of itself into the
/// chat's history.
pub(in crate::db) async fn upsert_for_chat(
    store: &DbStore,
    chat_id: SessionId,
    call_id: CallId,
    steps: &[TaskPlanStep],
    updated_at: DateTime<Utc>,
) -> Result<Option<TaskPlan>> {
    let now = canonical_db_timestamp(updated_at)?;
    let encoded = serde_json::to_string(steps)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("chat {chat_id} not found")));
    }
    let call = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!("task plan call {call_id} is not an admitted call"))
        })?;
    if call.chat_id != chat_id.0 || call.name != UPDATE_TASK_PLAN_TOOL {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "task plan call {call_id} does not belong to chat {chat_id}"
        )));
    }
    let turn_id = TurnId(call.turn_id);
    // The same fence every other live-turn journal writer takes. A claimed
    // attempt that lost its lease — because it stalled past expiry and another
    // worker reclaimed the turn — must not commit a plan the current attempt
    // would then be judged by. Legacy unclaimed turns carry no lease and are
    // not fenced, matching the rest of the durable write surface.
    if let Some(lease_token) = call.turn_lease_token {
        if !acquire_turn_write_lock(&transaction, turn_id).await? {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
        if turn_lease_is_current_on(&transaction, turn_id, lease_token, now).await?
            != TurnLeaseFence::Current
        {
            transaction.commit().await.map_err(store_err)?;
            return Ok(None);
        }
    }
    let existing = entities::task_plan::Entity::find_by_id(chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    // One call records one plan. A re-executed call — the loop replaying a row
    // it already admitted — carries the same arguments by construction, so
    // recognizing its id is what keeps the hint from being journaled twice.
    if let Some(existing) = existing.as_ref() {
        if existing.call_id == call_id.0 {
            let plan = projection(existing)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(Some(plan));
        }
    }
    append_event_on(
        &transaction,
        chat_id,
        None,
        None,
        None,
        None,
        &AgentEvent::TaskPlanUpdated { call_id, turn_id },
    )
    .await?;
    let updated_at = match existing {
        Some(existing) => {
            // A retried write must never move `updated_at` backwards past the
            // row's own creation, which the table's check constraint forbids.
            let updated_at = now.max(existing.created_at);
            let mut row: entities::task_plan::ActiveModel = existing.into();
            row.turn_id = Set(turn_id.0);
            row.call_id = Set(call_id.0);
            row.steps = Set(encoded);
            row.updated_at = Set(updated_at);
            row.update(&transaction).await.map_err(store_err)?;
            updated_at
        }
        None => {
            entities::task_plan::ActiveModel {
                chat_id: Set(chat_id.0),
                turn_id: Set(turn_id.0),
                call_id: Set(call_id.0),
                steps: Set(encoded),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(&transaction)
            .await
            .map_err(store_err)?;
            now
        }
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(TaskPlan {
        turn_id,
        steps: steps.to_vec(),
        updated_at,
    }))
}

/// The chat's current plan, or `None` when it never made one.
///
/// A plan outlives the turn that wrote it: a finished turn leaves its steps
/// exactly as they were, which is what makes a completed plan readable as
/// history instead of vanishing with the worker.
pub(in crate::db) async fn get_for_chat(
    store: &DbStore,
    chat_id: SessionId,
) -> Result<Option<TaskPlan>> {
    let Some(row) = entities::task_plan::Entity::find_by_id(chat_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    // A row whose steps no longer parse is one unreadable plan, not a chat the
    // reader can no longer open. Reporting it as "no plan" leaves every other
    // surface working and lets the next call replace it.
    match projection(&row) {
        Ok(plan) => Ok(Some(plan)),
        Err(error) => {
            tracing::warn!(
                chat_id = %chat_id,
                %error,
                "stored task plan could not be read; reporting no plan"
            );
            Ok(None)
        }
    }
}

fn projection(row: &entities::task_plan::Model) -> Result<TaskPlan> {
    Ok(TaskPlan {
        turn_id: TurnId(row.turn_id),
        steps: serde_json::from_str(&row.steps)?,
        updated_at: row.updated_at,
    })
}

/// Replace one background run's task plan inside its resolving transaction.
///
/// The caller is the sandbox checkpoint resolution, which already holds the
/// executor lease and the claim lock, so this takes no fence of its own: a
/// write that reaches here has already proven it owns the call. Committing the
/// plan and the checkpoint's receipt together is the point — a run must never
/// read "plan updated" back from a receipt whose plan write did not land.
///
/// A replayed resolution recognizes its own row by `call_id` and leaves it
/// alone, the same way the chat-scoped plan does.
pub(in crate::db) async fn upsert_for_agent_run_on<C>(
    connection: &C,
    agent_run_id: AgentRunId,
    call_id: CallId,
    steps: &[TaskPlanStep],
    now: DateTime<Utc>,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let encoded = serde_json::to_string(steps)?;
    let existing = entities::agent_run_task_plan::Entity::find_by_id(agent_run_id.0)
        .one(connection)
        .await
        .map_err(store_err)?;
    match existing {
        Some(existing) if existing.call_id == call_id.0 => Ok(()),
        Some(existing) => {
            // A retried write must never move `updated_at` behind the row's
            // own creation, which the table's check constraint forbids.
            let updated_at = now.max(existing.created_at);
            let mut row: entities::agent_run_task_plan::ActiveModel = existing.into();
            row.call_id = Set(call_id.0);
            row.steps = Set(encoded);
            row.updated_at = Set(updated_at);
            row.update(connection).await.map_err(store_err)?;
            Ok(())
        }
        None => {
            entities::agent_run_task_plan::ActiveModel {
                agent_run_id: Set(agent_run_id.0),
                call_id: Set(call_id.0),
                steps: Set(encoded),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(connection)
            .await
            .map_err(store_err)?;
            Ok(())
        }
    }
}

/// One background run's current plan, or `None` when it never made one.
pub(in crate::db) async fn get_for_agent_run(
    store: &DbStore,
    agent_run_id: AgentRunId,
) -> Result<Option<AgentRunTaskPlan>> {
    let Some(row) = entities::agent_run_task_plan::Entity::find_by_id(agent_run_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    // An unreadable plan is one unreadable checklist, not a run the reader can
    // no longer open. Reporting "no plan" keeps every other surface working and
    // lets the run's next call replace it.
    match serde_json::from_str(&row.steps) {
        Ok(steps) => Ok(Some(AgentRunTaskPlan {
            run_id: agent_run_id,
            steps,
            updated_at: row.updated_at,
        })),
        Err(error) => {
            tracing::warn!(
                agent_run_id = %agent_run_id,
                %error,
                "stored sandbox task plan could not be read; reporting no plan"
            );
            Ok(None)
        }
    }
}
