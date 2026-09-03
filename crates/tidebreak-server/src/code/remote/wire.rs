//! The gateway runtime wire contract, as this server speaks it.
//!
//! These types mirror the confining environment's runtime API shapes
//! (`/api/v1/runtime/...`); they are owned here rather than imported, the
//! same stance `tidebreak-supervised-agent` takes toward the control
//! endpoint (decision 0079): the environment owns the schema and this side
//! tracks it directly. Two asymmetries are deliberate:
//!
//! - Request types serialize exactly the fields the environment documents
//!   and nothing else, because the spawn and message bodies reject unknown
//!   fields.
//! - Response types default every field this server can live without, so an
//!   environment that grows a field — or an older one that lacks one —
//!   keeps working.

use serde::{Deserialize, Serialize};

/// Longest `events` wait the environment honors, in seconds. A larger
/// request is clamped server-side; staying at or under it keeps the clamp
/// out of the picture.
pub(crate) const EVENTS_MAX_WAIT_SECONDS: u32 = 25;

/// Largest message body the environment accepts, in bytes.
const MESSAGE_MAX_BODY_BYTES: usize = 32 * 1024;

/// One spawn request's arguments, exactly as the environment defines them.
///
/// Idle, wall-clock, and spend ceilings only ever narrow the profile's; the
/// environment intersects rather than trusting these values. `max_turns` is
/// a spawn-only opt-in — omission means no supervisor turn cap.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct SpawnArguments {
    /// Administrator-defined profile name on the runtime endpoint.
    pub profile: String,
    /// Harness token the sandbox drives headless (`claude_code`, `codex`,
    /// `opencode`, `grok_build`, `custom`).
    pub harness: String,
    /// Continuation policy: `goal` or `turn`. Omitted lets the environment
    /// default (`goal` for a first-party harness).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Task handed to the harness.
    pub task: String,
    /// Primary repository HTTPS URL. Absent for a research sandbox.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Branch or commit to start from. Preflight verifies the remote
    /// advertises it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_ref: Option<String>,
    /// Additional repositories beyond the primary, each with its own ref.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<SpawnRepository>,
    /// Optional connected-app subset narrowing what the sandbox may reach.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub apps: Vec<String>,
    /// Optional gateway model ID the harness is driven with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional provider-neutral reasoning effort token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Optional subscription account the sandbox's inference is pinned to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription: Option<String>,
    /// Optional idle ceiling in seconds, at most the profile's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle_timeout_seconds: Option<u32>,
    /// Optional wall-clock ceiling in seconds, at most the profile's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_clock_timeout_seconds: Option<u32>,
    /// Optional spend ceiling in micro-USD of shadow model cost.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_ceiling_microusd: Option<i64>,
    /// Optional turn budget including the spawn-task turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

/// One git repository a spawn declares.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SpawnRepository {
    /// HTTPS URL of the repository.
    pub url: String,
    /// Branch or commit to start from. Absent means the remote's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_ref: Option<String>,
}

/// What one accepted spawn produced. The spawner detaches; what it keeps is
/// the identifier and the cursor position to resume from.
#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct SandboxLease {
    /// Sandbox identifier every other call takes. Opaque here.
    pub sandbox_id: String,
    /// Lifecycle state at the moment the row committed.
    pub state: SandboxState,
    /// Highest event sequence already persisted; resume after this.
    #[serde(default)]
    pub latest_event_seq: i64,
    /// Wall-clock ceiling in seconds. A duration whose clock starts at pod
    /// start, not a deadline.
    #[serde(default)]
    pub expires_in_seconds: i64,
}

/// One sandbox's lifecycle state.
///
/// `Unknown` absorbs states a newer environment names; treating them as
/// non-terminal keeps this server polling rather than declaring an outcome
/// it cannot read.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SandboxState {
    /// Recorded and preflighted; no backend object exists yet.
    Pending,
    /// A backend object has been requested and is not yet ready.
    Provisioning,
    /// The harness is executing its task.
    Running,
    /// The task finished and results are still being delivered.
    Completing,
    /// The task finished and its results were delivered.
    Completed,
    /// The sandbox died: node loss, out-of-memory, a substrate refusal.
    Failed,
    /// A user or administrator cancelled the sandbox.
    Cancelled,
    /// The wall-clock ceiling elapsed before the task finished.
    Expired,
    /// A configured spend bound stopped the run.
    CeilingExceeded,
    /// A state this server does not know.
    #[serde(other)]
    Unknown,
}

impl SandboxState {
    /// Whether the environment will run this sandbox no further.
    #[must_use]
    pub(crate) fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Expired
                | Self::CeilingExceeded
        )
    }
}

/// Current state of one sandbox, reduced to the fields this server acts on.
#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct SandboxStatus {
    /// Sandbox identifier.
    pub sandbox_id: String,
    /// Current lifecycle state.
    pub state: SandboxState,
    /// Stable classification for a failed or expired sandbox.
    #[serde(default)]
    pub failure_reason: Option<String>,
    /// Why the sandbox was asked to stop, when something asked.
    #[serde(default)]
    pub termination_reason: Option<String>,
    /// Highest event sequence persisted, or zero for a silent sandbox.
    #[serde(default)]
    pub latest_event_seq: i64,
    /// Messages appended but not yet acknowledged as delivered.
    #[serde(default)]
    pub pending_messages: i64,
    /// Primary repository the spawn pinned, absent for a research sandbox.
    #[serde(default)]
    pub repository_url: Option<String>,
    /// Consumed inference spend in micro-USD of shadow model cost.
    #[serde(default)]
    pub spend_microusd: Option<i64>,
    /// Spend ceiling in micro-USD, resolved at spawn.
    #[serde(default)]
    pub spend_ceiling_microusd: Option<i64>,
    /// Advisory derivation that the run is busy and producing nothing.
    #[serde(default)]
    pub possibly_stalled: bool,
    /// UTC completion timestamp, present exactly for terminal states.
    #[serde(default)]
    pub completed_at: Option<String>,
}

/// Cursor into one sandbox's durable, gap-free event stream.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct EventCursor {
    /// Highest sequence already seen; omit to read from the beginning.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<i64>,
    /// Maximum events to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Hold until an event lands past `after_seq`, at most
    /// [`EVENTS_MAX_WAIT_SECONDS`]. Omit or zero for a single read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_seconds: Option<u32>,
}

/// A page of one sandbox's durable event stream.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SandboxEvents {
    /// Sandbox identifier.
    #[allow(dead_code)]
    pub sandbox_id: String,
    /// State at read time, so an event poll need not also poll status.
    pub state: SandboxState,
    /// Highest sequence persisted, which may exceed the last event returned.
    #[serde(default)]
    pub latest_event_seq: i64,
    /// Events after the requested cursor, oldest first and gap-free.
    #[serde(default)]
    pub events: Vec<SandboxEvent>,
}

/// One durable sandbox progress event.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct SandboxEvent {
    /// Per-sandbox monotonic, gap-free sequence number.
    pub seq: i64,
    /// Event kind slug.
    pub kind: String,
    /// Event payload.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// UTC event timestamp.
    #[serde(default)]
    #[allow(dead_code)]
    pub created_at: String,
}

/// One message on its way into a running sandbox's inbox.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct SandboxMessage {
    /// Ordinary input for the sandbox's next turn. The environment attaches
    /// no meaning to it.
    pub body: String,
    /// Whether to preempt the turn in flight rather than wait for it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupt: bool,
}

impl SandboxMessage {
    /// Refuses a body the environment would refuse, before the request.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.body.trim().is_empty() {
            return Err("sandbox message body is empty".to_owned());
        }
        if self.body.len() > MESSAGE_MAX_BODY_BYTES {
            return Err(format!(
                "sandbox message body is {} bytes; the ceiling is {MESSAGE_MAX_BODY_BYTES}",
                self.body.len()
            ));
        }
        Ok(())
    }
}

/// The receipt for one appended message. Durability, never delivery:
/// delivery is at-least-once and asynchronous.
#[derive(Clone, Copy, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct MessageReceipt {
    /// Per-sandbox monotonic sequence this message was recorded at.
    pub seq: i64,
    /// Whether it will preempt the turn in flight.
    #[serde(default)]
    pub interrupt: bool,
    /// Messages ahead of this one still waiting to be delivered.
    #[serde(default)]
    pub pending_messages: i64,
}

/// The environment's error body: `{"error", "error_description"}`.
#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct RuntimeErrorBody {
    /// Machine-readable refusal code.
    #[serde(default)]
    pub error: String,
    /// Human-readable description.
    #[serde(default)]
    pub error_description: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spawn body rejects unknown fields server-side, so what this side
    /// serializes must be exactly the documented shape: declared fields
    /// present, absent options omitted rather than null.
    #[test]
    fn spawn_arguments_omit_absent_fields() {
        let arguments = SpawnArguments {
            profile: "default".to_owned(),
            harness: "claude_code".to_owned(),
            task: "fix the flaky test".to_owned(),
            repository: Some("https://github.com/org/repo".to_owned()),
            ..SpawnArguments::default()
        };
        let value = serde_json::to_value(&arguments).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.keys().collect::<Vec<_>>(),
            ["harness", "profile", "repository", "task"]
                .iter()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_default_interrupt_is_omitted_from_the_message_body() {
        let message = SandboxMessage {
            body: "steer left".to_owned(),
            interrupt: false,
        };
        let value = serde_json::to_value(&message).unwrap();
        assert!(value.get("interrupt").is_none());
        let message = SandboxMessage {
            interrupt: true,
            ..message
        };
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["interrupt"], serde_json::json!(true));
    }

    /// A newer environment may name states this build does not know; they
    /// must decode as `Unknown` and read as non-terminal.
    #[test]
    fn an_unknown_state_decodes_and_is_not_terminal() {
        let state: SandboxState = serde_json::from_str("\"hibernating\"").unwrap();
        assert_eq!(state, SandboxState::Unknown);
        assert!(!state.is_terminal());
        let state: SandboxState = serde_json::from_str("\"ceiling_exceeded\"").unwrap();
        assert_eq!(state, SandboxState::CeilingExceeded);
        assert!(state.is_terminal());
    }

    #[test]
    fn a_lease_decodes_leniently() {
        let lease: SandboxLease = serde_json::from_value(serde_json::json!({
            "sandbox_id": "0d9f5a52-6a5e-4f6e-9d16-1f9a1c2b3d4e",
            "execution_id": "unused-by-this-side",
            "execution_mode": "buffered",
            "state": "pending",
            "unknown_future_field": 7,
        }))
        .unwrap();
        assert_eq!(lease.state, SandboxState::Pending);
        assert_eq!(lease.latest_event_seq, 0);
    }

    #[test]
    fn message_validation_names_the_fault() {
        let empty = SandboxMessage {
            body: "   ".to_owned(),
            interrupt: false,
        };
        assert!(empty.validate().unwrap_err().contains("empty"));
        let oversized = SandboxMessage {
            body: "x".repeat(MESSAGE_MAX_BODY_BYTES + 1),
            interrupt: false,
        };
        assert!(oversized.validate().unwrap_err().contains("ceiling"));
    }
}
