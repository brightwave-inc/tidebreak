//! Shared application state handed to every request handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openwave_core::{
    AgentConfig, CancelToken, ChatId, Config, SecretProvider, Store, ToolRegistry,
};
use uuid::Uuid;

use crate::approvals::ApprovalBroker;
use crate::bus::EventBus;
use crate::resolver::ProviderResolver;

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
    /// Builds the model provider for each turn from the configured credentials.
    pub resolver: Arc<dyn ProviderResolver>,
    /// Credential custody — where the model API key is read from and written to.
    pub secrets: Arc<dyn SecretProvider>,
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
    /// Parks Sensitive tool calls until `POST .../approvals/{call_id}` decides.
    pub approvals: Arc<ApprovalBroker>,
}

impl AppState {
    /// Assemble state, minting a fresh random bearer token for this launch.
    pub fn new(
        config: Config,
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        tools: Arc<ToolRegistry>,
        agent_config: AgentConfig,
    ) -> Self {
        Self {
            config: Arc::new(config),
            store,
            resolver,
            secrets,
            tools,
            agent_config,
            token: Uuid::new_v4().to_string().into(),
            active_turns: Arc::new(TurnGuard::default()),
            events: Arc::new(EventBus::default()),
            approvals: Arc::new(ApprovalBroker::new()),
        }
    }
}

/// Admits one running turn per chat, and holds each running turn's
/// [`CancelToken`] so a cancel request can find and trip it.
///
/// The agent's per-chat sequence numbering assumes a single writer; this guard
/// upholds that at the API edge — a turn holds its chat's slot until it finishes,
/// and a concurrent request for the same chat is refused (never queued behind it).
#[derive(Default)]
pub struct TurnGuard {
    active: Mutex<HashMap<ChatId, CancelToken>>,
}

impl TurnGuard {
    /// Claim the chat's turn slot, or `None` if a turn is already running for it.
    /// The returned [`ActiveTurn`] releases the slot when dropped — including on
    /// panic — so a failed turn never wedges the chat.
    pub fn try_acquire(self: &Arc<Self>, chat_id: ChatId) -> Option<ActiveTurn> {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(&chat_id) {
            return None;
        }
        let cancel = CancelToken::new();
        active.insert(chat_id, cancel.clone());
        Some(ActiveTurn {
            guard: Arc::clone(self),
            chat_id,
            cancel,
        })
    }

    /// Trip the cancel token of the turn running for `chat_id`, if any. Returns
    /// whether a turn was found to cancel. Idempotent — a second cancel while the
    /// turn winds down just re-trips the already-tripped token.
    pub fn cancel(&self, chat_id: ChatId) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
}

/// A held turn slot; releases the chat on drop.
pub struct ActiveTurn {
    guard: Arc<TurnGuard>,
    chat_id: ChatId,
    cancel: CancelToken,
}

impl ActiveTurn {
    /// The cancel token for this turn — handed to the agent so a `POST .../cancel`
    /// routed through [`TurnGuard::cancel`] stops it.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
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

    #[test]
    fn cancel_trips_the_running_turns_token() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();

        // No turn running yet: nothing to cancel.
        assert!(!guard.cancel(chat));

        let held = guard.try_acquire(chat).expect("acquire");
        let token = held.cancel_token();
        assert!(!token.is_cancelled());

        assert!(guard.cancel(chat), "a running turn is found and cancelled");
        assert!(token.is_cancelled(), "the turn's own token is tripped");

        // Once the slot frees, there's again nothing to cancel.
        drop(held);
        assert!(!guard.cancel(chat));
    }
}
