//! In-memory live fan-out for one code-mode session plus the per-owner
//! updates channels.
//!
//! The journal is the durable record a client replays on connect; this bus is
//! the live tail. The session worker appends each event to the journal and
//! then publishes it here with its assigned `seq`.
//!
//! Assistant deltas are the exception, and the bus owns the whole of their
//! life: they are published here and nowhere else, because the
//! `assistant_message` that follows repeats them byte for byte and the
//! journal would only store the same words twice (record 57). The bus keeps
//! the text streamed so far so a reader who connects mid-answer is not shown
//! a sentence that starts in the middle.
//!
//! The `/updates` stream is unsequenced: a dropped notice costs nothing because the
//! full digest is restated on every connect.
//!
//! Updates are keyed by owner rather than filtered on the way out. There is
//! one broadcast channel per principal, so a subscriber's receiver carries
//! only notices addressed to it and another principal's digest is not
//! something the socket could drop — it never reaches it. Filtering published
//! notices in the route, or in the client, would leave the cross-principal
//! event on the wire for anyone who skipped the filter; decision 47 names that
//! the wrong implementation.
//!
//! Who a notice is addressed to is settled before it is published. Decision
//! 0086 widened that from "the session's owner" to "every principal that
//! reads the session", which [`super::attention::emit_digest`] resolves from
//! the access rows. The partition itself is unchanged: a principal still
//! receives only what was addressed to it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use chrono::{DateTime, Utc};
use tidebreak_core::code::SequencedEvent;
use tidebreak_core::{
    Attention, CodeSubagentSummary, CodeWatchState, Event, HarnessKind, OwnerId, PullRequestDigest,
    RepoId, SessionActivity, SessionId, SessionKind, SessionLifecycle, SessionTreeChild,
    SessionTreeWait, TurnId, WorkspaceId, MAX_EVENT_TEXT_CHARS,
};
use tokio::sync::{broadcast, Notify};
use tokio::time::Instant;

const LIVE_BUFFER: usize = 256;
const UPDATES_BUFFER: usize = 256;

/// Cheap per-session digest published on `/updates`.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDigest {
    /// `None` for a session that binds no workspace.
    pub workspace: Option<WorkspaceId>,
    /// Whether this viewer can open the conversation through the authorized session route.
    pub can_open_chat: bool,
    pub session: SessionId,
    pub kind: SessionKind,
    /// Engine identity for list surfaces that collapse several sessions into
    /// one workspace row.
    pub harness_kind: HarnessKind,
    pub lifecycle: SessionLifecycle,
    pub attention: Attention,
    /// Recovery cause, independent of a manual attention pin.
    pub fence_reason: Option<tidebreak_core::FenceReason>,
    pub title: String,
    pub external_origin: Option<super::types::SessionExternalOrigin>,
    pub turn_count: i64,
    /// Timestamp trigger delivery uses to rank candidate sessions: the newest
    /// turn start, or session creation before the first turn.
    pub trigger_target_at: DateTime<Utc>,
    /// What the live turn is occupied with. Set only while lifecycle is
    /// running; older clients safely fall back to a generic running label.
    pub activity: Option<SessionActivity>,
    /// The subject of the tool the live turn is waiting on: the command,
    /// path, query, or task description. Set only while `activity` names a
    /// tool; bounded so a digest never carries a whole script.
    pub activity_detail: Option<String>,
    pub pr_state: Option<PullRequestDigest>,
    /// How many pull requests hold a durable attribution to this workspace
    /// (decision 77). Absent when none do; a change is the client's cheap
    /// signal to re-read the workspace's pull-request list.
    pub pr_count: Option<u64>,
    /// Watch progress, set only when `kind` is `Watch`. Lifecycle words
    /// undersell a watch ("running" for hours); these say what it is doing.
    pub watch_state: Option<CodeWatchState>,
    pub watch_detail: Option<String>,
    pub watch_cycles: Option<i64>,
    /// Harness subagents on this session, set only when any were observed
    /// (decision 52). Bounded on the session row.
    pub subagents: Option<Vec<CodeSubagentSummary>>,
    /// Where this session stands, in a sentence, from the newest turn that
    /// carries one (`super::recap`). Absent until a turn has been recapped,
    /// and on machines with no utility model to derive one.
    pub recap: Option<String>,
    /// Pending memory proposals that originated in this session.
    pub memory_proposal_count: Option<u64>,
    /// Direct parent session, when this conversation was started as a child.
    pub parent_session: Option<SessionId>,
    /// Authoritative parent wait, never inferred from running children.
    pub wait: Option<SessionTreeWait>,
}

/// Progress of one in-flight `git clone` job.
#[derive(Debug, Clone, PartialEq)]
pub struct CloneProgress {
    pub job: String,
    pub phase: String,
    pub percent: Option<u8>,
    pub done: bool,
    pub error: Option<String>,
    pub repo_id: Option<RepoId>,
}

/// Progress of one warm harness install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessInstallProgress {
    pub kind: HarnessKind,
    pub version: Option<String>,
    pub phase: String,
    pub done: bool,
    pub error: Option<String>,
}

/// One unsequenced notice on the install-wide updates channel.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeLiveUpdate {
    /// Cheap per-session digest, computed from rows. Boxed: the PR digest
    /// makes it much larger than the clone-progress variant.
    Digest(Box<SessionDigest>),
    /// Clone job progress. Not restated on connect.
    CloneProgress(CloneProgress),
    /// Warm harness install progress. Not restated on connect; the doctor
    /// report is the durable answer for what is installed.
    HarnessInstall(HarnessInstallProgress),
    /// The pull-request store changed (decision 66). No payload: delivery
    /// surfaces re-read their queries on receipt.
    Delivery,
    /// Lucid rewrite of a completed turn's closing message. Not restated on
    /// connect: the turn snapshot carries the stored rewrite.
    TurnRewrite(TurnRewriteNotice),
    /// Who may read this session changed (decision 0086). No payload beyond
    /// the id: a reader re-reads what it may see, which is what drops a
    /// session it no longer holds and adds one it just gained.
    AccessChanged(SessionId),
}

/// Progress of one background rewrite of a completed turn's closing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRewriteNotice {
    pub session: SessionId,
    pub turn_id: TurnId,
    pub state: TurnRewriteState,
    pub rewrite: Option<String>,
}

/// Where one rewrite stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRewriteState {
    Rewriting,
    Rewritten,
    Failed,
}

type SessionTreeProjection = (Vec<SessionTreeChild>, Option<SessionTreeWait>);

/// How often one session's digest may be rebuilt and published.
///
/// Tool calls, attention changes, and lifecycle moves all ask for a digest,
/// and a busy session asks many times a second. The first request after a
/// quiet spell publishes at once; later ones inside the interval fold into
/// one digest published when it ends, built from the session as it stands
/// then. So no change is lost, and a digest is built at most twice a second.
pub const DIGEST_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Per-session broadcast channels for live journal events, plus one digest
/// channel per owner.
///
/// A cheap handle: clones share one bus, so background work (a deferred
/// digest, say) can hold its own.
#[derive(Clone, Default)]
pub struct CodeEventBus {
    shared: Arc<BusState>,
}

struct BusState {
    channels: Mutex<HashMap<SessionId, LiveSession>>,
    updates: Mutex<HashMap<OwnerId, broadcast::Sender<CodeLiveUpdate>>>,
    /// Last `session_tree` published on each parent, so a child persist that
    /// did not change the projection does not journal another copy.
    session_trees: Mutex<HashMap<SessionId, SessionTreeProjection>>,
    /// One gate per parent covering compute → compare → journal → remember.
    session_tree_gates: Mutex<HashMap<SessionId, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    /// Fires when a client subscribes to `/updates`, so background work
    /// that slows down while nobody is looking can speed back up at once.
    updates_attached: Notify,
    /// When each session last published a digest, and whether a deferred one
    /// is already scheduled. See [`DIGEST_INTERVAL`].
    digest_pace: Mutex<HashMap<SessionId, DigestPace>>,
    /// One wake per session whose turn is parked, held by the waiting worker.
    park_wakes: Mutex<HashMap<SessionId, Weak<Notify>>>,
}

struct DigestPace {
    last: Instant,
    deferred: bool,
}

/// What a caller asking for a digest should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigestSlot {
    /// Build and publish it now.
    Now,
    /// A deferred digest is already scheduled and will carry this change.
    Scheduled,
    /// Schedule one deferred digest for this moment.
    At(Instant),
}

/// One session's live state: the channel, plus the small facts that only the
/// live stream knows.
///
/// The assistant tail is the reason this is a struct rather than a bare
/// sender. Deltas are published and never journaled, so between the first
/// delta and the `assistant_message` that states the whole answer, this is
/// the only copy of the text a reader who connects mid-turn could be given.
struct LiveSession {
    sender: broadcast::Sender<CodeLiveEvent>,
    /// Assistant text streamed since the last event that finished a message,
    /// bounded the same way the event that will carry it is bounded.
    assistant: String,
    /// Sequence of the newest journaled event published here. Pairs with
    /// `assistant`: the tail is only current for a reader that has not
    /// already replayed past this point.
    cursor: i64,
    /// When this session last published anything, live or journaled.
    last_activity: DateTime<Utc>,
    /// Pessimistic hint for the stall check; see [`CodeEventBus::maybe_stalled`].
    maybe_stalled: bool,
}

impl Default for LiveSession {
    fn default() -> Self {
        Self {
            sender: broadcast::channel(LIVE_BUFFER).0,
            assistant: String::new(),
            cursor: 0,
            last_activity: Utc::now(),
            maybe_stalled: true,
        }
    }
}

/// One event on a session's live channel.
///
/// `seq` is the journal position, and `None` marks an event that no row
/// holds: it is delivered live and never replayed, so it carries no cursor a
/// client could resume from.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeLiveEvent {
    pub seq: Option<i64>,
    /// Journal position this event streamed behind — its own, for a journaled
    /// event.
    ///
    /// A live-only event is only current while the journal has not moved past
    /// it. Once it has, the event that moved it (the message, the tool call,
    /// the turn's end) restates or discards this text, and a reader whose
    /// replay already read that far must not be handed the fragment as well.
    pub cursor: i64,
    pub event: Event,
}

/// What a freshly attached reader needs to catch up on the live tail.
pub struct LiveTail {
    /// Assistant text streamed but not yet journaled. Empty most of the time.
    pub assistant: String,
    /// Journal position the tail is current as of. A reader whose replay went
    /// past this cannot trust the text.
    pub cursor: i64,
}

impl Default for BusState {
    fn default() -> Self {
        Self {
            channels: Mutex::new(HashMap::new()),
            updates: Mutex::new(HashMap::new()),
            session_trees: Mutex::new(HashMap::new()),
            session_tree_gates: Mutex::new(HashMap::new()),
            updates_attached: Notify::new(),
            digest_pace: Mutex::new(HashMap::new()),
            park_wakes: Mutex::new(HashMap::new()),
        }
    }
}

impl CodeEventBus {
    fn with_session<T>(&self, session: SessionId, act: impl FnOnce(&mut LiveSession) -> T) -> T {
        let mut channels = self.shared.channels.lock().expect("code event bus lock");
        act(channels.entry(session).or_default())
    }

    /// Subscribe, and take the live tail in the same breath.
    ///
    /// The two have to be atomic. A caller that subscribed first and read the
    /// tail afterwards would count any delta published in between twice: once
    /// in the tail it read, once from its own receiver. Publication takes the
    /// same lock, so nothing lands between these two lines.
    pub fn attach(&self, session: SessionId) -> (broadcast::Receiver<CodeLiveEvent>, LiveTail) {
        self.with_session(session, |live| {
            (
                live.sender.subscribe(),
                LiveTail {
                    assistant: live.assistant.clone(),
                    cursor: live.cursor,
                },
            )
        })
    }

    /// Publish a journaled event and advance this session's live cursor.
    ///
    /// Journaled events are what the tail is measured against: an event that
    /// finishes the assistant's turn of speech retires the buffered text
    /// rather than leaving a stale copy for the next reconnect to replay.
    pub fn publish(&self, session: SessionId, event: SequencedEvent) {
        self.with_session(session, |live| {
            live.cursor = live.cursor.max(event.seq);
            live.last_activity = Utc::now();
            if ends_assistant_text(&event.event) {
                live.assistant.clear();
            }
            let _ = live.sender.send(CodeLiveEvent {
                seq: Some(event.seq),
                cursor: event.seq,
                event: event.event,
            });
        });
    }

    /// Publish a live-only event: no journal row, no cursor movement.
    ///
    /// Assistant deltas are the whole of this path today. They are how a
    /// reader watches an answer arrive, and the `assistant_message` that
    /// follows carries the same bytes, so the durable copy would say nothing
    /// new (record 57).
    pub fn publish_transient(&self, session: SessionId, event: Event) {
        self.with_session(session, |live| {
            live.last_activity = Utc::now();
            if let Event::AssistantDelta { text } = &event {
                append_bounded(&mut live.assistant, text);
            }
            let _ = live.sender.send(CodeLiveEvent {
                seq: None,
                cursor: live.cursor,
                event,
            });
        });
    }

    /// Assistant text streamed since the last event that finished a message.
    ///
    /// Empty once the message lands. Taking it clears it, which is what the
    /// journal path wants: it is about to write the text down.
    pub fn take_assistant_tail(&self, session: SessionId) -> String {
        self.with_session(session, |live| std::mem::take(&mut live.assistant))
    }

    /// Most recent moment this session published anything, live or journaled.
    ///
    /// The stall sweep reads it. Without it a session streaming a long answer
    /// and nothing else would look silent, because the deltas carrying that
    /// answer no longer touch a row's `created_at`.
    pub fn last_activity(&self, session: SessionId) -> Option<DateTime<Utc>> {
        self.shared
            .channels
            .lock()
            .expect("code event bus lock")
            .get(&session)
            .map(|live| live.last_activity)
    }

    /// Drop the live channel and transient tail for a session that ended.
    /// A late publisher safely creates a fresh entry through [`Self::with_session`].
    pub fn forget(&self, session: SessionId) {
        self.shared
            .channels
            .lock()
            .expect("code event bus lock")
            .remove(&session);
        self.shared
            .session_trees
            .lock()
            .expect("code session tree lock")
            .remove(&session);
    }

    /// Lock covering one parent's tree compute and journal write.
    pub fn session_tree_gate(&self, session: SessionId) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let mut gates = self
            .shared
            .session_tree_gates
            .lock()
            .expect("code session tree gate lock");
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&session).and_then(std::sync::Weak::upgrade) {
            return gate;
        }
        let gate = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        gates.insert(session, std::sync::Arc::downgrade(&gate));
        gate
    }

    /// Whether this parent already journaled this exact tree.
    pub fn session_tree_is_current(
        &self,
        session: SessionId,
        children: &[SessionTreeChild],
        wait: &Option<SessionTreeWait>,
    ) -> bool {
        self.shared
            .session_trees
            .lock()
            .expect("code session tree lock")
            .get(&session)
            .is_some_and(|(current_children, current_wait)| {
                current_children == children && current_wait == wait
            })
    }

    /// Remember the tree just journaled on this parent.
    pub fn remember_session_tree(
        &self,
        session: SessionId,
        children: Vec<SessionTreeChild>,
        wait: Option<SessionTreeWait>,
    ) {
        self.shared
            .session_trees
            .lock()
            .expect("code session tree lock")
            .insert(session, (children, wait));
    }

    /// Whether this session might be carrying [`AttentionState::Stalled`].
    ///
    /// A hint, not an answer: it starts pessimistic and is corrected by the
    /// first caller that reads the row. Its job is to spare the common path —
    /// a running session that is not stalled — a `get_session` per event.
    pub fn maybe_stalled(&self, session: SessionId) -> bool {
        self.shared
            .channels
            .lock()
            .expect("code event bus lock")
            .get(&session)
            .is_none_or(|live| live.maybe_stalled)
    }

    /// Record what the session row actually says, so the next event can skip
    /// the read.
    pub fn set_maybe_stalled(&self, session: SessionId, stalled: bool) {
        self.with_session(session, |live| live.maybe_stalled = stalled);
    }

    /// Ask to publish this session's digest, and learn when to.
    ///
    /// The first request after [`DIGEST_INTERVAL`] of quiet may publish now.
    /// A request inside the interval schedules one deferred digest for its
    /// end, and every later request folds into that one. The caller that
    /// gets [`DigestSlot::At`] must publish then, after calling
    /// [`Self::deferred_digest_due`].
    pub fn claim_digest(&self, session: SessionId) -> DigestSlot {
        let now = Instant::now();
        let mut pace = self
            .shared
            .digest_pace
            .lock()
            .expect("code digest pace lock");
        if pace.len() > 256 {
            pace.retain(|_, entry| entry.deferred || now - entry.last < DIGEST_INTERVAL);
        }
        match pace.get_mut(&session) {
            Some(entry) if entry.deferred => DigestSlot::Scheduled,
            Some(entry) if now - entry.last < DIGEST_INTERVAL => {
                entry.deferred = true;
                DigestSlot::At(entry.last + DIGEST_INTERVAL)
            }
            _ => {
                pace.insert(
                    session,
                    DigestPace {
                        last: now,
                        deferred: false,
                    },
                );
                DigestSlot::Now
            }
        }
    }

    /// A deferred digest is about to be built. Requests from here on ask
    /// for a new one, because this one may already have read the session.
    pub fn deferred_digest_due(&self, session: SessionId) {
        self.shared
            .digest_pace
            .lock()
            .expect("code digest pace lock")
            .insert(
                session,
                DigestPace {
                    last: Instant::now(),
                    deferred: false,
                },
            );
    }

    /// The wake a worker waits on while this session's turn is parked.
    ///
    /// Take it before the first read of the park: a wake that lands between
    /// that read and the wait is kept, not lost. The wake lives as long as
    /// the worker holds it.
    pub fn park_wake(&self, session: SessionId) -> Arc<Notify> {
        let mut wakes = self.shared.park_wakes.lock().expect("code park wake lock");
        wakes.retain(|_, wake| wake.strong_count() > 0);
        if let Some(wake) = wakes.get(&session).and_then(Weak::upgrade) {
            return wake;
        }
        let wake = Arc::new(Notify::new());
        wakes.insert(session, Arc::downgrade(&wake));
        wake
    }

    /// Tell a worker parked on this session that what it waits on may have
    /// settled, so it rereads its park now rather than at its next poll. A
    /// session with no parked worker ignores it.
    pub fn wake_parked(&self, session: SessionId) {
        let wake = self
            .shared
            .park_wakes
            .lock()
            .expect("code park wake lock")
            .get(&session)
            .and_then(Weak::upgrade);
        if let Some(wake) = wake {
            wake.notify_one();
        }
    }

    fn updates_sender(&self, owner: &OwnerId) -> broadcast::Sender<CodeLiveUpdate> {
        self.shared
            .updates
            .lock()
            .expect("code updates bus lock")
            .entry(owner.clone())
            .or_insert_with(|| broadcast::channel(UPDATES_BUFFER).0)
            .clone()
    }

    /// Subscribe to one owner's updates. The receiver is the only view of the
    /// channel available to an `/updates` socket, and it carries nothing else.
    ///
    /// Subscribing also wakes [`Self::updates_attached`] waiters. A client
    /// says it is looking by opening an `/updates` socket. Sweeps
    /// that back off while nobody is looking use that moment to return to
    /// their fast cadence.
    pub fn subscribe_updates(&self, owner: &OwnerId) -> broadcast::Receiver<CodeLiveUpdate> {
        let receiver = self.updates_sender(owner).subscribe();
        self.shared.updates_attached.notify_one();
        receiver
    }

    /// Whether any client currently holds an `/updates` subscription.
    ///
    /// This is the server's "someone is looking" signal for code mode: the
    /// desktop keeps that socket open for as long as it runs, and the CLI
    /// opens it to watch. Counting receivers is exact because a dropped
    /// socket drops its receiver with it.
    pub fn has_updates_subscribers(&self) -> bool {
        self.shared
            .updates
            .lock()
            .expect("code updates bus lock")
            .values()
            .any(|sender| sender.receiver_count() > 0)
    }

    /// Resolve when a client next subscribes to `/updates`.
    ///
    /// One pending wake is retained if nobody is waiting when a subscription
    /// lands, so a caller that checks [`Self::has_updates_subscribers`] and
    /// then waits cannot miss an attach that fell between the two. The cost
    /// is at most one spurious wake, which callers absorb by re-reading the
    /// subscriber count.
    pub async fn updates_attached(&self) {
        self.shared.updates_attached.notified().await;
    }

    /// Publish a notice to one owner. Publishers name the owner the notice
    /// belongs to; there is no channel that reaches everyone.
    pub fn publish_update(&self, owner: &OwnerId, update: CodeLiveUpdate) {
        let _ = self.updates_sender(owner).send(update);
    }

    /// Every principal that currently holds an `/updates` subscription.
    ///
    /// `deployment` visibility admits any authenticated principal, which is
    /// not a set the store can enumerate (decision 0086). This is the set that
    /// could observe a notice at all, so a session readable by everyone
    /// reaches its readers through it.
    pub fn attached_owners(&self) -> Vec<OwnerId> {
        self.shared
            .updates
            .lock()
            .expect("code updates bus lock")
            .iter()
            .filter(|(_, sender)| sender.receiver_count() > 0)
            .map(|(owner, _)| owner.clone())
            .collect()
    }
}

/// Does this journaled event end the assistant's current run of text?
///
/// `assistant_message` states the whole run, and a parent-level tool call or
/// a turn boundary closes it the same way the renderer's reducer does. Child
/// (subagent) messages and calls are excluded: they never owned the parent's
/// buffer.
fn ends_assistant_text(event: &Event) -> bool {
    matches!(
        event,
        Event::AssistantMessage {
            parent_call_id: None,
            ..
        } | Event::ToolStarted {
            parent_call_id: None,
            ..
        } | Event::TurnStarted { .. }
            | Event::TurnResumed { .. }
            | Event::TurnCompleted { .. }
            | Event::TurnRefused { .. }
            | Event::TurnFailed { .. }
            | Event::TurnInterrupted { .. }
    )
}

fn append_bounded(buffer: &mut String, text: &str) {
    let room = MAX_EVENT_TEXT_CHARS.saturating_sub(buffer.chars().count());
    if room == 0 {
        return;
    }
    match text.char_indices().nth(room) {
        Some((cut, _)) => buffer.push_str(&text[..cut]),
        None => buffer.push_str(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn removing_parent_preserves_an_active_tree_gate() {
        let bus = CodeEventBus::default();
        let parent = SessionId::new();
        let gate = bus.session_tree_gate(parent);
        bus.forget(parent);
        assert!(std::sync::Arc::ptr_eq(
            &gate,
            &bus.session_tree_gate(parent)
        ));
        drop(gate);
        let other = SessionId::new();
        let _other_gate = bus.session_tree_gate(other);
        assert!(!bus
            .shared
            .session_tree_gates
            .lock()
            .unwrap()
            .contains_key(&parent));
    }

    #[tokio::test]
    async fn a_park_wake_sent_before_the_worker_waits_is_kept() {
        let bus = CodeEventBus::default();
        let session = SessionId::new();
        // Nobody is parked yet, so there is nothing to wake.
        bus.wake_parked(session);
        let wake = bus.park_wake(session);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), wake.notified())
                .await
                .is_err(),
            "a wake sent before anyone parked reached a later worker"
        );

        // A settlement between the worker's read of its park and its wait.
        bus.wake_parked(session);
        tokio::time::timeout(std::time::Duration::from_secs(1), wake.notified())
            .await
            .expect("the wake sent before the wait was lost");

        drop(wake);
        let _other = bus.park_wake(SessionId::new());
        assert!(!bus.shared.park_wakes.lock().unwrap().contains_key(&session));
    }

    #[tokio::test]
    async fn updates_subscribers_are_counted_and_attach_wakes_waiters() {
        let bus = CodeEventBus::default();
        let owner = OwnerId::new("owner-a").unwrap();
        assert!(!bus.has_updates_subscribers());

        let receiver = bus.subscribe_updates(&owner);
        assert!(bus.has_updates_subscribers());
        // The attach that landed before anyone waited is retained, so a sweep
        // that checked the count first and then waited still wakes.
        tokio::time::timeout(Duration::from_secs(1), bus.updates_attached())
            .await
            .expect("subscribing wakes the attach signal");

        drop(receiver);
        assert!(!bus.has_updates_subscribers());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), bus.updates_attached())
                .await
                .is_err(),
            "a drop is not an attach"
        );
    }

    #[test]
    fn forgetting_a_session_drops_its_channel_and_late_publish_recreates_it() {
        let bus = CodeEventBus::default();
        let session = SessionId::new();

        let _receiver = bus.attach(session).0;
        assert!(bus.last_activity(session).is_some());

        bus.forget(session);
        assert!(bus.last_activity(session).is_none());

        bus.publish_transient(
            session,
            Event::AssistantDelta {
                text: "late".into(),
            },
        );
        assert!(bus.last_activity(session).is_some());
    }
}
