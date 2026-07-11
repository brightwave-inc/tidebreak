//! Shared application state handed to every request handler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use openwave_core::{
    AgentConfig, BlobStore, CancelToken, ChatId, Config, DocumentId, FsBlobStore, SecretProvider,
    SteerInbox, Store, ToolRegistry,
};
use openwave_retrieval::Retriever;
use tokio::sync::Notify;
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
    /// Durable raw bytes and generated artifacts under the configured data directory.
    pub blobs: Arc<dyn BlobStore>,
    /// Builds the model provider for each turn from the configured credentials.
    pub resolver: Arc<dyn ProviderResolver>,
    /// Credential custody — where the model API key is read from and written to.
    pub secrets: Arc<dyn SecretProvider>,
    /// The tools available to the agent.
    pub tools: Arc<ToolRegistry>,
    /// The retrieval pipeline used by the durable document worker and the
    /// agent's shared `search` tool.
    pub retrieval: Arc<Retriever>,
    /// Wakes the durable document worker after an enqueue commits.
    pub(crate) document_job_wake: Arc<Notify>,
    /// Serializes the final publish transition with source replacement/deletion.
    pub(crate) document_writes: Arc<DocumentWriteGuard>,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Config,
        store: Arc<dyn Store>,
        resolver: Arc<dyn ProviderResolver>,
        secrets: Arc<dyn SecretProvider>,
        tools: Arc<ToolRegistry>,
        retrieval: Arc<Retriever>,
        agent_config: AgentConfig,
    ) -> Self {
        let blobs: Arc<dyn BlobStore> = Arc::new(FsBlobStore::new(config.data_dir.join("blobs")));
        Self {
            config: Arc::new(config),
            store,
            blobs,
            resolver,
            secrets,
            tools,
            retrieval,
            document_job_wake: Arc::new(Notify::new()),
            document_writes: Arc::new(DocumentWriteGuard::default()),
            agent_config,
            token: Uuid::new_v4().to_string().into(),
            active_turns: Arc::new(TurnGuard::default()),
            events: Arc::new(EventBus::default()),
            approvals: Arc::new(ApprovalBroker::new()),
        }
    }
}

/// A bounded keyed async lock for document lifecycle transitions.
///
/// Lance admits one writer process for a dataset. Within that process these
/// stripes close the gap between the worker's final durable lease proof and
/// vector activation by excluding a concurrent source replacement or deletion.
pub(crate) struct DocumentWriteGuard {
    stripes: Box<[Arc<tokio::sync::Mutex<()>>]>,
}

impl Default for DocumentWriteGuard {
    fn default() -> Self {
        Self {
            stripes: (0..256)
                .map(|_| Arc::new(tokio::sync::Mutex::new(())))
                .collect(),
        }
    }
}

impl DocumentWriteGuard {
    pub(crate) async fn acquire(
        &self,
        document_id: DocumentId,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        document_id.hash(&mut hasher);
        let stripe = hasher.finish() as usize % self.stripes.len();
        self.stripes[stripe].clone().lock_owned().await
    }
}

/// Handles held for one running turn — cancel + steer mailboxes.
///
/// `accepting` is cleared when the agent returns so cancel/steer get `409`
/// during journal drain (the slot stays held for seq safety, but ingress stops).
#[derive(Clone)]
struct TurnHandles {
    cancel: CancelToken,
    steer: SteerInbox,
    accepting: Arc<std::sync::atomic::AtomicBool>,
}

/// Admits one running turn per chat, and holds each running turn's
/// [`CancelToken`] / [`SteerInbox`] so cancel and steer requests can find them.
///
/// The agent's per-chat sequence numbering assumes a single writer; this guard
/// upholds that at the API edge — a turn holds its chat's slot until it finishes,
/// and a concurrent request for the same chat is refused (never queued behind it).
#[derive(Default)]
pub struct TurnGuard {
    active: Mutex<HashMap<ChatId, TurnHandles>>,
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
        let steer = SteerInbox::new();
        let accepting = Arc::new(std::sync::atomic::AtomicBool::new(true));
        active.insert(
            chat_id,
            TurnHandles {
                cancel: cancel.clone(),
                steer: steer.clone(),
                accepting: accepting.clone(),
            },
        );
        Some(ActiveTurn {
            guard: Arc::clone(self),
            chat_id,
            cancel,
            steer,
            accepting,
        })
    }

    /// Trip the cancel token of the turn running for `chat_id`, if any. Returns
    /// whether a turn was found and still accepting cancel. Idempotent while the
    /// agent is running; returns false once ingress is closed (journal drain).
    pub fn cancel(&self, chat_id: ChatId) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.accepting.load(std::sync::atomic::Ordering::Acquire) => {
                handles.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Push a steer message into the turn running for `chat_id`, if any. Returns
    /// whether a turn was found and still accepting steer. When `interrupt` is
    /// true the agent preempts the provider stream; otherwise the message waits
    /// for the next step boundary.
    pub fn steer(&self, chat_id: ChatId, content: String, interrupt: bool) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.accepting.load(std::sync::atomic::Ordering::Acquire) => {
                handles.steer.push(content, interrupt)
            }
            _ => false,
        }
    }
}

/// A held turn slot; releases the chat on drop.
pub struct ActiveTurn {
    guard: Arc<TurnGuard>,
    chat_id: ChatId,
    cancel: CancelToken,
    steer: SteerInbox,
    accepting: Arc<std::sync::atomic::AtomicBool>,
}

impl ActiveTurn {
    /// The cancel token for this turn — handed to the agent so a `POST .../cancel`
    /// routed through [`TurnGuard::cancel`] stops it.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The steer inbox for this turn — handed to the agent so a `POST .../steer`
    /// routed through [`TurnGuard::steer`] injects mid-turn.
    pub fn steer_inbox(&self) -> SteerInbox {
        self.steer.clone()
    }

    /// Stop accepting cancel/steer (agent finished). The slot stays held until
    /// drop so the journal can finish without a concurrent turn racing seq.
    pub fn close_ingress(&self) {
        self.accepting
            .store(false, std::sync::atomic::Ordering::Release);
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
            "dropping the slot frees the chat for another turn"
        );
    }

    #[test]
    fn cancel_trips_the_held_token() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();

        assert!(!guard.cancel(chat), "nothing to cancel yet");
        let held = guard.try_acquire(chat).expect("acquire");
        assert!(!held.cancel_token().is_cancelled());
        assert!(guard.cancel(chat));
        assert!(held.cancel_token().is_cancelled());
        // Idempotent while the turn is still accepting.
        assert!(guard.cancel(chat));
    }

    #[test]
    fn close_ingress_refuses_further_cancel_and_steer() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let held = guard.try_acquire(chat).expect("acquire");
        held.close_ingress();
        assert!(!guard.cancel(chat), "cancel refused after ingress closed");
        assert!(
            !guard.steer(chat, "late".into(), false),
            "steer refused after ingress closed"
        );
        // Slot still held for seq safety.
        assert!(guard.try_acquire(chat).is_none());
    }

    #[test]
    fn steer_pushes_into_the_held_inbox() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();

        assert!(!guard.steer(chat, "x".into(), false));
        let held = guard.try_acquire(chat).expect("acquire");
        assert!(guard.steer(chat, "course correct".into(), true));
        let msgs = held.steer_inbox().drain();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "course correct");
    }
}
