//! Renderer-safe projection of the internal agent journal.
//!
//! The durable [`AgentEvent`] is an internal coordination record. It can contain
//! model-generated tool arguments, tool results, host paths, and provider
//! diagnostics. WebSocket clients receive this deliberately closed projection
//! instead.
//!
//! Reasoning is the one payload that used to be withheld and no longer is. What
//! reaches [`AgentEvent::ReasoningDelta`] is not raw chain-of-thought: every
//! adapter that emits it emits what the provider chose to expose for display.
//! The Anthropic path asks for `display: "summarized"` explicitly, and would
//! otherwise stream thinking blocks with empty text; Gemini's `thought` parts
//! are thought summaries; the OpenAI-compatible path forwards
//! `reasoning_content`/`reasoning`, which a gateway sets for exactly this
//! purpose. Withholding it cost the transcript the account of how an answer was
//! reached without protecting anything the provider was not already publishing.

use openwave_core::{
    AgentEvent, ApprovalClass, CallId, MessageId, RendererToolName, SequencedEvent,
    ToolActionPreview, ToolApprovalKind, ToolResultPreview, TurnId,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One frame on a chat's event socket.
///
/// Untagged, so a journaled event frame is byte-identical to what it has always
/// been: the sequence is the client's resume cursor and its dedup key, and every
/// consumer of it — replay, hydration, the session reducer — reads `seq` as the
/// only ordering there is. A metadata frame carries no sequence because it is
/// not part of that order, and a client tells the two apart by the `metadata`
/// discriminator rather than by a sequence it would have to invent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub(crate) enum RendererChatFrame {
    /// A journaled turn event at its sequence. Boxed: an event frame carries
    /// tool previews and dwarfs a metadata frame, and every frame is built
    /// once and serialized, so the indirection costs nothing on the hot path.
    Event(Box<RendererSequencedEvent>),
    /// Chat state that changed outside the journal.
    Metadata(RendererChatMetadata),
}

/// Chat metadata pushed to an open client, outside the turn journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "metadata", rename_all = "snake_case")]
pub(crate) enum RendererChatMetadata {
    /// The chat was named — by titling, for a chat that had no name.
    Titled { title: String },
    /// A post-turn file-change summary is ready for transcript hydration.
    FileChangesRecorded { turn_id: TurnId },
}

impl From<&crate::bus::ChatMetadataNotice> for RendererChatMetadata {
    fn from(value: &crate::bus::ChatMetadataNotice) -> Self {
        match value {
            crate::bus::ChatMetadataNotice::Titled { title } => Self::Titled {
                title: title.clone(),
            },
            crate::bus::ChatMetadataNotice::FileChangesRecorded { turn_id } => {
                Self::FileChangesRecorded { turn_id: *turn_id }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub(crate) struct RendererSequencedEvent {
    pub seq: i64,
    pub event: RendererAgentEvent,
}

/// Bounded refusal metadata safe to present in the desktop transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub(crate) struct RendererRefusal {
    pub category: Option<String>,
    pub partial_output: bool,
}

impl From<&openwave_core::RefusalOutcome> for RendererRefusal {
    fn from(value: &openwave_core::RefusalOutcome) -> Self {
        Self {
            category: value.category().map(str::to_owned),
            partial_output: value.partial_output(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum RendererAgentEvent {
    TurnStarted {
        turn_id: TurnId,
    },
    TextDelta {
        text: String,
    },
    /// A fragment of the provider's presentable reasoning summary.
    ReasoningDelta {
        text: String,
    },
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
    PlanProposed {
        call_id: CallId,
        turn_id: TurnId,
    },
    ApprovalRequired {
        call_id: CallId,
        action: RendererToolName,
        approval: ToolApprovalKind,
        class: ApprovalClass,
        /// Whether the Auto-mode judge owns this card right now. The card
        /// stays fully actionable either way; this only adds the "deciding
        /// automatically" hint.
        #[serde(default)]
        auto_judging: bool,
        /// Complete standing-grant ladder for this exact call, narrowest first.
        /// Empty means only one-shot approval is available.
        #[serde(default)]
        grant_rungs: Vec<crate::routes::ApprovalGrantRung>,
        /// The one deliberate opening in this boundary. A human cannot consent
        /// to a command they are not shown, so a tool may project a closed,
        /// field-by-field view of the action under review. Tools without one
        /// send nothing, as every tool did before.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
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
        #[ts(optional)]
        action: Option<ToolActionPreview>,
        /// What the call produced. A command's output is the reason it ran;
        /// withholding it leaves the transcript asserting that something
        /// happened without ever showing what.
        #[serde(skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<ToolResultPreview>,
    },
    TurnCompleted,
    TurnRefused {
        refusal: RendererRefusal,
    },
    TurnFailed {
        /// Why the turn failed, at the only resolution a client can act on.
        /// The failure's `kind` and `message` stay internal.
        category: TurnFailureCategory,
    },
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

/// Why a turn failed, closed and coarse enough to be stable.
///
/// A failure's `kind` is an internal diagnostic vocabulary: it grows with the
/// server, and its `message` can carry provider diagnostics and host paths, so
/// neither crosses to the renderer. What a client actually needs is narrower —
/// what to tell the person, and whether running the same turn again could
/// plausibly do anything different. This enum is exactly that, and nothing is
/// worth a variant here unless a client would say or do something different
/// for it.
///
/// It is also the worker's own retry taxonomy — the same classification decides
/// whether a failed turn is rescheduled — so the category a client sees and the
/// category the scheduler acted on cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TurnFailureCategory {
    /// The provider throttled or shed the request. Automatic retries were
    /// already spent, but waiting and asking again is the actual remedy.
    RateLimited,
    /// The provider rejected our credentials. Retrying replays the same
    /// rejection; only fixing the key changes the outcome.
    Auth,
    /// A transient fault below the turn — an upstream error, a storage or
    /// secret-store failure. Retrying is reasonable.
    Transient,
    /// Everything else: budgets the turn exceeded, malformed agent output,
    /// internal invariants. Retrying is a guess, so a client should not promise
    /// that it helps.
    Unknown,
}

impl TurnFailureCategory {
    /// Classify a failure `kind` from the internal vocabulary.
    ///
    /// Unrecognized kinds fall to [`Self::Unknown`], so a new internal failure
    /// code is coarse rather than wrong.
    pub(crate) fn from_kind(kind: &str) -> Self {
        match kind {
            "rate_limited" | "overloaded" => Self::RateLimited,
            "authentication" => Self::Auth,
            "provider" | "store" | "secret" | "empty_model_response" => Self::Transient,
            _ => Self::Unknown,
        }
    }

    /// Whether running the same turn again could plausibly succeed.
    pub(crate) const fn retries_may_succeed(self) -> bool {
        matches!(self, Self::RateLimited | Self::Transient)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RendererToolStatus {
    Completed,
    Failed,
}

impl From<&SequencedEvent> for RendererSequencedEvent {
    fn from(value: &SequencedEvent) -> Self {
        let event = match &value.event {
            AgentEvent::TurnStarted { turn_id } => {
                RendererAgentEvent::TurnStarted { turn_id: *turn_id }
            }
            AgentEvent::TextDelta { text } => RendererAgentEvent::TextDelta { text: text.clone() },
            AgentEvent::ReasoningDelta { text } => {
                RendererAgentEvent::ReasoningDelta { text: text.clone() }
            }
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
            AgentEvent::PlanProposed { call_id, turn_id } => RendererAgentEvent::PlanProposed {
                call_id: *call_id,
                turn_id: *turn_id,
            },
            AgentEvent::ApprovalRequired {
                auto_judging,
                call_id,
                tool_name,
                class,
                kind,
                grant_scopes,
                preview,
                ..
            } => RendererAgentEvent::ApprovalRequired {
                call_id: *call_id,
                action: tool_name.as_str().into(),
                approval: *kind,
                class: *class,
                auto_judging: *auto_judging,
                grant_rungs: crate::routes::grant_rungs_from_scopes(grant_scopes, true),
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
            AgentEvent::TurnRefused { refusal, .. } => RendererAgentEvent::TurnRefused {
                refusal: refusal.into(),
            },
            AgentEvent::TurnFailed { error } => RendererAgentEvent::TurnFailed {
                category: TurnFailureCategory::from_kind(&error.kind),
            },
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
    use openwave_core::{AgentError, RefusalDetails, RefusalOutcome, ToolOutput, Usage};

    use super::*;

    #[test]
    fn refusal_projection_exposes_only_bounded_presentation_metadata() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 12,
            event: AgentEvent::TurnRefused {
                usage: Usage {
                    input_tokens: 42,
                    output_tokens: 7,
                    ..Usage::default()
                },
                refusal: RefusalOutcome::new(
                    RefusalDetails::from_category(Some("general_harms")),
                    true,
                ),
            },
        });

        assert_eq!(
            projected,
            RendererSequencedEvent {
                seq: 12,
                event: RendererAgentEvent::TurnRefused {
                    refusal: RendererRefusal {
                        category: Some("general_harms".into()),
                        partial_output: true,
                    },
                },
            }
        );
        let json = serde_json::to_string(&projected).unwrap();
        assert!(!json.contains("input_tokens"), "{json}");
        assert!(!json.contains("output_tokens"), "{json}");
    }

    #[test]
    fn projection_redacts_internal_payloads_and_unknown_tool_names() {
        let call_id = CallId::new();
        // `ReasoningDelta` is deliberately absent: reasoning is a presentable
        // provider summary rather than an internal payload, and it now crosses
        // in full. `reasoning_summary_crosses_to_the_renderer` pins that.
        let cases = [
            AgentEvent::ToolCallStarted {
                call_id,
                name: "provider_tool_with_secret".into(),
            },
            AgentEvent::ToolCallArgsDelta {
                call_id,
                fragment: r#"{\"path\":\"/Users/private\"}"#.into(),
            },
            AgentEvent::ApprovalRequired {
                auto_judging: false,
                call_id,
                tool_name: "provider_tool_with_secret".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::Unsupported,
                grant_scopes: Vec::new(),
                preview: None,
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

    /// Reasoning used to be projected as a bare spinner signal, which left the
    /// renderer no way to show the account of how an answer was reached even
    /// though the journal held it in full. Carrying the text is the contract the
    /// thinking accordion is built on, so a projection that quietly went back to
    /// a unit variant would silently empty it.
    #[test]
    fn reasoning_summary_crosses_to_the_renderer() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 7,
            event: AgentEvent::ReasoningDelta {
                text: "weighing two approaches".into(),
            },
        });
        assert_eq!(
            projected.event,
            RendererAgentEvent::ReasoningDelta {
                text: "weighing two approaches".into(),
            }
        );
    }

    /// The category is the one thing a failure is allowed to tell a client, and
    /// a client branches on it — so both halves of that boundary are contract:
    /// the classification each failure lands in, and the diagnostics that stay
    /// behind.
    #[test]
    fn failure_projection_carries_a_category_and_nothing_else() {
        let cases = [
            (
                AgentError::RateLimited("upstream said 429 for key sk-live".into()),
                TurnFailureCategory::RateLimited,
            ),
            (
                AgentError::Authentication("invalid x-api-key sk-live".into()),
                TurnFailureCategory::Auth,
            ),
            (
                AgentError::Provider("upstream 503 from api.example".into()),
                TurnFailureCategory::Transient,
            ),
            (
                AgentError::msg("max steps per turn were consumed"),
                TurnFailureCategory::Unknown,
            ),
        ];

        for (error, expected) in cases {
            let projected = RendererSequencedEvent::from(&SequencedEvent {
                seq: 1,
                event: AgentEvent::TurnFailed {
                    error: (&error).into(),
                },
            });
            assert_eq!(
                projected.event,
                RendererAgentEvent::TurnFailed { category: expected },
                "{error}"
            );
            let encoded = serde_json::to_value(&projected).unwrap();
            assert_eq!(
                encoded["event"].as_object().unwrap().len(),
                2,
                "the failure frame carries only its type and category: {encoded}"
            );
        }
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "exec".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::ExecMayRunNetworkedCommand,
                grant_scopes: openwave_core::GrantScope::ladder_for(
                    "exec",
                    &serde_json::json!({
                        "command": "cargo",
                        "args": ["test", "--workspace"],
                        "cwd": "checkout",
                    }),
                ),
                preview: ToolActionPreview::build(
                    "exec",
                    &serde_json::json!({
                        "command": "cargo",
                        "args": ["test", "--workspace"],
                        "cwd": "checkout",
                    }),
                ),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains(r#""action":"exec""#));
        assert!(json.contains(r#""tool":"exec""#));
        assert!(json.contains(r#""command":"cargo""#));
        assert!(json.contains(r#""args":["test","--workspace"]"#));
        assert!(json.contains(r#""cwd":"checkout""#));
    }

    #[test]
    fn interpreter_approval_projects_no_remembered_grant_choices() {
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 11,
            event: AgentEvent::ApprovalRequired {
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "exec".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::ExecMayRunNetworkedCommand,
                grant_scopes: Vec::new(),
                preview: ToolActionPreview::build(
                    "exec",
                    &serde_json::json!({
                        "command": "python3",
                        "args": ["-c", "import pptx"],
                        "cwd": ".",
                    }),
                ),
            },
        });

        let encoded = serde_json::to_value(projected).unwrap();
        assert_eq!(
            encoded["event"]["grant_rungs"],
            serde_json::json!([]),
            "only one-shot approval may be presented"
        );
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
                    &ToolOutput::text("provider: local").with_data(serde_json::json!({
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
    fn an_mcp_completion_projects_a_view_reference_and_no_remote_metadata() {
        let output = ToolOutput::text("remote result text")
            .with_data(serde_json::json!({ "remote_field": "remote value" }))
            .with_ui_view(openwave_core::ToolUiView {
                server: "gateway".into(),
                resource_uri: "ui://gateway/app.html".into(),
            });
        let projected = RendererSequencedEvent::from(&SequencedEvent {
            seq: 14,
            event: AgentEvent::ToolCallCompleted {
                call_id: CallId::new(),
                action: ToolActionPreview::build(
                    "mcp__gateway__private_remote_tool",
                    &serde_json::json!({ "model": "authored" }),
                ),
                result: ToolResultPreview::build("mcp__gateway__private_remote_tool", &output),
                output,
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        // The deliberate opening: a typed reference the renderer resolves
        // through the dedicated view route into a sandboxed frame. The server
        // namespace is user-authored configuration, already shown in Settings.
        assert!(json.contains(r#""tool":"mcp_app""#));
        assert!(json.contains(r#""server":"gateway""#));
        assert!(json.contains(r#""resource_uri":"ui://gateway/app.html""#));
        // Everything remote-authored other than the validated reference stays
        // behind the boundary: tool name, output text, structured payload.
        assert!(!json.contains("private_remote_tool"));
        assert!(!json.contains("remote result text"));
        assert!(!json.contains("remote_field"));
        assert!(!json.contains("remote value"));
        assert!(!json.contains("authored"));
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
                    &ToolOutput::text("private result")
                        .with_data(serde_json::json!({ "stdout": "private stream" })),
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "write_file".into(),
                class: ApprovalClass::Workspace,
                kind: ToolApprovalKind::Unsupported,
                grant_scopes: Vec::new(),
                // A tool with no variant projects nothing, so the card has no
                // action to show; the desktop renders its own canned ask from
                // the approval kind.
                preview: ToolActionPreview::build(
                    "write_file",
                    &serde_json::json!({ "path": "private/path" }),
                ),
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "web_search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::WebSearchMayShareQuery,
                grant_scopes: openwave_core::GrantScope::ladder_for(
                    "web_search",
                    &serde_json::json!({
                        "query": "quarterly filings",
                        "max_results": 5,
                    }),
                ),
                preview: ToolActionPreview::build(
                    "web_search",
                    &serde_json::json!({ "query": "quarterly filings", "max_results": 5 }),
                ),
            },
        });
        let json = serde_json::to_string(&projected).unwrap();
        assert!(json.contains("quarterly filings"), "{json}");
        // Only the query. The other arguments are not what consent is about.
        assert!(!json.contains("max_results"), "{json}");
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::SearchMayShareQueryAndExcerpts,
                grant_scopes: vec![openwave_core::GrantScope::WholeTool],
                preview: None,
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "web_search".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::WebSearchMayShareQuery,
                grant_scopes: Vec::new(),
                preview: None,
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
                auto_judging: false,
                call_id: CallId::new(),
                tool_name: "mcp__private_server__private_tool".into(),
                class: ApprovalClass::Sensitive,
                kind: ToolApprovalKind::ExternalMcpMayCallServer,
                grant_scopes: Vec::new(),
                preview: None,
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
