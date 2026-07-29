//! The resumable, monotonically sequenced event stream.
//!
//! Events carry the sandbox's monotonic [`Sequence`]. The host resumes from its
//! last committed [`EventCursor`], commits a batch, and advances its cursor in
//! one transaction; a re-delivered sequence at or below the cursor is discarded,
//! so a crash between read and commit re-reads without duplicating and never
//! skips. The wire types here are what that contract is expressed over; the
//! reference backend and the conformance suite exercise it.

use serde::{Deserialize, Serialize};

use crate::{
    ids::{EventCursor, Sequence},
    protocol::MAX_EVENT_PAYLOAD_BYTES,
};

/// One event the sandbox emits, stamped with its monotonic sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxEvent {
    /// Monotonic per-run sequence; ordering is the stream's contract.
    pub sequence: Sequence,
    /// The bounded payload.
    pub payload: EventPayload,
}

/// The bounded content of one event.
///
/// The variants are deliberately narrow for this slice; the enum is
/// `#[non_exhaustive]` so richer progress and structured deliverables join
/// without a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventPayload {
    /// A bounded UTF-8 progress line.
    Progress(String),
    /// The run's terminal result submission.
    Result(String),
}

impl EventPayload {
    /// Whether the payload is within its declared per-event bound.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        let len = match self {
            EventPayload::Progress(text) | EventPayload::Result(text) => text.len(),
        };
        len <= MAX_EVENT_PAYLOAD_BYTES
    }
}

/// A batch of events the sandbox delivers on a resume, plus the state of the
/// stream after them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    /// Events strictly newer than the requested cursor, in ascending sequence.
    pub events: Vec<SandboxEvent>,
    /// Whether the sandbox had to checkpoint and stop producing because its
    /// un-acknowledged buffer overflowed. A resume that drains past the overflow
    /// clears it; this is a state the host resumes, not a terminal one.
    pub overflowed: bool,
}

impl EventBatch {
    /// The cursor the host should commit after consuming this batch: the highest
    /// sequence delivered, or the prior cursor if the batch was empty.
    #[must_use]
    pub fn next_cursor(&self, previous: EventCursor) -> EventCursor {
        self.events
            .last()
            .map_or(previous, |event| EventCursor::committed(event.sequence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_advances_cursor_to_highest_sequence() {
        let batch = EventBatch {
            events: vec![
                SandboxEvent {
                    sequence: Sequence::new(3),
                    payload: EventPayload::Progress("a".to_owned()),
                },
                SandboxEvent {
                    sequence: Sequence::new(4),
                    payload: EventPayload::Progress("b".to_owned()),
                },
            ],
            overflowed: false,
        };
        assert_eq!(
            batch.next_cursor(EventCursor::committed(Sequence::new(2))),
            EventCursor::committed(Sequence::new(4))
        );

        let empty = EventBatch {
            events: vec![],
            overflowed: false,
        };
        let previous = EventCursor::committed(Sequence::new(4));
        assert_eq!(empty.next_cursor(previous), previous);
    }

    #[test]
    fn event_roundtrips() {
        let event = SandboxEvent {
            sequence: Sequence::FIRST,
            payload: EventPayload::Result("done".to_owned()),
        };
        let encoded = serde_json::to_string(&event).unwrap();
        assert_eq!(
            serde_json::from_str::<SandboxEvent>(&encoded).unwrap(),
            event
        );
    }
}
