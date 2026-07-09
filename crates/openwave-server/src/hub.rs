//! Running a turn, journaling what it emits, and fanning it out live.
//!
//! A turn's [`AgentEvent`](openwave_core::AgentEvent)s flow through an unbounded
//! channel: the agent drives the turn on one side, and this hub drains the other,
//! appending each event to the store's per-chat journal and republishing it on the
//! live [`EventBus`](crate::bus::EventBus) with its assigned `seq`. The journal is
//! what a client replays on connect; the bus is the live tail. Together they let
//! the WebSocket surface do snapshot → replay → live without gaps.

use std::sync::Arc;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;

use openwave_core::{Agent, Chat, SequencedEvent, Store};

use crate::bus::EventBus;
use crate::state::ActiveTurn;

/// Drive one turn to completion, journaling every event it emits and publishing
/// it to the live bus.
///
/// The drive and the journal run concurrently, so events land in the store — and
/// on the bus — as the turn produces them. Returns once the turn has finished and
/// every event is persisted.
///
/// The [`ActiveTurn`] slot is held until the agent has returned and the journal
/// has drained, so the next turn cannot race this turn's sequence assignment.
pub(crate) async fn drive_and_journal(
    agent: Agent,
    chat: Chat,
    input: String,
    store: Arc<dyn Store>,
    events: Arc<EventBus>,
    active: ActiveTurn,
) {
    let (events_tx, mut events_rx) = unbounded();
    let chat_id = chat.id;
    let live = events.sender(chat_id);

    let journal = async move {
        while let Some(event) = events_rx.next().await {
            // The seq the journal assigns is the same one the live event carries,
            // so a client can dedup replay vs. live by seq. A write failure can't
            // be surfaced to the (already-returned) client; skip publishing it so
            // the live stream never carries an event that isn't in the journal.
            if let Ok(seq) = store.append_event(chat_id, &event).await {
                // `send` errors only when no one is subscribed — fine, that client
                // will replay from the journal when it connects.
                let _ = live.send(SequencedEvent { seq, event });
            }
        }
    };

    // Hold the slot across both phases. Dropping `events_tx` closes the channel,
    // then awaiting the journal guarantees every queued event has its seq before
    // another turn for this chat can start.
    let _active = active;
    let _ = agent.run_turn(&chat, &input, &events_tx).await;
    // Agent is done — refuse further cancel/steer (would be 202-with-no-effect)
    // while the slot stays held for journal drain / seq safety.
    _active.close_ingress();
    drop(events_tx);
    journal.await;
}
