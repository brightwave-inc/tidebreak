//! Opaque cross-conversation prompt summaries for shell-level notification.

use std::collections::{BTreeMap, HashMap};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::error::Result;
use crate::model::{ToolCallExecution, ToolCallStatus, TurnClientWaitStatus, TurnRunStatus};
use crate::storage::{InboxItemKind, PendingChatPrompt};
use crate::{
    validate_request_folder_access_arguments, validate_write_output_to_connected_folder_arguments,
    CallId, ChatId, TurnId, ASK_USER_QUESTIONS_TOOL, EXIT_PLAN_MODE_TOOL,
    REQUEST_FOLDER_ACCESS_TOOL, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};

use super::super::{entities, store_err, DbStore};

/// One renderer-owned prompt that is parked, before it is projected for a
/// particular caller. Rows arrive grouped by kind and ordered within each.
pub(in crate::db) struct PendingPromptRow {
    pub chat_id: ChatId,
    pub turn_id: TurnId,
    pub call_id: CallId,
    pub kind: InboxItemKind,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// Fold prompt rows into the per-conversation attention summary.
pub(in crate::db) async fn list_pending_chat_prompts(
    store: &DbStore,
) -> Result<Vec<PendingChatPrompt>> {
    let mut prompts = BTreeMap::<uuid::Uuid, PendingChatPrompt>::new();
    for row in list_pending_prompt_rows(store).await? {
        let entry = prompts
            .entry(row.chat_id.0)
            .or_insert_with(|| PendingChatPrompt {
                chat_id: row.chat_id,
                question_call_ids: Vec::new(),
                plan_call_ids: Vec::new(),
                folder_access_call_ids: Vec::new(),
                output_writeback_call_ids: Vec::new(),
            });
        match row.kind {
            InboxItemKind::Question => entry.question_call_ids.push(row.call_id),
            InboxItemKind::PlanReview => entry.plan_call_ids.push(row.call_id),
            InboxItemKind::FolderAccess => entry.folder_access_call_ids.push(row.call_id),
            InboxItemKind::OutputWriteback => entry.output_writeback_call_ids.push(row.call_id),
            // Approvals park on the tool-call gate rather than on a
            // renderer-owned prompt, so they are never in this set.
            InboxItemKind::ToolApproval => {}
        }
    }
    Ok(prompts.into_values().collect())
}

/// Read the minimal, durable prompt state the desktop needs to find chats that
/// need attention. This deliberately uses set queries rather than replaying
/// every chat's prompt endpoint from the shell.
pub(in crate::db) async fn list_pending_prompt_rows(
    store: &DbStore,
) -> Result<Vec<PendingPromptRow>> {
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

    let mut rows = Vec::<PendingPromptRow>::new();
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
        rows.push(PendingPromptRow {
            chat_id: ChatId(request.chat_id),
            turn_id: TurnId(request.turn_id),
            call_id: CallId(request.call_id),
            kind: InboxItemKind::Question,
            requested_at: request.asked_at,
        });
    }

    let plan_requests = entities::plan_request::Entity::find()
        .filter(
            entities::plan_request::Column::Status.eq(crate::PlanRequestStatus::Pending.as_str()),
        )
        .order_by_asc(entities::plan_request::Column::ChatId)
        .order_by_asc(entities::plan_request::Column::ProposedAt)
        .order_by_asc(entities::plan_request::Column::CallId)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let plan_calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::Name.eq(EXIT_PLAN_MODE_TOOL))
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
    for request in plan_requests {
        let Some(call) = plan_calls.get(&request.call_id) else {
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
        rows.push(PendingPromptRow {
            chat_id: ChatId(request.chat_id),
            turn_id: TurnId(request.turn_id),
            call_id: CallId(request.call_id),
            kind: InboxItemKind::PlanReview,
            requested_at: request.proposed_at,
        });
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
        rows.push(PendingPromptRow {
            chat_id: ChatId(call.chat_id),
            turn_id: TurnId(call.turn_id),
            call_id: CallId(call.id),
            kind: InboxItemKind::FolderAccess,
            requested_at: call.created_at,
        });
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
        rows.push(PendingPromptRow {
            chat_id: ChatId(call.chat_id),
            turn_id: TurnId(call.turn_id),
            call_id: CallId(call.id),
            kind: InboxItemKind::OutputWriteback,
            requested_at: call.created_at,
        });
    }

    Ok(rows)
}
