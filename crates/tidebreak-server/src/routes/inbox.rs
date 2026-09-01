//! The single list of what is waiting on the reader, across every
//! conversation.
//!
//! A read model, not a store: every entry here is projected from state the
//! per-conversation routes already serve, and each item is resolved through
//! that same conversation's existing route — the approval gate, the question
//! answer, the plan decision, the native folder grant. Resolving is therefore
//! first-responder by construction: whoever commits first wins, and a second
//! attempt meets the same conflict the in-conversation card would have met.
//!
//! Decision 48 step 3 made this one queue over both surfaces. An entry is a
//! conversation carrying an [`Attention`] in the shared vocabulary, with the
//! parked items behind it for deep links. Chat and code still have separate
//! id spaces, so the conversation reference is tagged; that tag is the seam
//! step 5 removes when the entities merge, and it is deliberately the only
//! place either surface's shape shows through.
//!
//! Entries stay as opaque as the per-conversation attention summary.
//! Identity, kind, the title, and when it parked are enough to triage;
//! question prose, plan text, and canonical tool arguments stay behind the
//! conversation the entry deep-links to.

use serde::Serialize;
use tidebreak_core::{
    Attention, AttentionState, CallId, ChatId, CodeSessionId, InboxItemKind, TurnId, WorkspaceId,
};

use crate::error::ServerError;
use crate::extract::Json;
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

/// Which conversation an entry belongs to.
///
/// Tagged because chat ids and code session ids are still separate spaces
/// (the repository check `chat_and_code_entities_do_not_cross_reference`
/// enforces it). When step 5 merges the entities this collapses to one id.
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum InboxConversation {
    /// A conversation with the internal engine.
    Chat { chat_id: ChatId },
    /// A conversation with an external agent engine.
    ///
    /// Carries the workspace too: a code conversation is reached through its
    /// workspace, so a session id alone is not a link the reader can follow.
    Code {
        session_id: CodeSessionId,
        /// `None` for a session with no workspace: the in-process engine's.
        workspace_id: Option<WorkspaceId>,
    },
}

/// One item waiting on the reader, and where to go to answer it.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct InboxItemSnapshot {
    /// With the entry's conversation, the deep link back to the exact
    /// transcript position the item paused at.
    pub turn_id: TurnId,
    pub call_id: CallId,
    pub kind: InboxItemKind,
    /// The tool under review, for an approval. Absent for the other kinds,
    /// whose tool is implied by the kind. Closed renderer vocabulary: an
    /// unrecognized name folds to `other` rather than reaching a card.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub action: Option<tidebreak_core::RendererToolName>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// One conversation that wants the reader, and why.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct InboxEntrySnapshot {
    pub conversation: InboxConversation,
    /// Absent, not null, while the conversation is still untitled.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    pub attention: Attention,
    /// The parked calls behind this attention, oldest first.
    ///
    /// Empty for a code conversation: its approvals are answered through the
    /// code approval route, which carries the verbatim payload decision 33
    /// requires and which a deep link reaches.
    pub items: Vec<InboxItemSnapshot>,
    /// When the oldest thing here started waiting, for ordering.
    pub waiting_since: chrono::DateTime<chrono::Utc>,
}

/// `GET /inbox` — every conversation waiting on the reader, longest first.
pub async fn list_inbox(
    axum::extract::State(state): axum::extract::State<AppState>,
    store: ScopedStore,
) -> Result<Json<Vec<InboxEntrySnapshot>>, ServerError> {
    let items = store.list_inbox_items().await?;
    let attention = store.chat_attention(&items).await?;

    let mut entries: Vec<InboxEntrySnapshot> = Vec::new();
    let mut by_chat: std::collections::HashMap<ChatId, usize> = std::collections::HashMap::new();

    for item in items {
        let index = match by_chat.get(&item.chat_id) {
            Some(index) => *index,
            None => {
                let attention = attention
                    .get(&item.chat_id)
                    .cloned()
                    // A parked item is why this conversation is listed, so it
                    // has an attention by construction; fall back rather than
                    // drop the entry if the two reads ever disagree.
                    .unwrap_or_else(|| {
                        Attention::needs_you(
                            "something is waiting",
                            tidebreak_core::AttentionSource::Structured,
                        )
                    });
                entries.push(InboxEntrySnapshot {
                    conversation: InboxConversation::Chat {
                        chat_id: item.chat_id,
                    },
                    title: item.chat_title.clone(),
                    attention,
                    items: Vec::new(),
                    waiting_since: item.requested_at,
                });
                by_chat.insert(item.chat_id, entries.len() - 1);
                entries.len() - 1
            }
        };
        entries[index].items.push(InboxItemSnapshot {
            turn_id: item.turn_id,
            call_id: item.call_id,
            kind: item.kind,
            action: item
                .tool_name
                .as_deref()
                .map(tidebreak_core::RendererToolName::from),
            requested_at: item.requested_at,
        });
    }

    // Code sessions carry their attention on the row, so the same question —
    // is this waiting on the reader? — is asked of the state rather than of a
    // projection. Only states that want a person are listed; `Working` and
    // `Idle` are not the reader's problem.
    // Code mode is optional on a deployment, and a server without it still has
    // an inbox. Absent means the queue is chats only, not that the read fails.
    let code = state
        .code
        .clone()
        .map(|runtime| crate::code::ScopedCode::for_owner(runtime, store.owner().clone()));
    if let Some(code) = code {
        let sessions = code.list_sessions().await?;
        if sessions
            .iter()
            .any(|session| wants_the_reader(&session.attention.state))
        {
            // One read for the titles: a session has none of its own, and an
            // entry the reader cannot recognize is not worth listing.
            let titles = code
                .list_workspaces(None)
                .await?
                .into_iter()
                .map(|workspace| (workspace.id, workspace.title))
                .collect::<std::collections::HashMap<_, _>>();
            for session in sessions {
                if !wants_the_reader(&session.attention.state) {
                    continue;
                }
                entries.push(InboxEntrySnapshot {
                    title: session
                        .workspace_id
                        .and_then(|workspace_id| titles.get(&workspace_id).cloned()),
                    conversation: InboxConversation::Code {
                        session_id: session.id,
                        workspace_id: session.workspace_id,
                    },
                    attention: session.attention,
                    items: Vec::new(),
                    waiting_since: session.created_at,
                });
            }
        }
    }

    // Longest-waiting first: the thing blocked longest is what the reader most
    // needs to see, and it is what survives any cap a client applies.
    entries.sort_by(|left, right| {
        left.waiting_since.cmp(&right.waiting_since).then_with(|| {
            format!("{:?}", left.conversation).cmp(&format!("{:?}", right.conversation))
        })
    });
    Ok(Json(entries))
}

/// Whether an attention state is asking for a person.
///
/// `Manual` counts: the reader pinned it precisely so it would keep showing
/// up. `Stalled` counts because a silent engine is a decision waiting to be
/// made. `DoneUnreviewed` counts because unreviewed work is the reader's.
fn wants_the_reader(state: &AttentionState) -> bool {
    match state {
        AttentionState::NeedsYou { .. }
        | AttentionState::Stalled { .. }
        | AttentionState::Fenced { .. }
        | AttentionState::DoneUnreviewed
        | AttentionState::Manual { .. } => true,
        AttentionState::Working | AttentionState::Idle => false,
    }
}
