//! In-memory live fan-out for one code-mode session plus the per-owner
//! updates channels.
//!
//! The journal is the durable record a client replays on connect; this bus is
//! the live tail. The session worker appends each event to the journal and
//! then publishes it here with its assigned `seq`.
//!
//! `/code/updates` is unsequenced: a dropped notice costs nothing because the
//! full digest is restated on every connect.
//!
//! Updates are keyed by owner rather than filtered on the way out. There is
//! one broadcast channel per principal, so a subscriber's receiver carries
//! only its own owner's notices and another owner's digest is not something
//! the socket could drop — it never reaches it. Filtering published notices
//! in the route, or in the client, would leave the cross-owner event on the
//! wire for anyone who skipped the filter; decision 47 names that the wrong
//! implementation.

use std::collections::HashMap;
use std::sync::Mutex;

use tidebreak_core::{
    Attention, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeWatchState, OwnerId,
    PullRequestDigest, RepoId, SequencedCodeEvent, WorkspaceId,
};
use tokio::sync::broadcast;

const LIVE_BUFFER: usize = 256;
const UPDATES_BUFFER: usize = 256;

/// Cheap per-session digest published on `/code/updates`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionDigest {
    pub workspace: WorkspaceId,
    pub session: CodeSessionId,
    pub kind: CodeSessionKind,
    pub lifecycle: CodeSessionLifecycle,
    pub attention: Attention,
    pub title: String,
    pub turn_count: i64,
    pub pr_state: Option<PullRequestDigest>,
    /// Watch progress, set only when `kind` is `Watch`. Lifecycle words
    /// undersell a watch ("running" for hours); these say what it is doing.
    pub watch_state: Option<CodeWatchState>,
    pub watch_detail: Option<String>,
    pub watch_cycles: Option<i64>,
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

/// Per-session broadcast channels for live journal events, plus one digest
/// channel per owner.
pub(crate) struct CodeEventBus {
    channels: Mutex<HashMap<CodeSessionId, broadcast::Sender<SequencedCodeEvent>>>,
    updates: Mutex<HashMap<OwnerId, broadcast::Sender<CodeLiveUpdate>>>,
}

impl Default for CodeEventBus {
    fn default() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            updates: Mutex::new(HashMap::new()),
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

    fn updates_sender(&self, owner: &OwnerId) -> broadcast::Sender<CodeLiveUpdate> {
        self.updates
            .lock()
            .expect("code updates bus lock")
            .entry(owner.clone())
            .or_insert_with(|| broadcast::channel(UPDATES_BUFFER).0)
            .clone()
    }

    /// Subscribe to one owner's updates. The receiver is the only view of the
    /// channel a `/code/updates` socket gets, and it carries nothing else.
    pub(crate) fn subscribe_updates(&self, owner: &OwnerId) -> broadcast::Receiver<CodeLiveUpdate> {
        self.updates_sender(owner).subscribe()
    }

    /// Publish a notice to one owner. Publishers name the owner the notice
    /// belongs to; there is no channel that reaches everyone.
    pub(crate) fn publish_update(&self, owner: &OwnerId, update: CodeLiveUpdate) {
        let _ = self.updates_sender(owner).send(update);
    }
}
