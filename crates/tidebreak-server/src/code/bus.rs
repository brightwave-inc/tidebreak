//! In-memory live fan-out for one code-mode session.
//!
//! The journal is the durable record a client replays on connect; this bus is
//! the live tail. The session worker appends each event to the journal and
//! then publishes it here with its assigned `seq`.

use std::collections::HashMap;
use std::sync::Mutex;

use tidebreak_core::{CodeSessionId, SequencedCodeEvent};
use tokio::sync::broadcast;

const LIVE_BUFFER: usize = 256;

/// Per-session broadcast channels for live journal events.
#[derive(Default)]
pub(crate) struct CodeEventBus {
    channels: Mutex<HashMap<CodeSessionId, broadcast::Sender<SequencedCodeEvent>>>,
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
}
