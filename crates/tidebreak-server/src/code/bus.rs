//! In-memory live fan-out for one code-mode session plus the install-wide
//! updates channel.
//!
//! The journal is the durable record a client replays on connect; this bus is
//! the live tail. The session worker appends each event to the journal and
//! then publishes it here with its assigned `seq`.
//!
//! `/code/updates` is unsequenced: a dropped notice costs nothing because the
//! full digest is restated on every connect.

use std::collections::HashMap;
use std::sync::Mutex;

use tidebreak_core::{
    Attention, CodeSessionId, CodeSessionLifecycle, PullRequestDigest, RepoId, SequencedCodeEvent,
    WorkspaceId,
};
use tokio::sync::broadcast;

const LIVE_BUFFER: usize = 256;
const UPDATES_BUFFER: usize = 256;

/// Cheap per-session digest published on `/code/updates`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionDigest {
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
    pub lifecycle: CodeSessionLifecycle,
    pub attention: Attention,
    pub title: String,
    pub turn_count: i64,
    pub pr_state: Option<PullRequestDigest>,
}

/// Progress of one in-flight `git clone` job.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CloneProgress {
    pub job: String,
    pub phase: String,
    pub percent: Option<u8>,
    pub done: bool,
    pub error: Option<String>,
    pub repo_id: Option<RepoId>,
}

/// One unsequenced notice on the install-wide updates channel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CodeLiveUpdate {
    /// Cheap per-session digest, computed from rows. Boxed: the PR digest
    /// makes it much larger than the clone-progress variant.
    Digest(Box<SessionDigest>),
    /// Clone job progress. Not restated on connect.
    CloneProgress(CloneProgress),
}

/// Per-session broadcast channels for live journal events, plus the
/// install-wide digest channel.
pub(crate) struct CodeEventBus {
    channels: Mutex<HashMap<CodeSessionId, broadcast::Sender<SequencedCodeEvent>>>,
    updates: broadcast::Sender<CodeLiveUpdate>,
}

impl Default for CodeEventBus {
    fn default() -> Self {
        let (updates, _) = broadcast::channel(UPDATES_BUFFER);
        Self {
            channels: Mutex::new(HashMap::new()),
            updates,
        }
    }
}

impl CodeEventBus {
    pub(crate) fn sender(&self, session: CodeSessionId) -> broadcast::Sender<SequencedCodeEvent> {
        self.channels
            .lock()
            .expect("code event bus lock")
            .entry(session)
            .or_insert_with(|| broadcast::channel(LIVE_BUFFER).0)
            .clone()
    }

    pub(crate) fn subscribe(
        &self,
        session: CodeSessionId,
    ) -> broadcast::Receiver<SequencedCodeEvent> {
        self.sender(session).subscribe()
    }

    pub(crate) fn publish(&self, session: CodeSessionId, event: SequencedCodeEvent) {
        let _ = self.sender(session).send(event);
    }

    pub(crate) fn subscribe_updates(&self) -> broadcast::Receiver<CodeLiveUpdate> {
        self.updates.subscribe()
    }

    pub(crate) fn publish_update(&self, update: CodeLiveUpdate) {
        let _ = self.updates.send(update);
    }
}
