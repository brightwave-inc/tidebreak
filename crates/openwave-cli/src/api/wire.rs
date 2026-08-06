//! Client-side decode of the chat event socket's frames.
//!
//! The server sends `RendererChatFrame`s — the closed, renderer-safe projection
//! of the internal journal (see `openwave_server::event_projection`). These
//! types mirror that wire shape loosely on purpose: closed vocabularies the
//! server may grow (tool names, approval kinds, preview variants) decode as
//! plain strings or JSON here, and an unrecognized event type decodes as
//! [`ClientEvent::Unknown`] instead of failing the whole stream.

use openwave_core::{CallId, TurnId};
use serde::Deserialize;

/// One frame on the socket: a journaled event (carrying `seq`, the resume
/// cursor) or out-of-band metadata (no sequence).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ChatFrame {
    Event(SequencedFrame),
    Metadata(MetadataFrame),
}

/// A journaled event at its sequence.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SequencedFrame {
    pub seq: i64,
    pub event: ClientEvent,
}

/// A metadata push; only the tag is read today (the TUI has no title surface).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetadataFrame {
    pub metadata: String,
}

/// The events the CLI renders. Only fields a CLI surface uses are declared;
/// serde drops the rest, so a server-side field addition never breaks decoding.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientEvent {
    TurnStarted {
        turn_id: TurnId,
    },
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    StreamInterrupted,
    ToolCallStarted {
        call_id: CallId,
        name: String,
    },
    ToolCallArgsDelta,
    UserQuestionsAsked,
    PlanProposed,
    ApprovalRequired {
        call_id: CallId,
        action: String,
        approval: String,
        #[serde(default)]
        auto_judging: bool,
        #[serde(default)]
        preview: Option<serde_json::Value>,
    },
    ApprovalDecided {
        call_id: CallId,
    },
    ToolCallCompleted {
        call_id: CallId,
        status: ToolCallStatus,
    },
    TurnCompleted,
    TurnRefused {
        refusal: Refusal,
    },
    TurnFailed {
        category: String,
    },
    TurnCancelled,
    UserSteered {
        text: String,
    },
    ContextTruncated {
        original_tokens: u32,
        fitted_tokens: u32,
    },
    /// A newer server's event this build does not know; the journal cursor
    /// still advances past it.
    #[serde(other)]
    Unknown,
}

/// Whether a finished tool call succeeded. Unknown statuses decode rather than
/// failing; they render as failures (the conservative display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

/// Bounded refusal metadata, as much as a client can act on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Refusal {
    #[serde(default)]
    pub category: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(json: &str) -> ChatFrame {
        serde_json::from_str(json).expect("the frame decodes")
    }

    /// Representative frames exactly as the server serializes them (untagged
    /// frame union, internally-tagged snake_case events). This is the contract
    /// across the wire boundary: a rename or tag change server-side fails here.
    #[test]
    fn representative_frames_decode() {
        let metadata = frame(r#"{"metadata":"titled","title":"A chat"}"#);
        assert_eq!(
            metadata,
            ChatFrame::Metadata(MetadataFrame {
                metadata: "titled".into()
            })
        );

        let turn_started = frame(
            r#"{"seq":1,"event":{"type":"turn_started","turn_id":"00000000-0000-0000-0000-000000000002"}}"#,
        );
        let ChatFrame::Event(turn_started) = turn_started else {
            panic!("expected an event frame: {turn_started:?}");
        };
        assert_eq!(turn_started.seq, 1);
        assert!(matches!(
            turn_started.event,
            ClientEvent::TurnStarted { .. }
        ));

        let text = frame(r#"{"seq":2,"event":{"type":"text_delta","text":"Hello"}}"#);
        assert_eq!(
            text,
            ChatFrame::Event(SequencedFrame {
                seq: 2,
                event: ClientEvent::TextDelta {
                    text: "Hello".into()
                }
            })
        );

        let tool = frame(
            r#"{"seq":3,"event":{"type":"tool_call_started","call_id":"00000000-0000-0000-0000-000000000003","name":"exec"}}"#,
        );
        let ChatFrame::Event(tool) = tool else {
            panic!("expected an event frame: {tool:?}");
        };
        let ClientEvent::ToolCallStarted { call_id, name } = &tool.event else {
            panic!("expected tool_call_started: {tool:?}");
        };
        assert_eq!(name, "exec");
        let started_call = *call_id;

        // ApprovalRequired carries the one deliberate payload opening: the
        // tool's closed preview of the action under review.
        let approval = frame(&format!(
            r#"{{"seq":4,"event":{{"type":"approval_required","call_id":"{started_call}","action":"exec","approval":"exec_may_run_networked_command","class":"sensitive","preview":{{"tool":"exec","command":"git","args":["status"],"cwd":".","files":[]}}}}}}"#
        ));
        let ChatFrame::Event(approval) = approval else {
            panic!("expected an event frame: {approval:?}");
        };
        let ClientEvent::ApprovalRequired {
            call_id,
            action,
            approval: kind,
            auto_judging,
            preview,
        } = &approval.event
        else {
            panic!("expected approval_required: {approval:?}");
        };
        assert_eq!(*call_id, started_call);
        assert_eq!(action, "exec");
        assert_eq!(kind, "exec_may_run_networked_command");
        assert!(!auto_judging);
        assert_eq!(
            preview.as_ref().and_then(|p| p.get("tool")),
            Some(&serde_json::json!("exec"))
        );

        let completed = frame(&format!(
            r#"{{"seq":5,"event":{{"type":"tool_call_completed","call_id":"{started_call}","status":"completed"}}}}"#
        ));
        assert_eq!(
            completed,
            ChatFrame::Event(SequencedFrame {
                seq: 5,
                event: ClientEvent::ToolCallCompleted {
                    call_id: started_call,
                    status: ToolCallStatus::Completed,
                }
            })
        );

        // TurnCompleted's usage is present on the wire but unread by the TUI.
        let done = frame(
            r#"{"seq":6,"event":{"type":"turn_completed","usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        );
        assert_eq!(
            done,
            ChatFrame::Event(SequencedFrame {
                seq: 6,
                event: ClientEvent::TurnCompleted,
            })
        );
    }

    /// Forward compatibility: an event type this build doesn't know decodes as
    /// `Unknown` (the cursor advances, the stream survives), and the
    /// projection's own fail-closed `event_omitted` marker lands there too.
    #[test]
    fn unrecognized_event_types_decode_as_unknown() {
        let future = frame(r#"{"seq":9,"event":{"type":"some_future_event","payload":{}}}"#);
        assert_eq!(
            future,
            ChatFrame::Event(SequencedFrame {
                seq: 9,
                event: ClientEvent::Unknown,
            })
        );

        let omitted = frame(r#"{"seq":10,"event":{"type":"event_omitted"}}"#);
        assert_eq!(
            omitted,
            ChatFrame::Event(SequencedFrame {
                seq: 10,
                event: ClientEvent::Unknown,
            })
        );
    }
}
