//! Running a turn and journaling what it emits.
//!
//! A turn's [`AgentEvent`](openwave_core::AgentEvent)s flow through an unbounded
//! channel: the agent drives the turn on one side, and this hub drains the other,
//! appending each event to the store's per-chat journal as it arrives. The journal
//! is what a client replays on connect, so persisting here (rather than in the
//! handler after the fact) is what will let the WebSocket surface do
//! snapshot → replay → live in the next slice.

use std::sync::Arc;

use futures::channel::mpsc::unbounded;
use futures::{future, StreamExt};

use openwave_core::{Agent, Chat, Store};

/// Drive one turn to completion, journaling every event it emits.
///
/// The drive and the journal run concurrently, so events land in the store as the
/// turn produces them. Returns once the turn has finished and every event is
/// persisted.
pub(crate) async fn drive_and_journal(
    agent: Agent,
    chat: Chat,
    input: String,
    store: Arc<dyn Store>,
) {
    let (events_tx, mut events_rx) = unbounded();
    let chat_id = chat.id;

    let journal = async move {
        while let Some(event) = events_rx.next().await {
            // A journal write failure can't be surfaced to the (already-returned)
            // client; the turn continues and the live stream still carries the
            // event once the WS surface lands.
            let _ = store.append_event(chat_id, &event).await;
        }
    };

    let drive = async move {
        // Dropping `events_tx` at the end of this future closes the channel,
        // which ends the journal loop.
        let _ = agent.run_turn(&chat, &input, &events_tx).await;
    };

    future::join(drive, journal).await;
}
