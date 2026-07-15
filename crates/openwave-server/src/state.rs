//! Shared application state handed to every request handler.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use openwave_core::{
    AgentConfig, AgentError, BlobStore, CancelToken, ChatId, Config, DocumentId, FsBlobStore,
    Result, SecretProvider, SteerInbox, Store, ToolRegistry, TurnId,
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
    /// Wakes the durable turn worker after acceptance or cancellation commits.
    pub(crate) turn_job_wake: Arc<Notify>,
    /// Wakes the source-blob retirement worker after a reference drop commits.
    pub(crate) blob_retirement_wake: Arc<Notify>,
    /// Serializes the final publish transition with source replacement/deletion.
    pub(crate) document_writes: Arc<DocumentWriteGuard>,
    /// Coordinates source publication and retirement across server processes.
    pub(crate) blob_writes: Arc<BlobWriteGuard>,
    /// Per-turn agent tuning (model, limits, …).
    pub agent_config: AgentConfig,
    /// The secret every request must present as `Authorization: Bearer <token>`.
    pub token: Arc<str>,
    /// Process-local cancel/steer handles for exact durably claimed attempts.
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
        let blob_writes = Arc::new(BlobWriteGuard::new(config.data_dir.join("blob-locks")));
        Self {
            config: Arc::new(config),
            store,
            blobs,
            resolver,
            secrets,
            tools,
            retrieval,
            document_job_wake: Arc::new(Notify::new()),
            turn_job_wake: Arc::new(Notify::new()),
            blob_retirement_wake: Arc::new(Notify::new()),
            document_writes: Arc::new(DocumentWriteGuard::default()),
            blob_writes,
            agent_config,
            token: Uuid::new_v4().to_string().into(),
            active_turns: Arc::new(TurnGuard::default()),
            events: Arc::new(EventBus::default()),
            approvals: Arc::new(ApprovalBroker::new()),
        }
    }
}

/// Cross-process exclusion for one content-addressed blob lifecycle.
///
/// Lock files are permanent stable rendezvous points. Removing one could split
/// waiters across different inodes, so the grace-period auditor must ignore this
/// dedicated directory.
pub(crate) struct BlobWriteGuard {
    root: Arc<PathBuf>,
}

pub(crate) struct BlobWritePermit {
    _file: File,
}

impl BlobWriteGuard {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
        }
    }

    pub(crate) async fn acquire(&self, blob_id: Uuid) -> Result<BlobWritePermit> {
        let root = Arc::clone(&self.root);
        tokio::task::spawn_blocking(move || {
            fs::create_dir_all(&*root).map_err(blob_lock_error)?;
            let path = root.join(format!("{blob_id}.lock"));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true);
            #[cfg(unix)]
            options.mode(0o600);
            let file = options.open(path).map_err(blob_lock_error)?;
            file.lock().map_err(blob_lock_error)?;
            Ok(BlobWritePermit { _file: file })
        })
        .await
        .map_err(|error| AgentError::Store(format!("blob lock task failed: {error}")))?
    }
}

fn blob_lock_error(error: std::io::Error) -> AgentError {
    AgentError::Store(format!("failed to acquire blob lifecycle lock: {error}"))
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

/// Exact local handles for one durably claimed turn attempt.
#[derive(Clone)]
struct TurnHandles {
    turn_id: TurnId,
    lease_token: Uuid,
    cancel: CancelToken,
    steer: SteerInbox,
}

/// Locates the process-local signals for an exact durably claimed attempt.
///
/// The database owns admission and single-writer fencing. This registry only
/// lets cancel/steer ingress reach the worker currently executing that exact
/// `(chat, turn, lease)` identity in this process.
#[derive(Default)]
pub struct TurnGuard {
    active: Mutex<HashMap<ChatId, TurnHandles>>,
    released: tokio::sync::Notify,
}

impl TurnGuard {
    /// Register one exact local attempt, or refuse a conflicting local worker.
    pub fn register(
        self: &Arc<Self>,
        chat_id: ChatId,
        turn_id: TurnId,
        lease_token: Uuid,
    ) -> Option<ActiveTurn> {
        let mut active = self.active.lock().unwrap();
        if active.contains_key(&chat_id) {
            return None;
        }
        let cancel = CancelToken::new();
        let steer = SteerInbox::new();
        active.insert(
            chat_id,
            TurnHandles {
                turn_id,
                lease_token,
                cancel: cancel.clone(),
                steer: steer.clone(),
            },
        );
        Some(ActiveTurn {
            guard: Arc::clone(self),
            chat_id,
            turn_id,
            lease_token,
            cancel,
            steer,
        })
    }

    /// Wait until no local worker owns this chat.
    pub async fn wait_until_vacant(&self, chat_id: ChatId) {
        loop {
            let released = self.released.notified();
            if !self.active.lock().unwrap().contains_key(&chat_id) {
                return;
            }
            released.await;
        }
    }

    /// Trip cancellation only for the exact turn currently executing locally.
    pub fn cancel(&self, chat_id: ChatId, turn_id: TurnId) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.turn_id == turn_id => {
                handles.cancel.cancel();
                true
            }
            _ => false,
        }
    }

    /// Wake the exact local worker after durable steering admission commits.
    ///
    /// The instruction remains in the store; this process-local signal only
    /// reduces delivery latency and optionally preempts the provider stream.
    pub fn signal_steer(&self, chat_id: ChatId, turn_id: TurnId, interrupt: bool) -> bool {
        match self.active.lock().unwrap().get(&chat_id) {
            Some(handles) if handles.turn_id == turn_id => handles.steer.signal_durable(interrupt),
            _ => false,
        }
    }
}

/// A held turn slot; releases the chat on drop.
pub struct ActiveTurn {
    guard: Arc<TurnGuard>,
    chat_id: ChatId,
    turn_id: TurnId,
    lease_token: Uuid,
    cancel: CancelToken,
    steer: SteerInbox,
}

impl ActiveTurn {
    /// The cancel token for this turn — handed to the agent so a `POST .../cancel`
    /// routed through [`TurnGuard::cancel`] stops it.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// The steer inbox for this turn — handed to the agent so a durable steer
    /// notification injects mid-turn.
    pub fn steer_inbox(&self) -> SteerInbox {
        self.steer.clone()
    }
}

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        let mut active = self.guard.active.lock().unwrap();
        if active.get(&self.chat_id).is_some_and(|handles| {
            handles.turn_id == self.turn_id && handles.lease_token == self.lease_token
        }) {
            active.remove(&self.chat_id);
            self.guard.released.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_local_attempt_per_chat_then_released_on_drop() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("first register succeeds");
        assert!(
            guard
                .register(chat, TurnId::new(), Uuid::new_v4())
                .is_none(),
            "a conflicting local attempt is refused"
        );
        assert!(guard
            .register(ChatId::new(), TurnId::new(), Uuid::new_v4())
            .is_some());

        drop(held);
        assert!(
            guard
                .register(chat, TurnId::new(), Uuid::new_v4())
                .is_some(),
            "dropping the permit frees the local registry"
        );
    }

    #[test]
    fn cancel_trips_the_held_token() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        assert!(!guard.cancel(chat, turn), "nothing to cancel yet");
        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("register");
        assert!(!held.cancel_token().is_cancelled());
        assert!(!guard.cancel(chat, TurnId::new()));
        assert!(guard.cancel(chat, turn));
        assert!(held.cancel_token().is_cancelled());
        assert!(guard.cancel(chat, turn));
    }

    #[test]
    fn stale_permit_cannot_remove_a_newer_attempt() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let old_turn = TurnId::new();
        let old_token = Uuid::new_v4();
        let held = guard.register(chat, old_turn, old_token).expect("register");
        let new_turn = TurnId::new();
        let new_token = Uuid::new_v4();
        guard.active.lock().unwrap().insert(
            chat,
            TurnHandles {
                turn_id: new_turn,
                lease_token: new_token,
                cancel: CancelToken::new(),
                steer: SteerInbox::new(),
            },
        );
        drop(held);
        assert!(guard.cancel(chat, new_turn));
    }

    #[test]
    fn steer_signal_wakes_the_held_inbox() {
        let guard = Arc::new(TurnGuard::default());
        let chat = ChatId::new();
        let turn = TurnId::new();

        assert!(!guard.signal_steer(chat, turn, false));
        let held = guard
            .register(chat, turn, Uuid::new_v4())
            .expect("register");
        assert!(!guard.signal_steer(chat, TurnId::new(), true));
        assert!(guard.signal_steer(chat, turn, true));
        assert!(held.steer_inbox().interrupt_requested());
        assert!(held.steer_inbox().drain().is_empty());
    }
}
