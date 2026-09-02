//! The cross-conversation read model of everything waiting on one reader.

use std::collections::{HashMap, HashSet};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::approval::ToolApprovalStatus;
use crate::error::Result;
use crate::model::OwnerId;
use crate::storage::{InboxItem, InboxItemKind};
use crate::{CallId, ChatId, TurnId};

use super::super::{entities, store_err, DbStore};
use super::conversation::internal_sessions;

/// The most items one read will return, newest waits dropped first.
///
/// A reader with more than this waiting has a backlog problem the inbox cannot
/// solve by listing further, and an unbounded cross-chat read is the kind of
/// query that degrades quietly as a profile ages.
const MAX_ITEMS: usize = 200;

/// Project every parked row that belongs to `owner` into one ordered list.
///
/// Nothing here writes: the same rows the per-chat recovery routes read are
/// the ones projected, so an item leaves this list exactly when its own
/// resolution path commits.
pub(in crate::db) async fn list_inbox_items(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<InboxItem>> {
    // The owner's chats are both the scope filter and the source of titles, so
    // an item can never name a conversation the reader may not see.
    let chats = entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(internal_sessions())
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|chat| (chat.id, chat.title))
        .collect::<HashMap<_, _>>();

    let mut items = Vec::<InboxItem>::new();
    // A call parked on its approval gate is one decision, even when the tool
    // behind it also has a renderer prompt of its own. The approval is what the
    // reader is actually being asked, so it claims the call.
    let mut approval_call_ids = HashSet::new();

    let approvals = entities::tool_call::Entity::find()
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::ApprovalRequestedAt)
        .order_by_asc(entities::tool_call::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    for call in approvals {
        let Some(title) = chats.get(&call.chat_id) else {
            continue;
        };
        // A pending approval without a request timestamp is a row mid-write or
        // one written by an older build; it has no place in a time-ordered
        // list, and the chat's own approval route still recovers it.
        let Some(requested_at) = call.approval_requested_at else {
            continue;
        };
        approval_call_ids.insert(call.id);
        items.push(InboxItem {
            chat_id: ChatId(call.chat_id),
            chat_title: title.clone(),
            turn_id: TurnId(call.turn_id),
            call_id: CallId(call.id),
            kind: InboxItemKind::ToolApproval,
            tool_name: Some(call.name),
            requested_at,
        });
    }

    for row in super::chat_prompt::list_pending_prompt_rows(store).await? {
        let Some(title) = chats.get(&row.chat_id.0) else {
            continue;
        };
        if approval_call_ids.contains(&row.call_id.0) {
            continue;
        }
        items.push(InboxItem {
            chat_id: row.chat_id,
            chat_title: title.clone(),
            turn_id: row.turn_id,
            call_id: row.call_id,
            kind: row.kind,
            tool_name: None,
            requested_at: row.requested_at,
        });
    }

    // Oldest first: the thing that has been blocked longest is the thing the
    // reader most needs to see, and it is also what survives the cap.
    items.sort_by(|left, right| {
        left.requested_at
            .cmp(&right.requested_at)
            .then_with(|| left.call_id.0.cmp(&right.call_id.0))
    });
    items.truncate(MAX_ITEMS);
    Ok(items)
}
