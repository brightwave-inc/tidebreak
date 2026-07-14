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

use openwave_core::{ChatId, SequencedEvent};

/// How many live events a slow subscriber may fall behind before it's dropped
/// (`Lagged`). A lagging client is expected to reconnect and replay from the
/// journal with an `after` cursor, so this only bounds memory, not correctness.
const LIVE_BUFFER: usize = 256;

/// Per-chat broadcast channels for live turn events.
#[derive(Default)]
pub struct EventBus {
    channels: Mutex<HashMap<ChatId, broadcast::Sender<SequencedEvent>>>,
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
}
