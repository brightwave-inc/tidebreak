//! Settling one parked `code_wait` call.
//!
//! The call is an orchestration row, like the questions card: no leased
//! client executes it, the server does. When every child the parent named has
//! settled, the server writes the ordered result onto the same call and the
//! parked turn becomes resumable. Nothing about the wait is copied into a
//! second place, so a restarted server settles it from the children's own
//! rows.

use chrono::{DateTime, Utc};
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedAgentEvent};
use crate::model::{ToolCallExecution, ToolCallRecord, ToolCallStatus, TurnRunStatus};
use crate::storage::SettleChildSessionWaitOutcome;
use crate::{CallId, SessionId, TurnId, CODE_WAIT_TOOL};

use super::super::{entities, store_err, DbStore};
use super::turn::{
    advance_turn_after_client_resolution_on, canonical_db_timestamp,
    recover_turn_after_client_resolution_on,
};
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// Complete one parked child-session wait with the ordered child results.
///
/// `result` is the same payload `code_wait` returns when it answers inline,
/// so the model reads one shape whether the call settled in twenty seconds or
/// across a restart. An exact retry recovers the first commit; a retry
/// carrying a different result is refused rather than overwriting it.
pub(in crate::db) async fn settle(
    store: &DbStore,
    chat_id: SessionId,
    call_id: CallId,
    result: &str,
    settled_at: DateTime<Utc>,
) -> Result<SettleChildSessionWaitOutcome> {
    if chat_id.0.is_nil() || call_id.0.is_nil() || result.len() > ToolCallRecord::MAX_RESULT_BYTES {
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    let requested_at = canonical_db_timestamp(settled_at)?;
    let Some(scope) = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    };
    if scope.chat_id != chat_id.0 {
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await?
        || !acquire_turn_write_lock(&transaction, TurnId(scope.turn_id)).await?
        || !acquire_tool_call_write_lock(&transaction, call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    let call = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked child-session wait call exists");
    if call.chat_id != chat_id.0
        || call.name != CODE_WAIT_TOOL
        || call.execution != ToolCallExecution::Orchestration.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    if call.status == ToolCallStatus::Completed.as_str() {
        if call.result.as_deref() != Some(result) {
            transaction.commit().await.map_err(store_err)?;
            return Ok(SettleChildSessionWaitOutcome::ResultConflict);
        }
        let transition = recover_turn_after_client_resolution_on(&transaction, &call)
            .await?
            .ok_or_else(|| {
                AgentError::Store(format!("settled child wait {call_id} is missing its wait"))
            })?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(SettleChildSessionWaitOutcome::Existing(transition.turn));
    }
    if call.status != ToolCallStatus::Pending.as_str() || call.client_executor_id.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    let turn = entities::turn::Entity::find_by_id(call.turn_id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked child-session wait turn exists");
    let adapter_park_call = super::turn::adapter_client_park_call_id(&turn)?;
    if turn.session_id != chat_id.0
        || (turn.status != TurnRunStatus::WaitingForClient.as_str()
            && adapter_park_call != Some(call_id))
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(SettleChildSessionWaitOutcome::Unavailable);
    }
    let database_now = super::agent_run::database_now(&transaction).await?;
    let settled_at = requested_at
        .max(database_now)
        .max(call.created_at)
        .max(turn.updated_at.unwrap_or(database_now));

    let preview = crate::ToolResultPreview::build(&call.name, &crate::ToolOutput::text(result));
    let mut active: entities::tool_call::ActiveModel = call.into();
    active.status = Set(ToolCallStatus::Completed.as_str().into());
    active.result = Set(Some(result.to_owned()));
    active.result_preview = Set(match &preview {
        Some(preview) => Some(serde_json::to_value(preview)?),
        None => None,
    });
    active.error_code = Set(None);
    active.error_detail = Set(None);
    active.resolved_at = Set(Some(settled_at));
    let resolved = active.update(&transaction).await.map_err(store_err)?;

    // The wait resolves outside the agent loop, so nothing else announces
    // that the call finished. Journaled in the transaction that makes the row
    // terminal, so the event cannot disagree with the row it describes.
    let completion_event = AgentEvent::ToolCallCompleted {
        call_id,
        output: crate::ToolOutput::text(result.to_owned()),
        action: crate::ToolActionPreview::build(&resolved.name, &resolved.arguments),
        result: preview,
    };
    let seq = super::conversation::append_event_on(
        &transaction,
        SessionId(resolved.chat_id),
        None,
        None,
        None,
        None,
        &completion_event,
    )
    .await?;
    let transition = advance_turn_after_client_resolution_on(&transaction, &resolved, settled_at)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!("settled child wait {call_id} is missing its wait"))
        })?;
    transaction.commit().await.map_err(store_err)?;
    Ok(SettleChildSessionWaitOutcome::Settled {
        turn: transition.turn,
        completion_event: Box::new(SequencedAgentEvent {
            seq,
            event: completion_event,
        }),
    })
}
