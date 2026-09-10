//! The supervisor poll wire contract, as this agent speaks it.
//!
//! These types mirror the control endpoint's published shapes; they are owned
//! here rather than imported so the agent tracks the endpoint's API directly
//! (decision 0077 — no protocol crate sits between). Two asymmetries are
//! deliberate:
//!
//! - The request types serialize exactly the fields the endpoint documents
//!   and nothing else, because the endpoint rejects unknown request fields.
//! - The response types default every field the agent can live without, so an
//!   endpoint that grows a field — or an older one that lacks one — keeps
//!   working. The endpoint owns the schema; this side stays lenient.

use serde::{Deserialize, Serialize};

/// Poll schema version this agent speaks. The endpoint accepts its current
/// release and the one before it, so this pins the agent to a known dialect
/// rather than silently drifting.
pub const SUPERVISOR_POLL_SCHEMA_VERSION: u32 = 1;

/// One turn of the poll loop, as the agent reports it.
#[derive(Clone, Debug, Serialize)]
pub struct SupervisorPoll {
    /// Always [`SUPERVISOR_POLL_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Probed identity for a managed supervised engine, repeated on every poll.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded_engine: Option<EmbeddedEngineRegistration>,
    /// Whether the engine is idle rather than mid-turn. Process state, not
    /// judgement: no engine turn is running.
    pub idle: bool,
    /// Highest inbox sequence this agent has delivered to the engine.
    ///
    /// Sent *after* delivery. An agent that dies before sending it redelivers
    /// on restart, which is the at-least-once guarantee; one that sent it
    /// first would silently skip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_through_seq: Option<i64>,
    /// Lifecycle and progress events to append to the sandbox's stream.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SupervisorEvent>,
}

impl SupervisorPoll {
    /// A poll carrying the current delivery cursor and nothing else.
    #[must_use]
    pub fn new(idle: bool, delivered_through_seq: Option<i64>) -> Self {
        Self {
            schema_version: SUPERVISOR_POLL_SCHEMA_VERSION,
            embedded_engine: None,
            idle,
            delivered_through_seq,
            events: Vec::new(),
        }
    }
}

/// Identity Gateway injects from the authenticated spawn request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddedEngine {
    pub engine_session_id: tidebreak_core::SessionId,
    pub engine: tidebreak_core::HarnessKind,
}

/// The installed version the agent registers before using the sandbox.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddedEngineRegistration {
    pub engine_session_id: tidebreak_core::SessionId,
    pub engine: tidebreak_core::HarnessKind,
    pub engine_version: String,
}

/// One event the agent reports about itself.
///
/// The endpoint constrains `kind` to a lowercase slug of at most 63
/// characters and bounds the payload; [`crate::control::Outbox`] keeps each
/// poll's batch under that bound.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SupervisorEvent {
    /// Slug-shaped event kind.
    pub kind: String,
    /// Event payload.
    pub payload: serde_json::Value,
}

impl SupervisorEvent {
    /// Builds one event.
    #[must_use]
    pub fn new(kind: &str, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.to_owned(),
            payload,
        }
    }
}

/// What the endpoint tells the agent in reply.
///
/// Every field beyond the message stream is advisory here: the environment
/// ends the sandbox regardless of whether the agent honors `stop`, so acting
/// on these promptly is tidiness, not correctness.
#[derive(Clone, Debug, Deserialize)]
pub struct SupervisorInstructions {
    /// Gateway must echo the exact registered identity on a managed poll.
    #[serde(default)]
    pub embedded_engine: Option<EmbeddedEngineRegistration>,
    /// Whether the agent should stop the engine and exit.
    #[serde(default)]
    pub stop: bool,
    /// Why it was asked to stop, when something asked.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Highest inbox sequence the endpoint has recorded as delivered.
    #[serde(default)]
    pub cursor: i64,
    /// Undelivered messages after the cursor, oldest first and gap-free.
    #[serde(default)]
    pub messages: Vec<SupervisorMessage>,
    /// Whether this sandbox has shipped a pull request this run.
    ///
    /// Computed by the environment from traffic it observed directly; the
    /// agent never infers this itself.
    #[serde(default)]
    pub acceptance_met: bool,
}

/// One steering message on its way to the engine.
#[derive(Clone, Debug, Deserialize)]
pub struct SupervisorMessage {
    /// Per-sandbox monotonic, gap-free sequence number.
    pub seq: i64,
    /// Ordinary input or a protected server-tool result.
    pub body: SupervisorMessageBody,
    /// Whether the turn in flight should be preempted to deliver it.
    #[serde(default)]
    pub interrupt: bool,
}

/// A typed message body over the endpoint's text transport.
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SupervisorMessageBody {
    /// Ordinary input for the engine.
    Input(String),
    /// A protected native-tool result from the host.
    Tool(SupervisorToolResult),
}

/// One protected native-tool request the sandbox asks the host to execute.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorToolRequest {
    /// Stable request id, repeated on agent retries.
    pub request_id: String,
    /// Allowlisted registered server-side tool name.
    pub tool: String,
    /// Tool arguments validated by the host's authoritative registry.
    pub arguments: serde_json::Value,
}

/// One native-tool result returned by the host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorToolResult {
    /// The request id this result answers.
    pub request_id: String,
    /// The serialized tool output (or a pending-request continuation).
    pub output: serde_json::Value,
    /// Bounded artifact files to materialize into the sandbox scratch.
    #[serde(default)]
    pub artifacts: Vec<SupervisorArtifact>,
}

/// One bounded artifact materialized into sandbox scratch by the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SupervisorArtifact {
    /// Safe private-scratch-relative path, generated by the host.
    pub path: String,
    /// MIME media type.
    pub media_type: String,
    /// File bytes; bounded by the host (text/JSONL up to 2 MiB, images up to 2 MiB).
    pub bytes: Vec<u8>,
}

/// The error body the endpoint returns on a refused poll.
#[derive(Clone, Debug, Deserialize)]
pub struct PollRejection {
    /// Machine-readable refusal code.
    pub error: String,
    /// Human-readable description.
    #[serde(default)]
    pub error_description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The endpoint rejects unknown request fields, so the serialized poll
    /// must carry exactly the documented ones — and omit the optional ones
    /// rather than send nulls an older endpoint may refuse.
    #[test]
    fn poll_serializes_only_documented_fields() {
        let poll = SupervisorPoll::new(true, None);
        let value = serde_json::to_value(&poll).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            ["idle", "schema_version"],
            "empty optionals must be omitted, not nulled"
        );

        let mut poll = SupervisorPoll::new(false, Some(7));
        poll.events
            .push(SupervisorEvent::new("turn_started", serde_json::json!({})));
        let value = serde_json::to_value(&poll).unwrap();
        let object = value.as_object().unwrap();
        let mut keys = object.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            ["delivered_through_seq", "events", "idle", "schema_version"]
        );
    }

    /// The agent must keep working against an endpoint that adds response
    /// fields, and against one that omits fields this release knows about.
    #[test]
    fn instructions_tolerate_unknown_and_missing_fields() {
        let full: SupervisorInstructions = serde_json::from_value(serde_json::json!({
            "sandbox_id": "018f0000-0000-7000-8000-000000000000",
            "state": "running",
            "stop": false,
            "cursor": 3,
            "messages": [{"seq": 4, "body": {"request_id":"r1","output":{},"artifacts":[]}, "interrupt": false, "created_at": "2026-08-27T00:00:00Z"}],
            "acceptance_met": true,
            "wrap_up_due": [{"ceiling": "spend"}],
            "spend_microusd": 12,
            "write_class_requests": 1,
            "cost_limit_exhausted": false,
            "retry_after_seconds": 5
        }))
        .unwrap();
        assert!(full.acceptance_met);
        assert_eq!(full.messages.len(), 1);
        assert_eq!(full.messages[0].seq, 4);

        let sparse: SupervisorInstructions =
            serde_json::from_value(serde_json::json!({"cursor": 0})).unwrap();
        assert!(!sparse.stop);
        assert!(sparse.messages.is_empty());
    }
}
