//! In-memory live event fan-out, keyed by chat.
//!
//! The journal (in the store) is the durable record a client replays on connect;
//! this bus is the *live* tail. As a turn runs, the worker appends each event to the
//! journal and republishes it here with its assigned `seq`, so a connected
//! WebSocket sees it immediately. A client that isn't connected misses nothing —
//! it replays from the journal when it connects.
//!
//! The journal is the one code journal (decision 48 step 5), and a chat is a
//! session on it, so every event published here is mirrored onto the
//! session's channel of the [`CodeEventBus`] in the row's own vocabulary.
//! The internal engine follows that channel, and so does a code-wire reader
//! of the same session; the chat channel stays what the chat routes serve.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;

use tidebreak_core::code::SequencedEvent;
use tidebreak_core::{chat_journal, SequencedAgentEvent, SessionId, TurnId};

use crate::code::bus::CodeEventBus;

/// How many live events a slow subscriber may fall behind before it's dropped
/// (`Lagged`). A lagging client is expected to reconnect and replay from the
/// journal with an `after` cursor, so this only bounds memory, not correctness.
const LIVE_BUFFER: usize = 256;

/// How many metadata notices a chat buffers for its subscribers.
///
/// Far smaller than the event buffer because these are rare — a conversation
/// gets named once — and losing one is not a correctness problem: the value is
/// already durable, so a client that misses the notice sees it in the chat's
/// next read.
const METADATA_BUFFER: usize = 8;

/// Chat state that changed without being turn history.
///
/// These are deliberately not [`SequencedAgentEvent`]s. The journal records what
/// happened inside a turn, in an order a client resumes from; a conversation's
/// name is neither — it is written beside a turn, sometimes after it ends, by
/// work that holds no lease. Carrying it here keeps chat metadata out of the
/// journal while still letting an open client see it the moment it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatMetadataNotice {
    /// The chat was given a name it did not have.
    Titled { title: String },
    /// A terminal turn's connected-folder write report is durable and readable.
    FileChangesRecorded { turn_id: TurnId },
    /// A terminal turn's memory capture wrote at least one proposal, and the
    /// records are durable and readable.
    MemoryProposalsRecorded { turn_id: TurnId },
    /// Whether code execution is currently preparing its sandbox image before
    /// it can run anything.
    ///
    /// Deliberately live-only, like everything else here: it describes what is
    /// happening right now, and a journaled row would still be asserting it
    /// months later. A client that connects mid-pull learns about it on the
    /// next report rather than from replay, which is the right trade for a
    /// state whose whole meaning is "still waiting".
    SandboxPreparing { preparing: bool },
}

/// Per-chat broadcast channels for live turn events and metadata notices.
#[derive(Default)]
pub struct EventBus {
    channels: Mutex<HashMap<SessionId, broadcast::Sender<SequencedAgentEvent>>>,
    metadata: Mutex<HashMap<SessionId, broadcast::Sender<ChatMetadataNotice>>>,
    /// The session-keyed bus every journaled chat event is mirrored onto.
    /// Installed once the code runtime exists; absent in tests that assemble
    /// state without one, where nothing follows the session channel.
    mirror: OnceLock<Arc<CodeEventBus>>,
}

/// The publishing half of one chat's live channel.
///
/// Sends the journaled event to the chat's subscribers and mirrors it, as the
/// code journal row it was stored as, onto the session's channel.
pub struct ChatEventSender {
    chat: SessionId,
    sender: broadcast::Sender<SequencedAgentEvent>,
    mirror: Option<Arc<CodeEventBus>>,
}

impl ChatEventSender {
    /// Publish one journaled event and say how many chat subscribers took
    /// it. Zero is not an error: the durable write already happened, and a
    /// chat nobody is watching streams to no one.
    pub fn send(&self, event: SequencedAgentEvent) -> usize {
        if let Some(mirror) = &self.mirror {
            mirror.publish(
                SessionId(self.chat.0),
                SequencedEvent {
                    seq: event.seq,
                    event: chat_journal::journal_row(&event.event),
                },
            );
        }
        self.sender.send(event).unwrap_or(0)
    }

    /// Publish a journal row stored in the code vocabulary: the row itself
    /// goes onto the session's channel, and its chat reading — when it has
    /// one — to the chat's subscribers. An approval settlement carries more
    /// than its chat reading (a grant scope, denial feedback), so it is
    /// mirrored as stored rather than re-derived.
    pub fn send_row(&self, row: SequencedEvent) -> usize {
        let chat_event = chat_journal::chat_event(row.event.clone())
            .ok()
            .flatten()
            .map(|event| SequencedAgentEvent {
                seq: row.seq,
                event,
            });
        if let Some(mirror) = &self.mirror {
            mirror.publish(SessionId(self.chat.0), row);
        }
        chat_event
            .map(|event| self.sender.send(event).unwrap_or(0))
            .unwrap_or(0)
    }

    /// Publish to the chat's subscribers only, for a row the session
    /// channel already carried.
    pub fn send_unmirrored(&self, event: SequencedAgentEvent) -> usize {
        self.sender.send(event).unwrap_or(0)
    }
}

impl EventBus {
    /// Mirror every journaled chat event onto the session bus. Called once,
    /// when the code runtime that owns the bus is assembled.
    pub fn mirror_into(&self, bus: Arc<CodeEventBus>) {
        let _ = self.mirror.set(bus);
    }

    /// The publisher for a chat's live channel, created on first use. The
    /// channel is kept in the map so it outlives any individual turn or
    /// subscriber.
    pub fn sender(&self, chat: SessionId) -> ChatEventSender {
        ChatEventSender {
            chat,
            sender: self.channel(chat),
            mirror: self.mirror.get().cloned(),
        }
    }

    fn channel(&self, chat: SessionId) -> broadcast::Sender<SequencedAgentEvent> {
        self.channels
            .lock()
            .unwrap()
            .entry(chat)
            .or_insert_with(|| broadcast::channel(LIVE_BUFFER).0)
            .clone()
    }

    /// Subscribe to a chat's live events. Events published after this call are
    /// delivered; a client pairs this with a journal replay to cover the past.
    pub fn subscribe(&self, chat: SessionId) -> broadcast::Receiver<SequencedAgentEvent> {
        self.channel(chat).subscribe()
    }

    /// Subscribe to a chat's metadata notices.
    pub fn subscribe_metadata(&self, chat: SessionId) -> broadcast::Receiver<ChatMetadataNotice> {
        self.metadata_sender(chat).subscribe()
    }

    /// Announce a metadata change to whoever is watching this chat right now.
    ///
    /// Nothing is retained for a client that connects later, and no caller checks
    /// the result: the durable write already happened, and this is only how an
    /// open window learns about it without asking.
    pub fn publish_metadata(&self, chat: SessionId, notice: ChatMetadataNotice) {
        let _ = self.metadata_sender(chat).send(notice);
    }

    fn metadata_sender(&self, chat: SessionId) -> broadcast::Sender<ChatMetadataNotice> {
        self.metadata
            .lock()
            .unwrap()
            .entry(chat)
            .or_insert_with(|| broadcast::channel(METADATA_BUFFER).0)
            .clone()
    }
}
