//! Opaque cross-conversation prompt summaries for shell-level notification.

use std::collections::{BTreeMap, HashMap, HashSet};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::error::Result;
use crate::model::{
    OwnerId, ToolCallExecution, ToolCallStatus, TurnClientWaitStatus, TurnRunStatus,
};
use crate::storage::{InboxItemKind, PendingChatPrompt};
use crate::{
    validate_request_folder_access_arguments, validate_write_output_to_connected_folder_arguments,
    CallId, SessionId, TurnId, ASK_USER_QUESTIONS_TOOL, EXIT_PLAN_MODE_TOOL,
    REQUEST_FOLDER_ACCESS_TOOL, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};

use super::super::{entities, store_err, DbStore};
use super::conversation::internal_sessions;

/// One renderer-owned prompt that is parked, before it is projected for a
/// particular caller. Rows arrive grouped by kind and ordered within each.
pub(in crate::db) struct PendingPromptRow {
    pub chat_id: SessionId,
    pub turn_id: TurnId,
    pub call_id: CallId,
    pub kind: InboxItemKind,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// Fold prompt rows into the per-conversation attention summary.
///
/// With an `owner`, the principal's own chats are the scope filter, so a
/// conversation someone else owns cannot appear in the summary — the same
/// derivation [`super::inbox::list_inbox_items`] uses for the cross-chat
/// inbox it shares these rows with.
pub(in crate::db) async fn list_pending_chat_prompts(
    store: &DbStore,
    owner: Option<&OwnerId>,
) -> Result<Vec<PendingChatPrompt>> {
    let visible = match owner {
        Some(owner) => Some(
            entities::session::Entity::find()
                .filter(entities::session::Column::Owner.eq(owner.as_str()))
                .filter(internal_sessions())
                .all(&store.conn)
                .await
                .map_err(store_err)?
                .into_iter()
                .map(|chat| chat.id)
                .collect::<HashSet<_>>(),
        ),
        None => None,
    };

    let mut prompts = BTreeMap::<uuid::Uuid, PendingChatPrompt>::new();
    for row in list_pending_prompt_rows(store).await? {
        if visible
            .as_ref()
            .is_some_and(|chat_ids| !chat_ids.contains(&row.chat_id.0))
        {
            continue;
        }
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
    // Every pending park is an approval row; its kind says which card it is
    // (decision 0048 step 5). Consent cards carry the engine's tool request
    // instead and belong to the inbox's approval projection.
    let parks = entities::approval::Entity::find()
        .filter(entities::approval::Column::State.eq(crate::code::ApprovalState::Pending.as_str()))
        .order_by_asc(entities::approval::Column::SessionId)
        .order_by_asc(entities::approval::Column::RequestedAt)
        .order_by_asc(entities::approval::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut question_requests = Vec::new();
    let mut plan_requests = Vec::new();
    for row in parks {
        match serde_json::from_value::<crate::code::ApprovalKind>(row.kind.clone()) {
            Ok(crate::code::ApprovalKind::Questions { .. }) => question_requests.push(row),
            Ok(crate::code::ApprovalKind::Plan { .. }) => plan_requests.push(row),
            _ => {}
        }
    }

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
    let turns = entities::turn::Entity::find()
        .filter(entities::turn::Column::Status.eq(TurnRunStatus::WaitingForClient.as_str()))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|turn| (turn.id, turn))
        .collect::<HashMap<_, _>>();

    let mut rows = Vec::<PendingPromptRow>::new();
    for request in question_requests {
        let Some(call) = question_calls.get(&request.id) else {
            continue;
        };
        let Some(wait) = waits.get(&request.id) else {
            continue;
        };
        let Some(turn) = turns.get(&request.turn_id) else {
            continue;
        };
        if call.chat_id != request.session_id
            || call.turn_id != request.turn_id
            || call.client_executor_id.is_some()
            || wait.session_id != request.session_id
            || wait.turn_id != request.turn_id
            || turn.session_id != request.session_id
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            continue;
        }
        rows.push(PendingPromptRow {
            chat_id: SessionId(request.session_id),
            turn_id: TurnId(request.turn_id),
            call_id: CallId(request.id),
            kind: InboxItemKind::Question,
            requested_at: request.requested_at,
        });
    }

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
        let Some(call) = plan_calls.get(&request.id) else {
            continue;
        };
        let Some(wait) = waits.get(&request.id) else {
            continue;
        };
        let Some(turn) = turns.get(&request.turn_id) else {
            continue;
        };
        if call.chat_id != request.session_id
            || call.turn_id != request.turn_id
            || call.client_executor_id.is_some()
            || wait.session_id != request.session_id
            || wait.turn_id != request.turn_id
            || turn.session_id != request.session_id
            || turn.attempt_count != wait.attempt_count
            || turn.claim_count != wait.claim_count
        {
            continue;
        }
        rows.push(PendingPromptRow {
            chat_id: SessionId(request.session_id),
            turn_id: TurnId(request.turn_id),
            call_id: CallId(request.id),
            kind: InboxItemKind::PlanReview,
            requested_at: request.requested_at,
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
            chat_id: SessionId(call.chat_id),
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
            chat_id: SessionId(call.chat_id),
            turn_id: TurnId(call.turn_id),
            call_id: CallId(call.id),
            kind: InboxItemKind::OutputWriteback,
            requested_at: call.created_at,
        });
    }

    Ok(rows)
}
