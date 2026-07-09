//! Shared application state handed to every request handler.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use openwave_core::{AgentConfig, ChatId, Config, ModelProvider, Store, ToolRegistry};
use uuid::Uuid;

use crate::bus::EventBus;

/// The state cloned into each handler: the boot config, the durable store, the
/// agent's dependencies (provider + tools + tuning), the per-launch bearer token,
/// and the guard that keeps a chat to one running turn at a time.
///
/// Cheap to clone — everything shared is behind an `Arc`.
#[derive(Clone)]
pub struct AppState {
    /// Boot configuration for this launch.
    pub config: Arc<Config>,
    /// Durable metadata, conversation state, and the event journal.
    pub store: Arc<dyn Store>,
    /// The model backend turns stream completions from.
    pub provider: Arc<dyn ModelProvider>,
    /// The tools available to the agent.
    pub tools: Arc<ToolRegistry>,
    /// Per-turn agent tuning (model, limits, …).
    pub agent_config: AgentConfig,
    /// The secret every request must present as `Authorization: Bearer <token>`.
    pub token: Arc<str>,
    /// Tracks which chats have a turn in flight, so a second concurrent turn on
    /// the same chat is refused rather than racing the event journal.
    pub active_turns: Arc<TurnGuard>,
    /// Live fan-out of turn events to connected WebSocket clients.
    pub events: Arc<EventBus>,
}

impl AppState {
    /// Assemble state, minting a fresh random bearer token for this launch.
    pub fn new(
        config: Config,
        store: Arc<dyn Store>,
        provider: Arc<dyn ModelProvider>,
        tools: Arc<ToolRegistry>,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            config: Arc::new(config),
            store,
            provider,
            tools,
            agent_config,
            token: Uuid::new_v4().to_string().into(),
            active_turns: Arc::new(TurnGuard::default()),
            events: Arc::new(EventBus::default()),
        }
    }
}

/// Admits one running turn per chat.
///
/// The agent's per-chat sequence numbering assumes a single writer; this guard
/// upholds that at the API edge — a turn holds its chat's slot until it finishes,
/// and a concurrent request for the same chat is refused (never queued behind it).
#[derive(Default)]
pub struct TurnGuard {
    active: Mutex<HashSet<ChatId>>,
}

impl TurnGuard {
    /// Claim the chat's turn slot, or `None` if a turn is already running for it.
    /// The returned [`ActiveTurn`] releases the slot when dropped — including on
    /// panic — so a failed turn never wedges the chat.
    pub fn try_acquire(self: &Arc<Self>, chat_id: ChatId) -> Option<ActiveTurn> {
        if self.active.lock().unwrap().insert(chat_id) {
            Some(ActiveTurn {
                guard: Arc::clone(self),
                chat_id,
            })
        } else {
            None
        }
    }
}

/// A held turn slot; releases the chat on drop.
pub struct ActiveTurn {
    guard: Arc<TurnGuard>,
    chat_id: ChatId,
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.guard.active.lock().unwrap().remove(&self.chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_turn_per_chat_then_released_on_drop() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();

        let held = guard.try_acquire(chat).expect("first acquire succeeds");
        assert!(
            guard.try_acquire(chat).is_none(),
            "a second turn for the same chat is refused"
        );
        // A different chat is independent.
        assert!(guard.try_acquire(ChatId::new()).is_some());

        drop(held);
        assert!(
            guard.try_acquire(chat).is_some(),
            "the slot frees once the turn is dropped"
        );
    }
}
