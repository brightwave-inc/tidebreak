//! The single list of what is waiting on the reader, across their chats.
//!
//! A read model, not a store: every row here is projected from the journal
//! state the per-chat recovery routes already serve, and each item is resolved
//! through that same chat's existing route — the approval gate, the question
//! answer, the plan decision, the native folder grant. Resolving is therefore
//! first-responder by construction: whoever commits first wins, and a second
//! attempt meets the same conflict the in-chat card would have met.
//!
//! Items stay as opaque as the per-chat attention summary. Identity, kind, the
//! conversation's title, and when it parked are enough to triage; question
//! prose, plan text, and canonical tool arguments stay behind the chat the
//! item deep-links to.

use openwave_core::{CallId, ChatId, InboxItemKind, TurnId};
use serde::Serialize;

use crate::error::ServerError;
use crate::extract::Json;
use crate::scoped_store::ScopedStore;

/// One item waiting on the reader, and where to go to answer it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct InboxItemSnapshot {
    /// The conversation to open. With `call_id`, the deep link back to the
    /// exact transcript position the item paused at.
    pub chat_id: ChatId,
    /// Absent, not null, while the conversation is still untitled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chat_title: Option<String>,
    pub turn_id: TurnId,
    pub call_id: CallId,
    pub kind: InboxItemKind,
    /// The tool under review, for an approval. Absent for the other kinds,
    /// whose tool is implied by the kind. Closed renderer vocabulary: an
    /// unrecognized name folds to `other` rather than reaching a card.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action: Option<openwave_core::RendererToolName>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /inbox` — everything parked on the reader, oldest first.
pub async fn list_inbox(store: ScopedStore) -> Result<Json<Vec<InboxItemSnapshot>>, ServerError> {
    Ok(Json(
        store
            .list_inbox_items()
            .await?
            .into_iter()
            .map(|item| InboxItemSnapshot {
                chat_id: item.chat_id,
                chat_title: item.chat_title,
                turn_id: item.turn_id,
                call_id: item.call_id,
                kind: item.kind,
                action: item
                    .tool_name
                    .as_deref()
                    .map(openwave_core::RendererToolName::from),
                requested_at: item.requested_at,
            })
            .collect(),
    ))
}
