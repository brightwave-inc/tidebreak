//! Opaque cross-conversation prompt summaries for shell-level notification.

use std::collections::{BTreeMap, HashMap};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::error::Result;
use crate::model::{ToolCallExecution, ToolCallStatus, TurnClientWaitStatus, TurnRunStatus};
use crate::storage::PendingChatPrompt;
use crate::{
    validate_request_folder_access_arguments, validate_write_output_to_connected_folder_arguments,
    CallId, ChatId, ASK_USER_QUESTIONS_TOOL, REQUEST_FOLDER_ACCESS_TOOL,
    WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};

use super::super::{entities, store_err, DbStore};

/// Read the minimal, durable prompt state the desktop needs to find chats that
/// need attention. This deliberately uses set queries rather than replaying
/// every chat's prompt endpoint from the shell.
pub(in crate::db) async fn list_pending_chat_prompts(
    store: &DbStore,
) -> Result<Vec<PendingChatPrompt>> {
    let question_requests = entities::user_question_request::Entity::find()
        .filter(
            entities::user_question_request::Column::Status
                .eq(crate::UserQuestionRequestStatus::Pending.as_str()),
        )
        .order_by_asc(entities::user_question_request::Column::ChatId)
        .order_by_asc(entities::user_question_request::Column::AskedAt)
        .order_by_asc(entities::user_question_request::Column::CallId)
        .all(&store.conn)
        .await
        .map_err(store_err)?;

    // A question request is an auxiliary renderer projection. Keep the same
    // live-continuation checks as its detailed recovery path so an incomplete
    // or stale projection cannot leave a chat marked as waiting forever.
    let question_calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::Name.eq(ASK_USER_QUESTIONS_TOOL))
        .filter(
            entities::tool_call::Column::Execution.eq(ToolCallExecution::Orchestration.as_str()),
        )
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|call| (call.id, call))
        .collect::<HashMap<_, _>>();
    let waits = entities::turn_client_wait::Entity::find()
        .filter(
            entities::turn_client_wait::Column::Status.eq(TurnClientWaitStatus::Waiting.as_str()),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|wait| (wait.call_id, wait))
        .collect::<HashMap<_, _>>();
    let turns = entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::Status.eq(TurnRunStatus::WaitingForClient.as_str()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|turn| (turn.id, turn))
        .collect::<HashMap<_, _>>();

    let mut prompts = BTreeMap::<uuid::Uuid, PendingChatPrompt>::new();
    for request in question_requests {
        let Some(call) = question_calls.get(&request.call_id) else {
            continue;
        };
        let Some(wait) = waits.get(&request.call_id) else {
            continue;
        };
        let Some(turn) = turns.get(&request.turn_id) else {
            continue;
        };
        if call.chat_id != request.chat_id
            || call.turn_id != request.turn_id
            || call.client_executor_id.is_some()
            || wait.chat_id != request.chat_id
            || wait.turn_id != request.turn_id
            || turn.chat_id != request.chat_id
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            continue;
        }
        prompts
            .entry(request.chat_id)
            .or_insert_with(|| PendingChatPrompt {
                chat_id: ChatId(request.chat_id),
                question_call_ids: Vec::new(),
                folder_access_call_ids: Vec::new(),
                output_writeback_call_ids: Vec::new(),
            })
            .question_call_ids
            .push(CallId(request.call_id));
    }

    let folder_calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::Name.eq(REQUEST_FOLDER_ACCESS_TOOL))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::ChatId)
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for call in folder_calls {
        if !validate_request_folder_access_arguments(&call.arguments) {
            continue;
        }
        prompts
            .entry(call.chat_id)
            .or_insert_with(|| PendingChatPrompt {
                chat_id: ChatId(call.chat_id),
                question_call_ids: Vec::new(),
                folder_access_call_ids: Vec::new(),
                output_writeback_call_ids: Vec::new(),
            })
            .folder_access_call_ids
            .push(CallId(call.id));
    }

    let writeback_calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::Name.eq(WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::ChatId)
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for call in writeback_calls {
        if !validate_write_output_to_connected_folder_arguments(&call.arguments) {
            continue;
        }
        prompts
            .entry(call.chat_id)
            .or_insert_with(|| PendingChatPrompt {
                chat_id: ChatId(call.chat_id),
                question_call_ids: Vec::new(),
                folder_access_call_ids: Vec::new(),
                output_writeback_call_ids: Vec::new(),
            })
            .output_writeback_call_ids
            .push(CallId(call.id));
    }

    Ok(prompts.into_values().collect())
}
