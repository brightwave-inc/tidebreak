//! Compaction the user asks for, outside any turn.
//!
//! Automatic compaction waits for the transcript to cross a threshold, which is
//! what keeps it infrequent. Someone watching the context meter may want it
//! sooner — before a long piece of work, or after a detour they do not want the
//! model carrying. This runs the same pass on demand, keeps every other rule
//! (the boundary selection, the protected tail, the fail-open outcome), and
//! reports back what it did.

use axum::extract::State;
use futures::channel::mpsc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use openwave_core::{Agent, ChatId, SequencedEvent, ToolRegistry};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

use super::providers_models::resolve_chat_model;

/// How much focus text one request may carry.
///
/// It is a hint for a summary, not a document: the summarizer has a small
/// budget, and text long enough to need its own summary would crowd out the
/// transcript this checkpoint is supposed to be about.
pub const MAX_COMPACTION_FOCUS_CHARS: usize = 500;

/// Body cap for `POST /chats/{id}/compact`.
pub const MAX_COMPACT_CHAT_BODY_BYTES: usize = 4 * 1024;

/// Body of `POST /chats/{id}/compact`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactChat {
    /// What the summary should hold on to, in the caller's words. Absent is the
    /// ordinary request, where the summarizer keeps what it judges load-bearing.
    #[serde(default)]
    pub focus: Option<String>,
}

/// What one on-demand compaction did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ts_rs::TS)]
pub struct CompactionRun {
    /// Whether a checkpoint was written. `false` is a complete, ordinary answer:
    /// the chat had too little history to give up, its recent messages are all
    /// protected, or the summarizer declined. Nothing is wrong, and nothing
    /// changed — the caller says so rather than leaving the reader with silence.
    pub compacted: bool,
}

/// `POST /chats/{id}/compact` — compact this chat now.
///
/// `404` for an unknown chat, `409` while a turn is running (compaction rewrites
/// what the next model call sees, so it cannot run under one), `400` for focus
/// text that is unusable, and `200` with `compacted` either way otherwise.
pub async fn post_compact(
    State(state): State<AppState>,
    store: ScopedStore,
    Path(id): Path<ChatId>,
    Json(body): Json<CompactChat>,
) -> Result<Json<CompactionRun>, ServerError> {
    let chat = store.require_chat(id).await?;
    let focus = validate_focus(body.focus.as_deref())?;

    // The durable turn rows are the authority on whether this chat is busy —
    // the in-process registry only knows about turns this server is running.
    if state
        .store
        .list_turn_runs(id)
        .await?
        .iter()
        .any(|turn| !turn.status.is_terminal())
    {
        return Err(ServerError::conflict_kind(
            "compaction_chat_busy",
            "this chat has a turn running; compaction rewrites what the next model call sees",
        ));
    }

    let mut config = state.agent_config.clone();
    let model = resolve_chat_model(&*state.store, &chat, &state.agent_config.model).await?;
    match crate::providers::resolve_model_policy(&*state.store, &model, true).await? {
        Some(policy) => {
            crate::providers::apply_model_policy(&mut config, &policy, chat.reasoning_effort)?;
        }
        None => {
            crate::providers::apply_free_form_model(&mut config, model, chat.reasoning_effort)?;
        }
    }
    // The maintenance call runs on the host's utility model, exactly as the
    // automatic pass does. Without one there is nothing to compact with, and
    // saying so beats reporting an empty success.
    config.utility_model = crate::model_roles::resolve_utility_model(
        &*state.store,
        &*state.secrets,
        &*state.provisioned_policy,
        &*state.os_policy,
    )
    .await?;
    if config.utility_model.is_none() {
        return Err(ServerError::unprocessable_kind(
            "compaction_utility_model_unavailable",
            "no utility model is configured, so there is nothing to summarize this chat with",
        ));
    }
    config.compaction = super::read_compaction_policy(&*state.store).await?;

    let agent = Agent::new(
        state.resolver.resolve().await,
        // Compaction sends no tools; an empty registry keeps that explicit.
        std::sync::Arc::new(ToolRegistry::new()),
        state.store.clone(),
        config,
    );
    let (sender, mut receiver) = mpsc::unbounded();
    let compacted = agent.compact_now(id, focus.as_deref(), &sender).await?;
    drop(sender);

    // Journaled after the fact rather than streamed: the pass is short, and the
    // renderer reads these two events for the same status it shows during a
    // turn. Order is preserved, and each publish carries the seq the append
    // allocated, which is what the live stream reconciles against.
    while let Some(event) = receiver.next().await {
        let seq = state.store.append_chat_event(id, &event).await?;
        let _ = state.events.sender(id).send(SequencedEvent { seq, event });
    }

    Ok(Json(CompactionRun {
        compacted: compacted.is_some(),
    }))
}

/// Focus text the summarizer can be given, or nothing.
///
/// Blank is the same as absent — a caller that sends an empty field is asking
/// for an ordinary compaction, not for a summary about nothing.
fn validate_focus(focus: Option<&str>) -> Result<Option<String>, ServerError> {
    let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) else {
        return Ok(None);
    };
    if focus.contains('\0') {
        return Err(ServerError::bad_request(
            "compaction focus must not contain NUL characters",
        ));
    }
    if focus.chars().count() > MAX_COMPACTION_FOCUS_CHARS {
        return Err(ServerError::bad_request(format!(
            "compaction focus must be at most {MAX_COMPACTION_FOCUS_CHARS} characters",
        )));
    }
    Ok(Some(focus.to_owned()))
}
