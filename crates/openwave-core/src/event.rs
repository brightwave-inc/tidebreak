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
use crate::id::{CallId, MessageId, TurnId};
use crate::provider::{RefusalOutcome, StopReason, Usage};
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
    /// A validated foreground question continuation committed and released its
    /// worker. The event is intentionally only a bounded refresh hint; clients
    /// load presentation data from the renderer-safe pending-question route.
    UserQuestionsAsked {
        /// Exact tool call awaiting answers.
        call_id: CallId,
        /// Turn that will resume after the exact answer commits.
        turn_id: TurnId,
    },
    /// A tool call needs explicit approval before it can run; the turn parks
    /// until an approval decision arrives.
    ApprovalRequired {
        /// Whether the Auto-mode judge owns this card right now.
        ///
        /// Absent both in journals written before the judge existed and on
        /// every ordinary card since — the row is written only when a judge
        /// actually owns it, so the stored shape of a human-decided approval
        /// is unchanged and old rows read back identically.
        #[serde(default, skip_serializing_if = "is_false")]
        auto_judging: bool,
        /// The call awaiting a decision.
        call_id: CallId,
        /// Canonical registered tool identity. Renderer boundaries must map
        /// this through a closed allowlist rather than serializing it directly.
        tool_name: String,
        /// The approval class that triggered the prompt.
        class: ApprovalClass,
        kind: crate::approval::ToolApprovalKind,
        /// The complete standing-grant ladder this exact call cleared.
        ///
        /// Empty means approving once is the only affirmative choice. The
        /// renderer must not reconstruct broader rungs from the approval kind:
        /// command policy can refuse every standing grant for an interpreter
        /// while still allowing one explicit human approval.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grant_scopes: Vec<crate::approval::GrantScope>,
        /// Closed projection of what the call will do, when its tool has one.
        /// Absent on journal rows written before previews existed.
        #[serde(default)]
        preview: Option<crate::preview::ToolActionPreview>,
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
        /// Closed projection of what the call did, when its tool has one.
        /// Absent on journal rows written before previews existed.
        #[serde(default)]
        action: Option<crate::preview::ToolActionPreview>,
        /// Closed projection of what the call produced, when its tool has one.
        #[serde(default)]
        result: Option<crate::preview::ToolResultPreview>,
    },
    /// The turn finished successfully.
    TurnCompleted {
        /// Token accounting for the turn.
        usage: Usage,
        /// Why the final model call stopped.
        stop_reason: StopReason,
    },
    /// The turn completed with a model refusal rather than a complete answer.
    TurnRefused {
        /// Token accounting up to the refusal.
        usage: Usage,
        /// Category detail and whether visible output is incomplete.
        refusal: RefusalOutcome,
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
    /// user message. `message_id` lets reconnecting renderers reconcile the
    /// event with that durable message without lossy content matching.
    UserSteered {
        /// Stable id of the persisted user message.
        message_id: MessageId,
        /// The steered user text.
        content: String,
    },
    /// The transcript was truncated to fit the model's context window before a
    /// model call. Informational — the turn continues with reduced context.
    ContextTruncated {
        /// Estimated tokens of the full transcript before reduction.
        original_tokens: u32,
        /// Estimated tokens after fitting to the budget.
        fitted_tokens: u32,
    },
    /// A validated plan-mode continuation committed and released its worker.
    /// Like [`Self::UserQuestionsAsked`], this is only a bounded refresh hint;
    /// clients load the plan from the renderer-safe pending-plan route.
    PlanProposed {
        /// Exact tool call awaiting the reader's decision.
        call_id: CallId,
        /// Turn that will resume after the decision commits.
        turn_id: TurnId,
    },
}

/// Whether to omit a defaulted `false` flag from a journal row.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
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
            action: None,
            result: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<AgentEvent>(&json).unwrap(), ev);
    }

    #[test]
    fn user_steer_roundtrips_with_its_durable_message_id() {
        let message_id = MessageId::new();
        let ev = AgentEvent::UserSteered {
            message_id,
            content: "remember this".into(),
        };

        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["type"], "user_steered");
        assert_eq!(json["message_id"], message_id.to_string());
        assert_eq!(serde_json::from_value::<AgentEvent>(json).unwrap(), ev);
    }

    /// Path of the checked-in journal fixture, relative to this crate.
    const JOURNAL_FIXTURE: &str = "fixtures/journal-events.json";

    /// Fixed ids, because a fixture with a fresh UUID in it would differ on every
    /// run and the comparison below would be meaningless.
    fn id(n: u128) -> uuid::Uuid {
        uuid::Uuid::from_u128(n)
    }

    /// Declaration index of a variant.
    ///
    /// The enum is `#[non_exhaustive]`, but that only binds other crates — the
    /// match here is exhaustive, so a new variant does not compile until it is
    /// added, and [`journal_samples`] is then checked for covering it.
    fn variant_index(event: &AgentEvent) -> usize {
        match event {
            AgentEvent::TurnStarted { .. } => 0,
            AgentEvent::TextDelta { .. } => 1,
            AgentEvent::ReasoningDelta { .. } => 2,
            AgentEvent::StreamInterrupted => 3,
            AgentEvent::ToolCallStarted { .. } => 4,
            AgentEvent::ToolCallArgsDelta { .. } => 5,
            AgentEvent::UserQuestionsAsked { .. } => 6,
            AgentEvent::ApprovalRequired { .. } => 7,
            AgentEvent::ApprovalDecided { .. } => 8,
            AgentEvent::ToolCallCompleted { .. } => 9,
            AgentEvent::TurnCompleted { .. } => 10,
            AgentEvent::TurnRefused { .. } => 11,
            AgentEvent::TurnFailed { .. } => 12,
            AgentEvent::TurnCancelled { .. } => 13,
            AgentEvent::UserSteered { .. } => 14,
            AgentEvent::ContextTruncated { .. } => 15,
            AgentEvent::PlanProposed { .. } => 16,
        }
    }

    /// One journal row per variant.
    ///
    /// Optional fields are populated rather than left `None`: a `None` field is
    /// skipped or written as null and would pin nothing, so the nested storage
    /// types — `ToolActionPreview`, `ToolResultPreview`, `ToolOutput` — would go
    /// unguarded exactly where the risk is.
    fn journal_samples() -> Vec<AgentEvent> {
        vec![
            AgentEvent::TurnStarted {
                turn_id: TurnId(id(1)),
            },
            AgentEvent::TextDelta {
                text: "assistant text".into(),
            },
            AgentEvent::ReasoningDelta {
                text: "reasoning text".into(),
            },
            AgentEvent::StreamInterrupted,
            AgentEvent::ToolCallStarted {
                call_id: CallId(id(2)),
                name: "exec".into(),
            },
            AgentEvent::ToolCallArgsDelta {
                call_id: CallId(id(2)),
                fragment: "{\"command\":".into(),
            },
            AgentEvent::UserQuestionsAsked {
                call_id: CallId(id(3)),
                turn_id: TurnId(id(1)),
            },
            AgentEvent::ApprovalRequired {
                auto_judging: false,
                call_id: CallId(id(2)),
                tool_name: "exec".into(),
                class: ApprovalClass::Sensitive,
                kind: crate::approval::ToolApprovalKind::ExecMayRunNetworkedCommand,
                grant_scopes: Vec::new(),
                preview: Some(crate::preview::ToolActionPreview::Exec {
                    command: "git".into(),
                    args: vec!["status".into()],
                    cwd: ".".into(),
                    files: vec!["documents/report.pdf".into()],
                }),
            },
            AgentEvent::ApprovalDecided {
                call_id: CallId(id(2)),
                approved: true,
            },
            AgentEvent::ToolCallCompleted {
                call_id: CallId(id(2)),
                output: ToolOutput {
                    content: "on branch main".into(),
                    data: Some(serde_json::json!({"exit_code": 0})),
                    is_error: false,
                    error_category: None,
                    // Populated to pin the field and `ToolUiView`'s shape; a
                    // real exec output never carries a view. The `McpApp`
                    // result variant is pinned by the server wire fixtures.
                    ui_view: Some(Box::new(crate::tool::ToolUiView {
                        server: "gateway".into(),
                        resource_uri: "ui://gateway/app.html".into(),
                    })),
                    images: Vec::new(),
                    image_data: crate::ImageAttachments::new(),
                },
                action: Some(crate::preview::ToolActionPreview::Exec {
                    command: "git".into(),
                    args: vec!["status".into()],
                    cwd: ".".into(),
                    files: vec!["documents/report.pdf".into()],
                }),
                result: Some(crate::preview::ToolResultPreview::Exec {
                    exit_code: Some(0),
                    timed_out: false,
                    output_truncated: false,
                    stdout: "on branch main".into(),
                    stderr: String::new(),
                    images: Vec::new(),
                    outputs: Vec::new(),
                    degraded: None,
                }),
            },
            AgentEvent::TurnCompleted {
                usage: Usage {
                    input_tokens: 11,
                    output_tokens: 22,
                    cache_read_input_tokens: 33,
                    cache_creation_input_tokens: 44,
                },
                stop_reason: StopReason::EndTurn,
            },
            AgentEvent::TurnRefused {
                usage: Usage {
                    input_tokens: 12,
                    output_tokens: 3,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
                refusal: RefusalOutcome::new(
                    crate::provider::RefusalDetails::from_category(Some("cyber")),
                    true,
                ),
            },
            AgentEvent::TurnFailed {
                error: (&AgentError::config("no provider")).into(),
            },
            AgentEvent::TurnCancelled {
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 6,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            },
            AgentEvent::UserSteered {
                message_id: MessageId(id(4)),
                content: "steered text".into(),
            },
            AgentEvent::ContextTruncated {
                original_tokens: 100_000,
                fitted_tokens: 60_000,
            },
            AgentEvent::PlanProposed {
                call_id: CallId(id(5)),
                turn_id: TurnId(id(1)),
            },
        ]
    }

    /// Every variant has a sample, so the fixture cannot silently cover fewer
    /// event kinds than the journal can contain.
    #[test]
    fn every_journal_event_kind_has_a_sample() {
        let samples = journal_samples();
        let covered: std::collections::BTreeSet<usize> =
            samples.iter().map(variant_index).collect();
        assert_eq!(
            covered.len(),
            samples.len(),
            "two samples describe the same variant"
        );
        assert_eq!(
            covered,
            (0..samples.len()).collect(),
            "a variant is missing from journal_samples"
        );
    }

    /// `AgentEvent` is written into `event.payload` and read back, so its field
    /// names are a storage format, not only a wire format. Nothing else pins
    /// them: the desktop schema epoch covers SQLite migrations, and a rename
    /// changes no table. `list_events` collects into `Result<Vec<_>>`, so one
    /// unreadable row fails a whole chat's history rather than one message.
    ///
    /// Regenerate with `UPDATE_JOURNAL_FIXTURE=1 cargo test -p openwave-core`.
    /// A diff here means the journal format changed, which needs a
    /// `#[serde(alias)]` or a migration — not a refreshed fixture.
    #[test]
    fn the_journal_event_shape_is_pinned() {
        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&journal_samples()).expect("events serialize")
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNAL_FIXTURE);
        if std::env::var_os("UPDATE_JOURNAL_FIXTURE").is_some() {
            std::fs::create_dir_all(path.parent().expect("the fixture path has a parent"))
                .expect("the fixture directory is creatable");
            std::fs::write(&path, &rendered).expect("the fixture path is writable");
            return;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            existing, rendered,
            "the journal event shape changed; existing chats may no longer load. \
             If this is deliberate, add #[serde(alias)] or a migration, then \
             regenerate with UPDATE_JOURNAL_FIXTURE=1 cargo test -p openwave-core"
        );
    }

    /// The direct statement of the compatibility property: bytes that a previous
    /// binary wrote still parse, and parse back to the same value.
    ///
    /// Separate from the comparison above because it fails for a different
    /// reason. A rename shows up in both, but a field that stops being optional
    /// only shows up here.
    #[test]
    fn journal_rows_written_before_this_build_still_load() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(JOURNAL_FIXTURE);
        let stored = std::fs::read_to_string(&path).expect("the journal fixture is checked in");
        let loaded: Vec<AgentEvent> =
            serde_json::from_str(&stored).expect("stored journal rows still deserialize");
        assert_eq!(
            loaded,
            journal_samples(),
            "a stored journal row no longer round-trips to the value it was written from"
        );
    }

    /// `ApprovalRequired` rows written before the free-text `summary` field was
    /// retired still carry it in `event.payload`; the extra key must stay
    /// ignorable, or one such row makes a whole chat's history unreadable.
    #[test]
    fn approval_rows_with_the_retired_summary_field_still_load() {
        let legacy = serde_json::json!({
            "type": "approval_required",
            "call_id": id(2),
            "tool_name": "exec",
            "class": "sensitive",
            "kind": "exec_may_run_networked_command",
            "summary": "exec requires approval",
        });
        let loaded: AgentEvent = serde_json::from_value(legacy).expect("legacy row deserializes");
        assert_eq!(
            loaded,
            AgentEvent::ApprovalRequired {
                auto_judging: false,
                call_id: CallId(id(2)),
                tool_name: "exec".into(),
                class: ApprovalClass::Sensitive,
                kind: crate::approval::ToolApprovalKind::ExecMayRunNetworkedCommand,
                grant_scopes: Vec::new(),
                preview: None,
            }
        );
    }

    /// Exec previews written before the staging list joined the projection have
    /// no `files` key. They must read back as "staged nothing" — the narrow
    /// reading — rather than failing the row and taking a chat's history with
    /// it.
    #[test]
    fn exec_previews_written_without_a_staging_list_still_load() {
        let legacy = serde_json::json!({
            "type": "approval_required",
            "call_id": id(2),
            "tool_name": "exec",
            "class": "sensitive",
            "kind": "exec_may_run_networked_command",
            "preview": { "tool": "exec", "command": "git", "args": ["status"], "cwd": "." },
        });
        let loaded: AgentEvent = serde_json::from_value(legacy).expect("legacy row deserializes");
        let AgentEvent::ApprovalRequired {
            preview: Some(crate::preview::ToolActionPreview::Exec { files, .. }),
            ..
        } = loaded
        else {
            panic!("the row is an exec approval with a preview");
        };
        assert!(files.is_empty());
    }
}
