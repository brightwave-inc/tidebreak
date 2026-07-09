//! The agent event stream — the one contract every client consumes.
//!
//! A running turn emits [`AgentEvent`]s on a broadcast bus. The chat UI renders
//! from this stream plus the persisted store — the desktop forwards it to the
//! webview, the headless mode serves it over WebSocket, Slack renders coarse
//! updates, the MCP face maps it to progress notifications. Because every surface
//! reads the *same* stream, adding a client is mechanical.
//!
//! Every variant is serializable so it can cross a WebSocket to the UI. Note the
//! failure case carries [`AgentErrorInfo`] (the serializable projection of
//! [`AgentError`](crate::AgentError)), not the error type itself.

use serde::{Deserialize, Serialize};

use crate::error::AgentErrorInfo;
use crate::id::{CallId, TurnId};
use crate::provider::{StopReason, Usage};
use crate::tool::{ApprovalClass, ToolOutput};

/// One event in a turn's lifecycle, streamed to every client surface.
///
/// Serialized as an internally-tagged union (a `type` field selects the
/// variant), and `#[non_exhaustive]` so new event kinds can be added without
/// breaking downstream consumers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentEvent {
    /// A new turn has begun.
    TurnStarted {
        /// The turn being processed.
        turn_id: TurnId,
    },
    /// A chunk of assistant text.
    TextDelta {
        /// The text fragment to append.
        text: String,
    },
    /// A chunk of reasoning/thinking text, where the provider exposes it.
    ReasoningDelta {
        /// The reasoning fragment.
        text: String,
    },
    /// The current provider stream was preempted and any partial assistant/tool
    /// deltas since the last stable boundary should be discarded by clients.
    StreamInterrupted,
    /// The model has begun a tool call; its name is known.
    ToolCallStarted {
        /// Correlates the following args deltas, approval, and result.
        call_id: CallId,
        /// The tool being called.
        name: String,
    },
    /// A fragment of a tool call's JSON arguments.
    ToolCallArgsDelta {
        /// The call these args belong to.
        call_id: CallId,
        /// Partial JSON to concatenate.
        fragment: String,
    },
    /// A tool call needs explicit approval before it can run; the turn parks
    /// until an approval decision arrives.
    ApprovalRequired {
        /// The call awaiting a decision.
        call_id: CallId,
        /// The approval class that triggered the prompt.
        class: ApprovalClass,
        /// A short, human-readable summary of what will happen.
        summary: String,
    },
    /// The human decided on a parked tool call.
    ApprovalDecided {
        /// The call that was decided.
        call_id: CallId,
        /// `true` if approved, `false` if rejected.
        approved: bool,
    },
    /// A tool call finished; its result is attached.
    ToolCallCompleted {
        /// The call that completed.
        call_id: CallId,
        /// The tool's output.
        output: ToolOutput,
    },
    /// The turn finished successfully.
    TurnCompleted {
        /// Token accounting for the turn.
        usage: Usage,
        /// Why the final model call stopped.
        stop_reason: StopReason,
    },
    /// The turn failed and was abandoned.
    TurnFailed {
        /// The serializable error description.
        error: AgentErrorInfo,
    },
    /// The turn was cancelled at the client's request and stopped early. A
    /// distinct terminal outcome from `TurnCompleted`/`TurnFailed` so the UI can
    /// show "stopped" rather than success or error.
    TurnCancelled {
        /// Token accounting up to the point of cancellation.
        usage: Usage,
    },
    /// A mid-turn steer message was injected into the running turn. The turn
    /// continues (unlike `TurnCancelled`); the content is also persisted as a
    /// user message.
    UserSteered {
        /// The steered user text.
        content: String,
    },
}

/// An [`AgentEvent`] paired with its per-chat sequence number, as stored in
/// the journal.
///
/// The journal is what makes reconnect work: a client tracks the highest `seq`
/// it has seen and resumes with `after = <seq>`, so a dropped or reconnecting
/// connection catches up without gaps and without replaying what it already has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    /// Monotonic per-chat sequence number; the first event in a chat is 1.
    pub seq: i64,
    /// The event at this position in the chat's stream.
    pub event: AgentEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;

    #[test]
    fn event_is_internally_tagged() {
        let ev = AgentEvent::TextDelta { text: "hi".into() };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "text_delta");
        assert_eq!(json["text"], "hi");
    }

    #[test]
    fn turn_failed_carries_serializable_error() {
        // The whole point of the DTO: an AgentEvent built from an AgentError
        // (which is not itself Serialize) serializes and round-trips.
        let ev = AgentEvent::TurnFailed {
            error: (&AgentError::config("no provider")).into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn tool_call_completed_roundtrips() {
        let ev = AgentEvent::ToolCallCompleted {
            call_id: CallId::new(),
            output: ToolOutput::text("done"),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }
}
