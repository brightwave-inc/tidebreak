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
use futures::future::{self, Either};
use futures::StreamExt;

use openwave_core::{Agent, Chat, SequencedEvent, Store};

use crate::bus::EventBus;
use crate::state::ActiveTurn;

/// Drive one turn to completion, journaling every event it emits and publishing
/// it to the live bus.
///
/// The drive and the journal run concurrently, so events land in the store — and
/// on the bus — as the turn produces them (including while parked on approval).
/// Returns once the turn has finished and every event is persisted.
///
/// The [`ActiveTurn`] slot is held until the agent has returned *and* the journal
/// has drained, so the next turn cannot race this turn's sequence assignment.
/// Cancel/steer ingress closes as soon as the agent returns, so late POSTs get
/// `409` rather than a silent `202` during journal drain.
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

    // Hold the slot across both phases. Drive and journal run together so a
    // turn parked on approval still journals `ApprovalRequired` live.
    let _active = active;
    let drive = async move {
        // Dropping `events_tx` when this future ends closes the channel and
        // lets the journal finish after draining what was already queued.
        let _ = agent.run_turn(&chat, &input, &events_tx).await;
    };

    match future::select(Box::pin(drive), Box::pin(journal)).await {
        Either::Left(((), journal)) => {
            // Agent finished — refuse further cancel/steer while the journal
            // drains under the still-held slot.
            _active.close_ingress();
            journal.await;
        }
        Either::Right(((), drive)) => {
            // Journal ended first (channel closed unexpectedly). Still close
            // ingress and wait for the agent so we don't drop mid-turn.
            _active.close_ingress();
            drive.await;
        }
    }
}
