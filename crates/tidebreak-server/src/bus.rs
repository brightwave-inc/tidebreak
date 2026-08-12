//! In-memory live event fan-out, keyed by chat.
//!
//! The journal (in the store) is the durable record a client replays on connect;
//! this bus is the *live* tail. As a turn runs, the worker appends each event to the
//! journal and republishes it here with its assigned `seq`, so a connected
//! WebSocket sees it immediately. A client that isn't connected misses nothing —
//! it replays from the journal when it connects.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::broadcast;

use tidebreak_core::{ChatId, SequencedEvent, TurnId};

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
/// These are deliberately not [`SequencedEvent`]s. The journal records what
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
    channels: Mutex<HashMap<ChatId, broadcast::Sender<SequencedEvent>>>,
    metadata: Mutex<HashMap<ChatId, broadcast::Sender<ChatMetadataNotice>>>,
}

impl EventBus {
    /// The broadcast sender for a chat, created on first use. Kept in the map so
    /// the channel outlives any individual turn or subscriber.
    pub fn sender(&self, chat: ChatId) -> broadcast::Sender<SequencedEvent> {
        self.channels
            .lock()
            .unwrap()
            .entry(chat)
            .or_insert_with(|| broadcast::channel(LIVE_BUFFER).0)
            .clone()
    }

    /// Subscribe to a chat's live events. Events published after this call are
    /// delivered; a client pairs this with a journal replay to cover the past.
    pub fn subscribe(&self, chat: ChatId) -> broadcast::Receiver<SequencedEvent> {
        self.sender(chat).subscribe()
    }

    /// Subscribe to a chat's metadata notices.
    pub fn subscribe_metadata(&self, chat: ChatId) -> broadcast::Receiver<ChatMetadataNotice> {
        self.metadata_sender(chat).subscribe()
    }

    /// Announce a metadata change to whoever is watching this chat right now.
    ///
    /// Nothing is retained for a client that connects later, and no caller checks
    /// the result: the durable write already happened, and this is only how an
    /// open window learns about it without asking.
    pub fn publish_metadata(&self, chat: ChatId, notice: ChatMetadataNotice) {
        let _ = self.metadata_sender(chat).send(notice);
    }

    fn metadata_sender(&self, chat: ChatId) -> broadcast::Sender<ChatMetadataNotice> {
        self.metadata
            .lock()
            .unwrap()
            .entry(chat)
            .or_insert_with(|| broadcast::channel(METADATA_BUFFER).0)
            .clone()
    }
}
