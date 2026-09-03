//! Event emission helpers for live and claimed turns.

use std::sync::atomic::{AtomicI32, Ordering};

use futures::channel::{mpsc::UnboundedSender, oneshot};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedAgentEvent};
use crate::provider::Usage;

#[derive(Default)]
pub(crate) struct AgentProgress {
    pub(crate) usage: Usage,
    pub(crate) model_steps: usize,
}

/// One emission from a durably claimed agent generation.
///
/// Ordinary events still need the worker to append them. A committed event was
/// journaled atomically with another state transition and only needs live
/// publication. Flush barriers let the agent wait until every preceding
/// ordinary event is durable before it performs such a transition.
pub enum ClaimedAgentEvent {
    /// Append this event under its exact attempt ordinal.
    Pending { ordinal: i32, event: AgentEvent },
    /// Publish an event whose journal transaction already committed.
    Committed {
        ordinal: i32,
        event: SequencedAgentEvent,
    },
    /// Consume an already committed event ordinal without live publication.
    Recovered {
        ordinal: i32,
        event: SequencedAgentEvent,
    },
    /// Acknowledge after all preceding channel items have been handled.
    Flush(oneshot::Sender<()>),
}

pub(crate) enum EventSink<'a> {
    Legacy(&'a UnboundedSender<AgentEvent>),
    Claimed {
        sender: &'a UnboundedSender<ClaimedAgentEvent>,
        next_ordinal: AtomicI32,
    },
}

impl EventSink<'_> {
    pub(crate) fn send(&self, event: AgentEvent) {
        match self {
            Self::Legacy(sender) => {
                let _ = sender.unbounded_send(event);
            }
            Self::Claimed {
                sender,
                next_ordinal,
            } => {
                if let Ok(ordinal) = reserve_event_ordinal(next_ordinal) {
                    let _ = sender.unbounded_send(ClaimedAgentEvent::Pending { ordinal, event });
                }
            }
        }
    }

    pub(crate) async fn flush(&self) -> Result<()> {
        let Self::Claimed { sender, .. } = self else {
            return Ok(());
        };
        let (acknowledge, acknowledged) = oneshot::channel();
        sender
            .unbounded_send(ClaimedAgentEvent::Flush(acknowledge))
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))?;
        acknowledged
            .await
            .map_err(|_| AgentError::Store("claimed turn event flush was abandoned".into()))
    }

    pub(crate) fn reserve_ordinal(&self) -> Result<i32> {
        match self {
            Self::Claimed { next_ordinal, .. } => reserve_event_ordinal(next_ordinal),
            Self::Legacy(_) => Err(AgentError::Store(
                "legacy turn cannot reserve a durable event ordinal".into(),
            )),
        }
    }

    pub(crate) fn send_committed(&self, ordinal: i32, event: SequencedAgentEvent) -> Result<()> {
        let Self::Claimed { sender, .. } = self else {
            return Err(AgentError::Store(
                "legacy turn cannot publish a committed durable event".into(),
            ));
        };
        sender
            .unbounded_send(ClaimedAgentEvent::Committed { ordinal, event })
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))
    }

    pub(crate) fn proposed_ordinal(&self) -> Result<Option<i32>> {
        match self {
            Self::Legacy(_) => Ok(None),
            Self::Claimed { next_ordinal, .. } => {
                let ordinal = next_ordinal.load(Ordering::SeqCst);
                if !(1..i32::MAX).contains(&ordinal) {
                    return Err(AgentError::Store("turn event ordinal exhausted".into()));
                }
                Ok(Some(ordinal))
            }
        }
    }

    pub(crate) fn send_committed_proposed(
        &self,
        ordinal: i32,
        event: SequencedAgentEvent,
    ) -> Result<()> {
        self.send_recovered_or_committed_proposed(ordinal, event, true)
    }

    pub(crate) fn send_recovered_proposed(
        &self,
        ordinal: i32,
        event: SequencedAgentEvent,
    ) -> Result<()> {
        self.send_recovered_or_committed_proposed(ordinal, event, false)
    }

    pub(crate) fn send_recovered_or_committed_proposed(
        &self,
        ordinal: i32,
        event: SequencedAgentEvent,
        publish: bool,
    ) -> Result<()> {
        let Self::Claimed {
            sender,
            next_ordinal,
        } = self
        else {
            return Err(AgentError::Store(
                "legacy turn cannot publish a committed durable event".into(),
            ));
        };
        let next = ordinal
            .checked_add(1)
            .filter(|next| *next < i32::MAX)
            .ok_or_else(|| AgentError::Store("turn event ordinal exhausted".into()))?;
        next_ordinal
            .compare_exchange(ordinal, next, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| AgentError::Store("turn event ordinal changed during approval".into()))?;
        sender
            .unbounded_send(if publish {
                ClaimedAgentEvent::Committed { ordinal, event }
            } else {
                ClaimedAgentEvent::Recovered { ordinal, event }
            })
            .map_err(|_| AgentError::Store("claimed turn event channel closed".into()))
    }
}

/// Forwards provider events without server-side citation rewriting.
pub(crate) struct AssistantStreamEventFilter<'a, 'b> {
    sink: &'a EventSink<'b>,
}

impl<'a, 'b> AssistantStreamEventFilter<'a, 'b> {
    pub(crate) fn new(sink: &'a EventSink<'b>) -> Self {
        Self { sink }
    }

    pub(crate) fn send(&mut self, event: AgentEvent) {
        self.sink.send(event);
    }

    pub(crate) fn send_text(&mut self, delta: &str) {
        self.sink.send(AgentEvent::TextDelta {
            text: delta.to_owned(),
        });
    }

    pub(crate) fn finish(&mut self) {
        // Nothing is buffered.
    }

    pub(crate) fn discard(&mut self) {
        // Nothing is buffered — the discard itself is the separately-sent
        // `StreamInterrupted` event, not anything this hook does.
    }
}

pub(crate) fn reserve_event_ordinal(next: &AtomicI32) -> Result<i32> {
    next.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |ordinal| {
        ordinal.checked_add(1).filter(|next| *next < i32::MAX)
    })
    .map_err(|_| AgentError::Store("turn event ordinal exhausted".into()))
}
