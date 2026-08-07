use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

use crate::error::{AgentError, Result};
use crate::event::AgentEvent;
use crate::{CallId, ChatId, TaskPlan, TaskPlanStep, TurnId, UPDATE_TASK_PLAN_TOOL};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, conversation::append_event_on};

/// Replace a chat's task plan and journal the refresh hint in one transaction.
///
/// The owning turn is read from the tool call rather than passed in: the call's
/// row is admitted before the tool runs, so it is already the authority on
/// which turn is speaking, and taking the caller's word for it would let a
/// mis-scoped call write a plan against a turn it does not belong to.
///
/// The journaled event carries no steps. It only tells a connected renderer to
/// re-read the route, the same way a proposed plan does; a plan rewritten
/// twenty times in a turn must not write twenty copies of itself into the
/// chat's history.
pub(in crate::db) async fn upsert_for_chat(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    steps: &[TaskPlanStep],
    updated_at: DateTime<Utc>,
) -> Result<TaskPlan> {
    let updated_at = canonical_db_timestamp(updated_at)?;
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
    match entities::task_plan::Entity::find_by_id(chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        Some(existing) => {
            let created_at = existing.created_at;
            let mut row: entities::task_plan::ActiveModel = existing.into();
            row.turn_id = Set(turn_id.0);
            row.steps = Set(encoded);
            // A retried write must never move `updated_at` backwards past the
            // row's own creation, which the table's check constraint forbids.
            row.updated_at = Set(updated_at.max(created_at));
            row.update(&transaction).await.map_err(store_err)?;
        }
        None => {
            entities::task_plan::ActiveModel {
                chat_id: Set(chat_id.0),
                turn_id: Set(turn_id.0),
                steps: Set(encoded),
                created_at: Set(updated_at),
                updated_at: Set(updated_at),
            }
            .insert(&transaction)
            .await
            .map_err(store_err)?;
        }
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(TaskPlan {
        turn_id,
        steps: steps.to_vec(),
        updated_at,
    })
}

/// The chat's current plan, or `None` when it never made one.
///
/// A plan outlives the turn that wrote it: a finished turn leaves its steps
/// exactly as they were, which is what makes a completed plan readable as
/// history instead of vanishing with the worker.
pub(in crate::db) async fn get_for_chat(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Option<TaskPlan>> {
    let Some(row) = entities::task_plan::Entity::find_by_id(chat_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let steps: Vec<TaskPlanStep> = serde_json::from_str(&row.steps)?;
    Ok(Some(TaskPlan {
        turn_id: TurnId(row.turn_id),
        steps,
        updated_at: row.updated_at,
    }))
}
