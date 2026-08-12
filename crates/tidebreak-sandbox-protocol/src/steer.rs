//! Host -> sandbox steering: mid-run guidance for a live agent loop.
//!
//! A background run is otherwise fire-and-join — the host delivers a task at
//! [init](crate::init::RunInit) and then only reads events. Steering is the one
//! host-originated instruction that reaches a *running* loop: the host sends a
//! [`SteerMessage`] over the live connection and the sandbox folds its text into
//! the agent's next model step.
//!
//! # Attached-only, and deliberately not queued
//!
//! A steer is delivered over the connection the host holds; there is no durable
//! queue behind it. A host with no live connection cannot steer, and learns so
//! immediately rather than parking an instruction that may never be read. The
//! sandbox likewise buffers only a bounded number of un-consumed steers
//! ([`MAX_PENDING_STEERS`]) so a host that steers faster than the loop steps
//! cannot grow sandbox memory without bound.
//!
//! # Ordering
//!
//! Steering is carried on the reserved control lane, which the writer drains
//! ahead of the request lane, so an instruction is not stuck behind a reverse
//! response or a replay backlog. It is applied at a step boundary, never
//! mid-step: the loop consumes what has arrived when it composes its next
//! prompt, so a steer sent while a model call is in flight lands on the step
//! after it.

use serde::{Deserialize, Serialize};

/// Largest UTF-8 payload one steer message may carry.
///
/// Well under the frame bound: an instruction is a sentence or a paragraph, not
/// a document, and a host that wants to hand the sandbox a document has the
/// filesystem for it.
pub const MAX_STEER_BYTES: usize = 8 * 1024;

/// Largest number of steer messages the sandbox holds for a loop that has not
/// reached its next step boundary. Past it the oldest pending instruction is
/// dropped: the newest guidance is the guidance the user meant.
pub const MAX_PENDING_STEERS: usize = 16;

/// One mid-run instruction the host sends to a live sandbox-resident agent.
///
/// The sandbox folds [`text`](Self::text) into the agent's next model step as
/// user-authored guidance. It is host-originated and never echoed back; the
/// sandbox reports what it did with it on its own event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SteerMessage {
    /// The instruction, bounded by [`MAX_STEER_BYTES`].
    pub text: String,
}

impl SteerMessage {
    /// A steer carrying `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Whether the instruction is within its declared bound.
    ///
    /// Checked by the sender before it writes the frame and by the sandbox
    /// before it queues one: an over-bound steer is refused, not truncated,
    /// because a half-instruction is worse guidance than none.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        self.text.len() <= MAX_STEER_BYTES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_roundtrips_and_is_bounded() {
        let message = SteerMessage::new("prefer the smaller refactor");
        let encoded = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<SteerMessage>(&encoded).unwrap(),
            message
        );
        assert!(message.within_bounds());
        assert!(!SteerMessage::new("x".repeat(MAX_STEER_BYTES + 1)).within_bounds());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<SteerMessage>(r#"{"text":"hi","urgent":true}"#).is_err());
    }
}
