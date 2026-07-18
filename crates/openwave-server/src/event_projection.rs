//! Renderer-safe projection of the internal agent journal.
//!
//! The durable [`AgentEvent`] is an internal coordination record. It can contain
//! model-generated tool arguments, tool results, host paths, reasoning, and
//! provider diagnostics. WebSocket clients receive this deliberately closed
//! projection instead.

use openwave_core::{AgentEvent, ApprovalClass, CallId, MessageId, SequencedEvent, TurnId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RendererSequencedEvent {
    pub seq: i64,
    pub event: RendererAgentEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RendererAgentEvent {
    TurnStarted {
        turn_id: TurnId,
    },
    TextDelta {
        text: String,
    },
    /// Signals progress without exposing provider reasoning.
    ReasoningDelta,
    StreamInterrupted,
    ToolCallStarted {
        call_id: CallId,
        name: RendererToolName,
    },
    /// Preserves the journal cursor and tool lifecycle without argument bytes.
    ToolCallArgsDelta {
        call_id: CallId,
    },
    ApprovalRequired {
        call_id: CallId,
        action: RendererToolName,
        class: ApprovalClass,
    },
    ApprovalDecided {
        call_id: CallId,
        approved: bool,
    },
    ToolCallCompleted {
        call_id: CallId,
        status: RendererToolStatus,
    },
    TurnCompleted,
    TurnFailed,
    TurnCancelled,
    /// The message body is available through the transcript endpoint. Keeping
    /// only its durable id lets clients reconcile without duplicating content.
    UserSteered {
        message_id: MessageId,
        text: String,
    },
    ContextTruncated,
    /// Fail-closed marker for a newer internal event until it gets an explicit
    /// renderer projection. The sequence still advances without dropping it.
    EventOmitted,
}

/// Tool names are model-controlled. Only names with fixed renderer
/// presentations cross the boundary; everything else becomes `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RendererToolName {
    Search,
    WebSearch,
    ReadFile,
    ListDir,
    WriteFile,
    RequestFolderAccess,
    ConnectFolder,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    SpawnSandboxAgent,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RendererToolStatus {
    Completed,
    Failed,
}

impl From<&str> for RendererToolName {
    fn from(name: &str) -> Self {
        match name {
            "search" => Self::Search,
            "web_search" => Self::WebSearch,
            "read_file" => Self::ReadFile,
            "list_dir" => Self::ListDir,
            "write_file" => Self::WriteFile,
            "request_folder_access" => Self::RequestFolderAccess,
            "connect_folder" => Self::ConnectFolder,
            "list_connected_folders" => Self::ListConnectedFolders,
            "list_folder" => Self::ListFolder,
            "read_connected_file" => Self::ReadConnectedFile,
            "spawn_sandbox_agent" => Self::SpawnSandboxAgent,
            _ => Self::Other,
        }
    }
}

impl RendererToolName {
    /// Whether the renderer has a fixed, server-owned description that is
    /// sufficient to present this action for an approval decision. This list
    /// is intentionally narrower than the general tool-card allowlist: adding
    /// a new Sensitive tool requires an explicit review of its approval copy.
    pub(crate) const fn is_approvable(self) -> bool {
        matches!(self, Self::Search)
    }
}

impl From<&SequencedEvent> for RendererSequencedEvent {
    fn from(value: &SequencedEvent) -> Self {
        let event = match &value.event {
            AgentEvent::TurnStarted { turn_id } => {
                RendererAgentEvent::TurnStarted { turn_id: *turn_id }
            }
            AgentEvent::TextDelta { text } => RendererAgentEvent::TextDelta { text: text.clone() },
            AgentEvent::ReasoningDelta { .. } => RendererAgentEvent::ReasoningDelta,
            AgentEvent::StreamInterrupted => RendererAgentEvent::StreamInterrupted,
            AgentEvent::ToolCallStarted { call_id, name } => RendererAgentEvent::ToolCallStarted {
                call_id: *call_id,
                name: name.as_str().into(),
            },
            AgentEvent::ToolCallArgsDelta { call_id, .. } => {
                RendererAgentEvent::ToolCallArgsDelta { call_id: *call_id }
            }
            AgentEvent::ApprovalRequired {
                call_id,
                tool_name,
                class,
                ..
            } => RendererAgentEvent::ApprovalRequired {
                call_id: *call_id,
                action: tool_name.as_str().into(),
                class: *class,
            },
            AgentEvent::ApprovalDecided { call_id, approved } => {
                RendererAgentEvent::ApprovalDecided {
                    call_id: *call_id,
                    approved: *approved,
                }
            }
            AgentEvent::ToolCallCompleted { call_id, output } => {
                RendererAgentEvent::ToolCallCompleted {
                    call_id: *call_id,
                    status: if output.is_error {
                        RendererToolStatus::Failed
                    } else {
                        RendererToolStatus::Completed
                    },
                }
            }
            AgentEvent::TurnCompleted { .. } => RendererAgentEvent::TurnCompleted,
            AgentEvent::TurnFailed { .. } => RendererAgentEvent::TurnFailed,
            AgentEvent::TurnCancelled { .. } => RendererAgentEvent::TurnCancelled,
            AgentEvent::UserSteered {
                message_id,
                content,
            } => RendererAgentEvent::UserSteered {
                message_id: *message_id,
                text: content.clone(),
            },
            AgentEvent::ContextTruncated { .. } => RendererAgentEvent::ContextTruncated,
            _ => RendererAgentEvent::EventOmitted,
        };

        Self {
            seq: value.seq,
            event,
        }
    }
}

#[cfg(test)]
mod tests {
    use openwave_core::{AgentError, ToolOutput};

    use super::*;

    #[test]
    fn projection_redacts_internal_payloads_and_unknown_tool_names() {
        let call_id = CallId::new();
        let cases = [
            AgentEvent::ReasoningDelta {
                text: "private reasoning".into(),
            },
            AgentEvent::ToolCallStarted {
                call_id,
                name: "provider_tool_with_secret".into(),
            },
            AgentEvent::ToolCallArgsDelta {
                call_id,
                fragment: r#"{\"path\":\"/Users/private\"}"#.into(),
            },
            AgentEvent::ApprovalRequired {
                call_id,
                tool_name: "provider_tool_with_secret".into(),
                class: ApprovalClass::Sensitive,
                summary: "upload /Users/private/document.txt".into(),
            },
            AgentEvent::ToolCallCompleted {
                call_id,
                output: ToolOutput::error("secret output").with_data(serde_json::json!({
                    "path": "/Users/private/document.txt",
                })),
            },
            AgentEvent::TurnFailed {
                error: (&AgentError::config("provider diagnostic")).into(),
            },
            AgentEvent::UserSteered {
                message_id: MessageId::new(),
                content: "visible steer text".into(),
            },
        ];

        let serialized = cases
            .iter()
            .enumerate()
            .map(|(index, event)| {
                serde_json::to_string(&RendererSequencedEvent::from(&SequencedEvent {
                    seq: index as i64 + 1,
                    event: event.clone(),
                }))
                .unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");

        for forbidden in [
            "private reasoning",
            "provider_tool_with_secret",
            "/Users/private",
            "document.txt",
            "secret output",
            "provider diagnostic",
            "fragment",
            "output",
            "summary",
            "content",
            "data",
            "error",
        ] {
            assert!(!serialized.contains(forbidden), "leaked {forbidden}");
        }
        assert!(serialized.contains(r#""name":"other""#));
        assert!(serialized.contains(r#""action":"other""#));
        assert!(serialized.contains(r#""status":"failed""#));
        assert!(serialized.contains(r#""text":"visible steer text""#));
    }

    #[test]
    fn steered_messages_are_projected_inline_in_sequence_order() {
        let first_id = MessageId::new();
        let second_id = MessageId::new();
        let projected = [
            SequencedEvent {
                seq: 41,
                event: AgentEvent::UserSteered {
                    message_id: first_id,
                    content: "first".into(),
                },
            },
            SequencedEvent {
                seq: 42,
                event: AgentEvent::UserSteered {
                    message_id: second_id,
                    content: "second".into(),
                },
            },
        ]
        .iter()
        .map(RendererSequencedEvent::from)
        .collect::<Vec<_>>();

        assert_eq!(
            projected,
            vec![
                RendererSequencedEvent {
                    seq: 41,
                    event: RendererAgentEvent::UserSteered {
                        message_id: first_id,
                        text: "first".into(),
                    },
                },
                RendererSequencedEvent {
                    seq: 42,
                    event: RendererAgentEvent::UserSteered {
                        message_id: second_id,
                        text: "second".into(),
                    },
                },
            ]
        );
    }
}
