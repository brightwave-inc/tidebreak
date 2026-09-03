//! A chat's attention, derived from the rows that already describe it.
//!
//! Decision 48 step 3 gives every conversation the attention vocabulary code
//! mode introduced, so one supervising client watches one queue. Code stores
//! its attention on `code_session`, because a session is the unit and its
//! lifecycle lives on the same row. Chat has no such row: a conversation's
//! liveness is spread across `code_turn`, and its waiting work is the same set
//! the inbox already projects.
//!
//! So chat derives rather than stores. Adding attention columns to `chat`
//! would duplicate state `turn_run` owns and need a write at every turn
//! transition to stay true — the shape issue #1462 spent a wave removing. The
//! inbox next door already states the principle: a projection of rows that
//! carry the state, "deliberately not a store of its own, so resolving an item
//! through its existing route is what removes it from here."
//!
//! Chat never produces [`AttentionState::DoneUnreviewed`] or
//! [`AttentionState::Fenced`]. Reviewed-ness is a code-mode notion carried by
//! an attached events socket, and there is no foreign process to fence. A
//! shared vocabulary does not oblige every engine to use all of it.

use std::collections::HashMap;

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::attention::{Attention, AttentionSource, AttentionState};
use crate::code::CodeTurnStatus;
use crate::error::Result;
use crate::model::OwnerId;
use crate::storage::{InboxItem, InboxItemKind};
use crate::ChatId;

use super::super::{entities, store_err, DbStore};
use super::conversation::internal_sessions;

/// What the reader is being asked, for a badge that must stay opaque.
///
/// Kind only — never question text, plan prose, or tool arguments. The inbox
/// holds that line deliberately, and an attention prompt is shown in more
/// places than the inbox is.
fn needs_you_prompt(kind: InboxItemKind) -> &'static str {
    match kind {
        InboxItemKind::ToolApproval => "a tool call is waiting for approval",
        InboxItemKind::Question => "a question is waiting for an answer",
        InboxItemKind::PlanReview => "a plan is waiting for review",
        InboxItemKind::FolderAccess => "a folder request is waiting",
        InboxItemKind::OutputWriteback => "a file write is waiting for confirmation",
    }
}

/// Derive one attention value per chat that has anything to say.
///
/// Chats absent from the map are [`AttentionState::Idle`]; materializing a
/// row per idle conversation would make the cost scale with history rather
/// than with what is happening now.
pub(in crate::db) async fn chat_attention(
    store: &DbStore,
    owner: &OwnerId,
    items: &[InboxItem],
) -> Result<HashMap<ChatId, Attention>> {
    let mut attention = HashMap::<ChatId, Attention>::new();

    // A live turn is the weaker claim, so it goes first and a waiting item
    // overwrites it below. A conversation that is both running and holding an
    // approval is, to the reader, waiting on them.
    let live = entities::code_turn::Entity::find()
        .filter(
            entities::code_turn::Column::Status
                .is_in(CodeTurnStatus::LIVE.iter().map(|status| status.as_str())),
        )
        .order_by_asc(entities::code_turn::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let owned = entities::code_session::Entity::find()
        .filter(entities::code_session::Column::Owner.eq(owner.as_str()))
        .filter(internal_sessions())
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|chat| chat.id)
        .collect::<std::collections::HashSet<_>>();
    for run in live {
        if !owned.contains(&run.session_id) {
            continue;
        }
        attention.insert(
            ChatId(run.session_id),
            Attention::working(AttentionSource::Lifecycle),
        );
    }

    // `items` is already owner-scoped and oldest-first, so the first item for
    // a chat is the one that has been waiting longest — which is the one the
    // badge should name.
    for item in items {
        match attention.get(&item.chat_id) {
            // Already naming an older wait: the badge keeps the longest-waiting
            // one, which is what `items` ordering already decided.
            Some(existing) if matches!(existing.state, AttentionState::NeedsYou { .. }) => continue,
            _ => {
                attention.insert(
                    item.chat_id,
                    Attention::needs_you(needs_you_prompt(item.kind), AttentionSource::Structured),
                );
            }
        }
    }

    Ok(attention)
}
