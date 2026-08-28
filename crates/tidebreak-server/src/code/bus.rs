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

use chrono::{DateTime, Utc};
use tidebreak_core::{
    Attention, CodeEvent, CodeSessionActivity, CodeSessionId, CodeSessionKind,
    CodeSessionLifecycle, CodeSubagentSummary, CodeTurnId, CodeWatchState, HarnessKind,
    MAX_EVENT_TEXT_CHARS, OwnerId, PullRequestDigest, RepoId, SequencedCodeEvent, WorkspaceId,
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
    /// Engine identity for list surfaces that collapse several sessions into
    /// one workspace row.
    pub harness_kind: HarnessKind,
    pub lifecycle: CodeSessionLifecycle,
    pub attention: Attention,
    pub title: String,
    pub turn_count: i64,
    /// What the live turn is occupied with. Set only while lifecycle is
    /// running; older clients safely fall back to a generic running label.
    pub activity: Option<CodeSessionActivity>,
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

/// Progress of one warm harness install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HarnessInstallProgress {
    pub kind: HarnessKind,
    pub version: Option<String>,
    pub phase: String,
    pub done: bool,
    pub error: Option<String>,
}

/// One unsequenced notice on the install-wide updates channel.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CodeLiveUpdate {
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
}

/// Progress of one background rewrite of a completed turn's closing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnRewriteNotice {
    pub session: CodeSessionId,
    pub turn_id: CodeTurnId,
    pub state: TurnRewriteState,
    pub rewrite: Option<String>,
}

/// Where one rewrite stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnRewriteState {
    Rewriting,
    Rewritten,
    Failed,
}

/// Per-session broadcast channels for live journal events, plus one digest
/// channel per owner.
pub(crate) struct CodeEventBus {
    channels: Mutex<HashMap<CodeSessionId, LiveSession>>,
    updates: Mutex<HashMap<OwnerId, broadcast::Sender<CodeLiveUpdate>>>,
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
pub(crate) struct CodeLiveEvent {
    pub seq: Option<i64>,
    /// Journal position this event streamed behind — its own, for a journaled
    /// event.
    ///
    /// A live-only event is only current while the journal has not moved past
    /// it. Once it has, the event that moved it (the message, the tool call,
    /// the turn's end) restates or discards this text, and a reader whose
    /// replay already read that far must not be handed the fragment as well.
    pub cursor: i64,
    pub event: CodeEvent,
}

/// What a freshly attached reader needs to catch up on the live tail.
pub(crate) struct LiveTail {
    /// Assistant text streamed but not yet journaled. Empty most of the time.
    pub assistant: String,
    /// Journal position the tail is current as of. A reader whose replay went
    /// past this cannot trust the text.
    pub cursor: i64,
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
    fn with_session<T>(
        &self,
        session: CodeSessionId,
        act: impl FnOnce(&mut LiveSession) -> T,
    ) -> T {
        let mut channels = self.channels.lock().expect("code event bus lock");
        act(channels.entry(session).or_default())
    }

    /// Subscribe, and take the live tail in the same breath.
    ///
    /// The two have to be atomic. A caller that subscribed first and read the
    /// tail afterwards would count any delta published in between twice: once
    /// in the tail it read, once from its own receiver. Publication takes the
    /// same lock, so nothing lands between these two lines.
    pub(crate) fn attach(
        &self,
        session: CodeSessionId,
    ) -> (broadcast::Receiver<CodeLiveEvent>, LiveTail) {
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
    pub(crate) fn publish(&self, session: CodeSessionId, event: SequencedCodeEvent) {
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
    pub(crate) fn publish_transient(&self, session: CodeSessionId, event: CodeEvent) {
        self.with_session(session, |live| {
            live.last_activity = Utc::now();
            if let CodeEvent::AssistantDelta { text } = &event {
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
    pub(crate) fn take_assistant_tail(&self, session: CodeSessionId) -> String {
        self.with_session(session, |live| std::mem::take(&mut live.assistant))
    }

    /// Most recent moment this session published anything, live or journaled.
    ///
    /// The stall sweep reads it. Without it a session streaming a long answer
    /// and nothing else would look silent, because the deltas carrying that
    /// answer no longer touch a row's `created_at`.
    pub(crate) fn last_activity(&self, session: CodeSessionId) -> Option<DateTime<Utc>> {
        self.channels
            .lock()
            .expect("code event bus lock")
            .get(&session)
            .map(|live| live.last_activity)
    }

    /// Whether this session might be carrying [`AttentionState::Stalled`].
    ///
    /// A hint, not an answer: it starts pessimistic and is corrected by the
    /// first caller that reads the row. Its job is to spare the common path —
    /// a running session that is not stalled — a `get_session` per event.
    pub(crate) fn maybe_stalled(&self, session: CodeSessionId) -> bool {
        self.channels
            .lock()
            .expect("code event bus lock")
            .get(&session)
            .is_none_or(|live| live.maybe_stalled)
    }

    /// Record what the session row actually says, so the next event can skip
    /// the read.
    pub(crate) fn set_maybe_stalled(&self, session: CodeSessionId, stalled: bool) {
        self.with_session(session, |live| live.maybe_stalled = stalled);
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

/// Does this journaled event end the assistant's current run of text?
///
/// `assistant_message` states the whole run, and a parent-level tool call or
/// a turn boundary closes it the same way the renderer's reducer does. Child
/// (subagent) messages and calls are excluded: they never owned the parent's
/// buffer.
fn ends_assistant_text(event: &CodeEvent) -> bool {
    matches!(
        event,
        CodeEvent::AssistantMessage {
            parent_call_id: None,
            ..
        } | CodeEvent::ToolStarted {
            parent_call_id: None,
            ..
        } | CodeEvent::TurnStarted { .. }
            | CodeEvent::TurnCompleted { .. }
            | CodeEvent::TurnFailed { .. }
            | CodeEvent::TurnInterrupted
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
