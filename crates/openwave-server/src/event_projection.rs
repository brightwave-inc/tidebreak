//! Renderer-safe projection of the internal agent journal.
//!
//! The durable [`AgentEvent`] is an internal coordination record. It can contain
//! model-generated tool arguments, tool results, host paths, reasoning, and
//! provider diagnostics. WebSocket clients receive this deliberately closed
//! projection instead.

use openwave_core::{
    AgentEvent, ApprovalClass, CallId, MessageId, SequencedEvent, ToolActionPreview,
    ToolApprovalKind, ToolResultPreview, TurnId,
};
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
    UserQuestionsAsked {
        call_id: CallId,
        turn_id: TurnId,
    },
    ApprovalRequired {
        call_id: CallId,
        action: RendererToolName,
        approval: ToolApprovalKind,
        class: ApprovalClass,
        /// The one deliberate opening in this boundary. A human cannot consent
        /// to a command they are not shown, so a tool may project a closed,
        /// field-by-field view of the action under review. Tools without one
        /// send nothing, as every tool did before.
        #[serde(skip_serializing_if = "Option::is_none")]
        preview: Option<ToolActionPreview>,
    },
    ApprovalDecided {
        call_id: CallId,
        approved: bool,
    },
    ToolCallCompleted {
        call_id: CallId,
        status: RendererToolStatus,
        /// What the call did, when its tool projects it. Approval is not the
        /// only moment a person needs to see the action.
        #[serde(skip_serializing_if = "Option::is_none")]
        action: Option<ToolActionPreview>,
        /// What the call produced. A command's output is the reason it ran;
        /// withholding it leaves the transcript asserting that something
        /// happened without ever showing what.
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<ToolResultPreview>,
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
    ListSources,
    ReadSource,
    ReadToolResult,
    WebSearch,
    ReadDelegatedFile,
    ReadFile,
    ListDir,
    WriteFile,
    CreateDeliverable,
    RequestFolderAccess,
    ConnectFolder,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    ImportConnectedFile,
    SpawnSandboxAgent,
    WaitForAgents,
    AskUserQuestions,
    Exec,
    Other,
}

/// The renderer contract lives entirely in the test below, so these exist for
/// it rather than for the running server.
#[cfg(test)]
impl RendererToolName {
    /// Every name, in declaration order.
    ///
    /// This is the renderer contract: the desktop maintains a matching union, a
    /// runtime guard, a copy table, and an icon table by hand, and nothing
    /// linked them. Writing the list down lets both sides be checked against
    /// one source instead of against each other's memory.
    ///
    /// Kept complete by [`Self::position`]: a new variant fails to compile
    /// there, and the test below fails if it is missing here.
    pub(crate) const ALL: &'static [Self] = &[
        Self::Search,
        Self::ListSources,
        Self::ReadSource,
        Self::ReadToolResult,
        Self::WebSearch,
        Self::ReadDelegatedFile,
        Self::ReadFile,
        Self::ListDir,
        Self::WriteFile,
        Self::CreateDeliverable,
        Self::RequestFolderAccess,
        Self::ConnectFolder,
        Self::ListConnectedFolders,
        Self::ListFolder,
        Self::ReadConnectedFile,
        Self::ImportConnectedFile,
        Self::SpawnSandboxAgent,
        Self::WaitForAgents,
        Self::AskUserQuestions,
        Self::Exec,
        Self::Other,
    ];

    /// Declaration index. Exists to make [`Self::ALL`] impossible to forget:
    /// adding a variant without a match arm here does not compile.
    const fn position(self) -> usize {
        match self {
            Self::Search => 0,
            Self::ListSources => 1,
            Self::ReadSource => 2,
            Self::ReadToolResult => 3,
            Self::WebSearch => 4,
            Self::ReadDelegatedFile => 5,
            Self::ReadFile => 6,
            Self::ListDir => 7,
            Self::WriteFile => 8,
            Self::CreateDeliverable => 9,
            Self::RequestFolderAccess => 10,
            Self::ConnectFolder => 11,
            Self::ListConnectedFolders => 12,
            Self::ListFolder => 13,
            Self::ReadConnectedFile => 14,
            Self::ImportConnectedFile => 15,
            Self::SpawnSandboxAgent => 16,
            Self::WaitForAgents => 17,
            Self::AskUserQuestions => 18,
            Self::Exec => 19,
            Self::Other => 20,
        }
    }

    /// The wire spelling, which is what the renderer actually matches on.
    fn wire_name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .expect("a renderer tool name always serializes as a string")
    }
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
            crate::source_tools::LIST_SOURCES_TOOL => Self::ListSources,
            crate::source_tools::READ_SOURCE_TOOL => Self::ReadSource,
            crate::source_tools::READ_TOOL_RESULT_TOOL => Self::ReadToolResult,
            "web_search" => Self::WebSearch,
            openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => Self::ReadDelegatedFile,
            "read_file" => Self::ReadFile,
            "list_dir" => Self::ListDir,
            "write_file" => Self::WriteFile,
            "create_deliverable" => Self::CreateDeliverable,
            "request_folder_access" => Self::RequestFolderAccess,
            "connect_folder" => Self::ConnectFolder,
            "list_connected_folders" => Self::ListConnectedFolders,
            "list_folder" => Self::ListFolder,
            "read_connected_file" => Self::ReadConnectedFile,
            "import_connected_file" => Self::ImportConnectedFile,
            "spawn_sandbox_agent" => Self::SpawnSandboxAgent,
            "wait_for_agents" => Self::WaitForAgents,
            openwave_core::ASK_USER_QUESTIONS_TOOL => Self::AskUserQuestions,
            "exec" => Self::Exec,
            _ => Self::Other,
        }
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
            AgentEvent::UserQuestionsAsked { call_id, turn_id } => {
                RendererAgentEvent::UserQuestionsAsked {
                    call_id: *call_id,
                    turn_id: *turn_id,
                }
            }
            AgentEvent::ApprovalRequired {
                call_id,
                tool_name,
                class,
                kind,
                preview,
                ..
            } => RendererAgentEvent::ApprovalRequired {
                call_id: *call_id,
                action: tool_name.as_str().into(),
                approval: *kind,
                class: *class,
                preview: preview.clone(),
            },
            AgentEvent::ApprovalDecided { call_id, approved } => {
                RendererAgentEvent::ApprovalDecided {
                    call_id: *call_id,
                    approved: *approved,
                }
            }
            AgentEvent::ToolCallCompleted {
                call_id,
                output,
                action,
                result,
            } => RendererAgentEvent::ToolCallCompleted {
                call_id: *call_id,
                status: if output.is_error {
                    RendererToolStatus::Failed
                } else {
                    RendererToolStatus::Completed
                },
                action: action.clone(),
                result: result.clone(),
            },
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
                kind: ToolApprovalKind::Unsupported,
                preview: None,
                summary: "upload /Users/private/document.txt".into(),
            },
            AgentEvent::ToolCallCompleted {
                call_id,
                output: ToolOutput::error("secret output").with_data(serde_json::json!({
                    "path": "/Users/private/document.txt",
                })),
                action: None,
                result: None,
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
    fn question_event_is_only_a_bounded_refresh_hint() {
        let call_id = CallId::new();
        let turn_id = TurnId::new();
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 7,
            event: AgentEvent::UserQuestionsAsked { call_id, turn_id },
        });
        assert_eq!(
            projected.event,
            RendererAgentEvent::UserQuestionsAsked { call_id, turn_id }
        );
        let encoded = serde_json::to_value(projected).unwrap();
        assert_eq!(encoded["event"]["type"], "user_questions_asked");
        assert_eq!(encoded["event"].as_object().unwrap().len(), 3);
    }

    #[test]
    fn exec_approval_projects_the_command_under_review() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 10,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "exec".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::ExecMayRunNetworkedCommand,
                preview: ToolActionPreview::build(
                    "exec",
                    &serde_json::json!({
                        "command": "cargo",
                        "args": ["test", "--workspace"],
                        "cwd": "checkout",
                    }),
                ),
                summary: "private model-authored summary".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains(r#""action":"exec""#));
        assert!(json.contains(r#""tool":"exec""#));
        assert!(json.contains(r#""command":"cargo""#));
        assert!(json.contains(r#""args":["test","--workspace"]"#));
        assert!(json.contains(r#""cwd":"checkout""#));
        // The preview replaces the model-authored summary; it does not join it.
        assert!(!json.contains("private model-authored summary"));
    }

    #[test]
    fn exec_completion_projects_what_ran_and_what_it_produced() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 12,
            event: AgentEvent::ToolCallCompleted {
                call_id: CallId::new(),
                output: ToolOutput::text("provider: local\nexit: 0").with_data(serde_json::json!({
                    "provider": "local",
                    "exit_code": 0,
                    "timed_out": false,
                    "output_truncated": false,
                    "duration_ms": 12,
                    "stdout": "two tests passed\n",
                    "stderr": "",
                })),
                action: ToolActionPreview::build(
                    "exec",
                    &serde_json::json!({ "command": "cargo", "args": ["test"] }),
                ),
                result: ToolResultPreview::build(
                    "exec",
                    Some(&serde_json::json!({
                        "provider": "local",
                        "exit_code": 0,
                        "duration_ms": 12,
                        "stdout": "two tests passed\n",
                        "stderr": "",
                    })),
                ),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains(r#""command":"cargo""#));
        assert!(json.contains(r#""stdout":"two tests passed\n""#));
        assert!(json.contains(r#""exit_code":0"#));
        // Only the enumerated fields cross. The provider identity and the
        // model-facing content stay behind the boundary.
        assert!(!json.contains("provider"));
        assert!(!json.contains("duration_ms"));
        assert!(!json.contains("exit: 0"));
    }

    #[test]
    fn completions_without_a_projection_stay_closed() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 13,
            event: AgentEvent::ToolCallCompleted {
                call_id: CallId::new(),
                output: ToolOutput::text("private result").with_data(serde_json::json!({
                    "stdout": "private stream",
                })),
                // `write_file` projects neither an action nor a result, so
                // nothing about it may cross — not the output text, and not
                // the structured payload beside it.
                action: ToolActionPreview::build(
                    "write_file",
                    &serde_json::json!({ "path": "private/path" }),
                ),
                result: ToolResultPreview::build(
                    "write_file",
                    Some(&serde_json::json!({ "stdout": "private stream" })),
                ),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(!json.contains("action"));
        assert!(!json.contains("result"));
        assert!(!json.contains("private"));
    }

    #[test]
    fn approvals_without_a_preview_stay_closed() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 11,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "write_file".into(),
                class: ApprovalClass::Workspace,
                kind: ToolApprovalKind::Unsupported,
                // A tool with no variant projects nothing, so the card has no
                // action to show and the summary is all that crosses.
                preview: ToolActionPreview::build(
                    "write_file",
                    &serde_json::json!({ "path": "private/path" }),
                ),
                summary: "a write".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(!json.contains("preview"));
        assert!(!json.contains("private"));
    }

    /// Consent to an action you cannot see is not consent, and for a web search
    /// the action *is* the query — it is the thing that leaves the device. So
    /// this one has to cross, while everything else about the call still does
    /// not.
    #[test]
    fn a_web_search_approval_carries_the_query_being_shared() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 11,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "web_search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::WebSearchMayShareQuery,
                preview: ToolActionPreview::build(
                    "web_search",
                    &serde_json::json!({ "query": "quarterly filings", "max_results": 5 }),
                ),
                summary: "a web search".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains("quarterly filings"), "{json}");
        // Only the query. The other arguments are not what consent is about.
        assert!(!json.contains("max_results"), "{json}");
    }

    /// Path of the checked-in renderer contract, relative to this crate.
    const TOOL_NAME_CONTRACT: &str = "../openwave-desktop/ui/src/renderer-tool-names.json";

    /// The desktop maintains four hand-written tables keyed on this vocabulary.
    /// Nothing linked them, and three tools shipped missing an entry in one or
    /// another. So write the list down and let both sides check against it.
    ///
    /// Regenerate with `UPDATE_RENDERER_CONTRACT=1 cargo test -p openwave-server`.
    /// The check is the guarantee, not the generation: CI fails on a diff.
    #[test]
    fn the_renderer_tool_name_contract_is_written_down_and_current() {
        // `position` is what makes `ALL` safe to trust: a new variant cannot
        // compile without an arm there, and this catches forgetting `ALL`.
        for (index, name) in RendererToolName::ALL.iter().enumerate() {
            assert_eq!(name.position(), index, "{name:?} is out of order in ALL");
        }
        assert_eq!(
            RendererToolName::ALL.len(),
            RendererToolName::Other.position() + 1,
            "ALL is missing a variant"
        );

        let names: Vec<String> = RendererToolName::ALL
            .iter()
            .copied()
            .map(RendererToolName::wire_name)
            .collect();
        assert_eq!(
            names.iter().collect::<std::collections::HashSet<_>>().len(),
            names.len(),
            "two names share a wire spelling"
        );

        let rendered = format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                "comment": "Generated from RendererToolName. Regenerate with \
                            UPDATE_RENDERER_CONTRACT=1 cargo test -p openwave-server.",
                "tools": names,
            }))
            .expect("a list of names always serializes")
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(TOOL_NAME_CONTRACT);
        if std::env::var_os("UPDATE_RENDERER_CONTRACT").is_some() {
            std::fs::write(&path, &rendered).expect("the contract path is writable");
            return;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            existing, rendered,
            "the renderer tool-name contract is out of date; regenerate with \
             UPDATE_RENDERER_CONTRACT=1 cargo test -p openwave-server"
        );
    }

    #[test]
    fn source_tools_use_fixed_renderer_names() {
        assert_eq!(
            RendererToolName::from(crate::source_tools::LIST_SOURCES_TOOL),
            RendererToolName::ListSources
        );
        assert_eq!(
            RendererToolName::from(crate::source_tools::READ_SOURCE_TOOL),
            RendererToolName::ReadSource
        );
        assert_eq!(
            RendererToolName::from("create_deliverable"),
            RendererToolName::CreateDeliverable
        );
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

    #[test]
    fn search_approval_projects_frozen_egress_kind_without_private_payload() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 7,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::SearchMayShareQueryAndExcerpts,
                preview: None,
                summary: "private query and document title".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains("search_may_share_query_and_excerpts"));
        assert!(!json.contains("private query"));
        assert!(!json.contains("document title"));
    }

    #[test]
    fn web_search_approval_projects_narrow_query_egress_kind() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 8,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "web_search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::WebSearchMayShareQuery,
                preview: None,
                summary: "private query and domain filters".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains(r#""action":"web_search""#));
        assert!(json.contains(r#""approval":"web_search_may_share_query""#));
        assert!(!json.contains("private query"));
        assert!(!json.contains("domain filters"));
    }

    #[test]
    fn mcp_approval_projects_one_generic_action_without_remote_metadata() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 9,
            event: AgentEvent::ApprovalRequired {
                call_id: CallId::new(),
                tool_name: "mcp__private_server__private_tool".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::ExternalMcpMayCallServer,
                preview: None,
                summary: "private model-authored arguments".into(),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains(r#""action":"other""#));
        assert!(json.contains(r#""approval":"external_mcp_may_call_server""#));
        assert!(!json.contains("private_server"));
        assert!(!json.contains("private_tool"));
        assert!(!json.contains("model-authored"));
    }

    #[test]
    fn orchestration_and_continuation_tools_use_fixed_renderer_names() {
        for (name, expected) in [
            ("spawn_sandbox_agent", r#""name":"spawn_sandbox_agent""#),
            ("wait_for_agents", r#""name":"wait_for_agents""#),
            (
                openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL,
                r#""name":"read_delegated_file""#,
            ),
            (
                openwave_core::ASK_USER_QUESTIONS_TOOL,
                r#""name":"ask_user_questions""#,
            ),
        ] {
            let projected = RendererSequencedEvent::from(&SequencedEvent {
                seq: 1,
                event: AgentEvent::ToolCallStarted {
                    call_id: CallId::new(),
                    name: name.into(),
                },
            });
            let json = serde_json::to_string(&projected).unwrap();
            assert!(json.contains(expected));
        }
    }
}
