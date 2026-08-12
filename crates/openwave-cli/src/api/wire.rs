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

/// A metadata push: chat state that changed outside the turn journal.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "metadata", rename_all = "snake_case")]
pub enum MetadataFrame {
    /// The chat was named — by titling, for a chat that had no name.
    Titled { title: String },
    /// A post-turn file-change summary is ready; the TUI has no surface for
    /// the undo flow, but recognizing the frame keeps the tag decode closed.
    FileChangesRecorded,
    /// Code execution is preparing its sandbox image, or has stopped.
    SandboxPreparing { preparing: bool },
    /// A newer metadata kind this build does not know.
    #[serde(other)]
    Unknown,
}

/// A turn's token accounting, as the terminal events carry it. The four
/// counts are disjoint; the context footprint is their plain sum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct TurnUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

impl TurnUsage {
    /// Everything that occupied the model's context across the turn.
    pub fn context_tokens(self) -> u64 {
        u64::from(self.input_tokens)
            + u64::from(self.output_tokens)
            + u64::from(self.cache_read_input_tokens)
            + u64::from(self.cache_creation_input_tokens)
    }
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
    UserQuestionsAsked {
        #[serde(default)]
        call_id: Option<CallId>,
    },
    PlanProposed {
        #[serde(default)]
        call_id: Option<CallId>,
    },
    ApprovalRequired {
        call_id: CallId,
        action: String,
        approval: String,
        #[serde(default)]
        auto_judging: bool,
        /// Standing-grant ladder for this exact call, narrowest first.
        #[serde(default)]
        grant_rungs: Vec<GrantRung>,
        #[serde(default)]
        preview: Option<serde_json::Value>,
    },
    ApprovalDecided {
        call_id: CallId,
        #[serde(default)]
        approved: Option<bool>,
    },
    ToolCallCompleted {
        call_id: CallId,
        status: ToolCallStatus,
        /// What the call did, when its tool projects it.
        #[serde(default)]
        action: Option<serde_json::Value>,
        /// What the call produced, when its tool projects it.
        #[serde(default)]
        result: Option<serde_json::Value>,
    },
    TurnCompleted {
        #[serde(default)]
        usage: TurnUsage,
    },
    TurnRefused {
        refusal: Refusal,
        #[serde(default)]
        usage: TurnUsage,
    },
    TurnFailed {
        category: String,
        #[serde(default)]
        model: Option<ModelIdentity>,
    },
    TurnCancelled {
        #[serde(default)]
        usage: TurnUsage,
    },
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

/// How wide a standing grant the server offers on an approval, mirroring
/// `ApprovalGrantRung`. Decodes from and encodes to the same wire strings so
/// the decision can name the rung back verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantRung {
    /// Exactly the action the card showed.
    ExactAction,
    /// A leading run of the command's argv tokens.
    CommandPrefix { tokens: usize },
    /// A leading run of a workspace write's path segments.
    PathPrefix { segments: usize },
    /// Every call to this tool.
    WholeTool,
}

/// Exact model route involved in a provider failure.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelIdentity {
    pub id: String,
    #[serde(default)]
    pub provider: Option<String>,
}

/// Whether a finished tool call succeeded. Unknown statuses decode rather than
/// failing; they render as failures (the conservative display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Completed,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// Bounded refusal metadata, as much as a client can act on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Refusal {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub partial_output: bool,
}

/// One entry of the durable transcript, from `GET /chats/{id}/messages`.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageSnapshot {
    pub role: TranscriptRole,
    pub content: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Images submitted with a user message (identity and geometry only).
    #[serde(default)]
    pub image_attachments: Option<Vec<ImageAttachment>>,
    /// Files submitted with a user message.
    #[serde(default)]
    pub file_attachments: Option<Vec<FileAttachment>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptRole {
    User,
    Assistant,
    /// A durable host-authored note between turns; shown as a subtle notice.
    System,
    /// The transcript never emits these, but decode defensively anyway.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageAttachment {
    pub media_type: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileAttachment {
    pub name: String,
    /// The document's media type; decoded for forward compatibility, not yet
    /// shown on the TUI's attachment chips.
    #[allow(dead_code)]
    #[serde(default)]
    pub media_type: String,
}

/// One finished tool call in history, projected through the renderer-safe
/// allowlist. Only the fields the TUI renders are declared.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolActivitySnapshot {
    /// The call's durable identity; the TUI doesn't key history by it yet,
    /// but it is part of the wire contract.
    #[allow(dead_code)]
    pub call_id: CallId,
    pub tool: String,
    #[serde(default)]
    pub action: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
}

/// One terminal turn's status and visible content.
#[derive(Debug, Clone, Deserialize)]
pub struct TerminalTurnSnapshot {
    /// The turn's identity and its partial/reasoning text are part of the
    /// wire contract; the TUI reads the status, usage, and failure fields and
    /// doesn't reprint settled partial text.
    #[allow(dead_code)]
    pub turn_id: TurnId,
    pub status: TerminalTurnStatus,
    #[allow(dead_code)]
    #[serde(default)]
    pub partial_content: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub refusal: Option<Refusal>,
    #[serde(default)]
    pub failure_category: Option<String>,
    #[serde(default)]
    pub usage: TurnUsage,
    #[allow(dead_code)]
    pub finished_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalTurnStatus {
    Completed,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// The visible transcript plus the journal watermark that produced it.
#[derive(Debug, Clone, Deserialize)]
pub struct Transcript {
    #[serde(default)]
    pub messages: Vec<MessageSnapshot>,
    #[serde(default)]
    pub tool_activity: Vec<ToolActivitySnapshot>,
    #[serde(default)]
    pub terminal_turns: Vec<TerminalTurnSnapshot>,
    #[serde(default)]
    pub last_event_seq: i64,
}

/// The chat record, as `GET /chats` and `GET /chats/{id}` return it.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatSummary {
    pub id: openwave_core::ChatId,
    /// The project this chat belongs to; decoded so the move picker can show
    /// the current home, not yet rendered.
    #[allow(dead_code)]
    #[serde(default)]
    pub project_id: Option<openwave_core::ProjectId>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// When the chat was created; the TUI doesn't sort or label by it yet.
    #[allow(dead_code)]
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One project (workspace), as `GET /projects` returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectSummary {
    pub id: openwave_core::ProjectId,
    #[serde(default)]
    pub title: Option<String>,
    /// When the project was created; not shown in the picker.
    #[allow(dead_code)]
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One item waiting on the user, from `GET /inbox`.
#[derive(Debug, Clone, Deserialize)]
pub struct InboxItem {
    pub chat_id: openwave_core::ChatId,
    /// The detail fields are part of the wire contract; the TUI's inbox use
    /// is the attention marker, so only the chat identity is read today.
    #[allow(dead_code)]
    #[serde(default)]
    pub chat_title: Option<String>,
    #[allow(dead_code)]
    pub kind: String,
    #[allow(dead_code)]
    #[serde(default)]
    pub action: Option<String>,
    #[allow(dead_code)]
    pub requested_at: chrono::DateTime<chrono::Utc>,
}

/// One background agent run, from `GET /chats/{id}/agent-runs`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentRunSnapshot {
    pub id: openwave_core::AgentRunId,
    #[serde(default)]
    pub parent_id: Option<openwave_core::AgentRunId>,
    #[serde(default)]
    pub tier: Option<String>,
    /// Where the agent run loop itself executes (`in_process` / `container`).
    /// Not the code-exec backend — see [`Self::code_execution_provider`].
    #[serde(default)]
    pub execution_location: Option<String>,
    /// Host code-execution backend for `exec` (`local` / `e2b` / `docker` /
    /// `daytona` / `off`). Independent of [`Self::execution_location`].
    #[serde(default)]
    pub code_execution_provider: Option<String>,
    pub status: String,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub last_error_code: Option<String>,
    #[serde(default)]
    pub activity: Option<AgentActivity>,
    /// Files the run named in its terminal `done` submission.
    #[serde(default)]
    pub submitted_outputs: Vec<SubmittedOutput>,
    #[serde(default)]
    pub terminal_text: Option<String>,
    /// The call that spawned this run; lets a transcript attach the run to
    /// its spawning step. The TUI doesn't join the two yet.
    #[allow(dead_code)]
    #[serde(default)]
    pub spawn_call_id: Option<CallId>,
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One file a background run submitted, as the list route returns it.
#[derive(Debug, Clone, Deserialize)]
pub struct SubmittedOutput {
    pub output_id: openwave_core::OutputId,
    pub filename: String,
}

/// The live checkpoint of a background run.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentActivity {
    pub kind: AgentActivityKind,
    pub status: String,
}

/// The renderer-safe activity vocabulary, closed like the server's.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    Exec,
    WebSearch,
    ReadDelegatedFile,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
    ImportConnectedFile,
    #[serde(other)]
    Other,
}

impl AgentActivityKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Exec => "a command",
            Self::WebSearch => "a web search",
            Self::ReadDelegatedFile => "a delegated file",
            Self::ListConnectedFolders => "connected folders",
            Self::ListFolder => "a folder",
            Self::ReadConnectedFile => "a file",
            Self::ImportConnectedFile => "an import",
            Self::Other => "a step",
        }
    }

    /// Stable snake_case name for JSON drivers and text listings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::WebSearch => "web_search",
            Self::ReadDelegatedFile => "read_delegated_file",
            Self::ListConnectedFolders => "list_connected_folders",
            Self::ListFolder => "list_folder",
            Self::ReadConnectedFile => "read_connected_file",
            Self::ImportConnectedFile => "import_connected_file",
            Self::Other => "other",
        }
    }
}

/// One history entry in a background run's activity timeline.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentActivityItem {
    pub kind: AgentActivityKind,
    pub outcome: String,
    pub at: chrono::DateTime<chrono::Utc>,
    /// Renderer-safe command/query detail when the server supplies it.
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

/// One pending approval, from `GET /chats/{id}/approvals`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingApprovalSnapshot {
    pub call_id: CallId,
    pub action: String,
    pub approval: String,
    #[serde(default)]
    pub auto_judge_status: Option<String>,
    #[serde(default)]
    pub preview: Option<serde_json::Value>,
    /// Whether the kind is approvable at all; a `false` card is decline-only.
    /// The TUI shows the same card either way today.
    #[allow(dead_code)]
    #[serde(default)]
    pub can_approve: bool,
    #[serde(default)]
    pub grant_rungs: Vec<GrantRung>,
}

/// One proposed plan awaiting review, from `GET /chats/{id}/plans/pending`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingPlan {
    pub call_id: CallId,
    /// The turn that proposed it; part of the wire contract, not rendered.
    #[allow(dead_code)]
    pub turn_id: TurnId,
    pub title: String,
    pub plan: String,
}

/// One block of questions the model is asking, from
/// `GET /chats/{id}/questions/pending`.
#[derive(Debug, Clone, Deserialize)]
pub struct PendingQuestions {
    pub call_id: CallId,
    /// The turn that asked; part of the wire contract, not rendered.
    #[allow(dead_code)]
    pub turn_id: TurnId,
    #[serde(default)]
    pub questions: Vec<UserQuestion>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserQuestion {
    pub id: String,
    #[serde(default)]
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// `"single"` or `"multi"`; anything else treats as single.
    #[serde(default)]
    pub question_type: String,
    #[serde(default)]
    pub allow_free_form: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuestionOption {
    /// The option's identity, which the answer's `selected_option_ids` names.
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One selectable model, from `GET /models`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub key: String,
    pub id: String,
    pub display_name: String,
    /// The provider that serves the model; decoded for forward compatibility,
    /// not yet grouped on in the picker.
    #[allow(dead_code)]
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub context_window: u32,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
}

/// The model catalog response.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelCatalog {
    #[serde(default)]
    pub models: Vec<ModelInfo>,
    /// Every named model role, its selection, and what it resolves to now.
    #[serde(default)]
    pub roles: Vec<ModelRoleInfo>,
}

/// One named model role, from `GET /models`.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelRoleInfo {
    pub role: String,
    /// The catalog key pinned to this role, or `None` for automatic.
    #[serde(default)]
    pub selection: Option<String>,
    /// What the role resolves to right now, selection or not.
    #[serde(default)]
    pub resolved_key: Option<String>,
}

/// One provider row from `GET /providers`.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInfo {
    /// Wire form of the provider kind (`anthropic`, `openai_compatible`, …),
    /// which is also the path segment the write routes take.
    pub kind: String,
    #[serde(default)]
    pub enabled: bool,
    /// Whether a credential is stored — never the credential itself.
    #[serde(default)]
    pub has_credential: bool,
    /// How the provider is authenticated (`api_key`, `chatgpt`, …), when it is.
    #[serde(default)]
    pub auth_mode: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

/// The `GET /providers` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct ProvidersList {
    #[serde(default)]
    pub providers: Vec<ProviderInfo>,
}

/// One mounted MCP server, from `GET /mcp/servers`. The response flattens each
/// server's definition into its live projection, so both are read here.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    /// `initializing` / `healthy` / `degraded` / `reconnecting` / `disabled`.
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub tool_count: usize,
    /// The plugin this server came from, when it is plugin-sourced. Those are
    /// derived by the server and cannot be edited over the config route.
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub gateway_endpoint: Option<String>,
    #[serde(default)]
    pub diagnostic: Option<String>,
}

/// The `GET /mcp/servers` envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServersInfo {
    #[serde(default)]
    pub servers: Vec<McpServerInfo>,
}

/// One row of a conversation's outputs catalog.
///
/// The output routes answer in the shape the desktop renderer already
/// validates, which is why these fields are camelCase where the rest of the
/// API is not — moving the surface off Tauri was a transport change, not a
/// payload change.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSummary {
    pub output_id: openwave_core::OutputId,
    pub filename: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub revision_count: u32,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputsCatalog {
    pub deliverables: Vec<OutputSummary>,
    /// Whether the conversation has more outputs than one answer carries.
    pub truncated: bool,
}

/// One output's bounded text preview.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPreview {
    pub filename: String,
    pub media_type: String,
    pub revision_id: openwave_core::OutputRevisionId,
    pub content: String,
    pub truncated: bool,
}

/// One row of an output's version history.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRevisionRow {
    pub revision_id: openwave_core::OutputRevisionId,
    pub ordinal: u32,
    pub size_bytes: u64,
    pub created_at: String,
    pub produced_by: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputRevisions {
    pub revisions: Vec<OutputRevisionRow>,
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
            ChatFrame::Metadata(MetadataFrame::Titled {
                title: "A chat".into()
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
        // tool's closed preview of the action under review, plus the grant
        // ladder the decision can name back.
        let approval = frame(&format!(
            r#"{{"seq":4,"event":{{"type":"approval_required","call_id":"{started_call}","action":"exec","approval":"exec_may_run_networked_command","class":"sensitive","grant_rungs":["exact_action",{{"command_prefix":{{"tokens":1}}}},"whole_tool"],"preview":{{"tool":"exec","command":"git","args":["status"],"cwd":".","files":[]}}}}}}"#
        ));
        let ChatFrame::Event(approval) = approval else {
            panic!("expected an event frame: {approval:?}");
        };
        let ClientEvent::ApprovalRequired {
            call_id,
            action,
            approval: kind,
            auto_judging,
            grant_rungs,
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
            grant_rungs,
            &vec![
                GrantRung::ExactAction,
                GrantRung::CommandPrefix { tokens: 1 },
                GrantRung::WholeTool,
            ]
        );
        assert_eq!(
            preview.as_ref().and_then(|p| p.get("tool")),
            Some(&serde_json::json!("exec"))
        );

        // ToolCallCompleted carries the action and result projections.
        let completed = frame(&format!(
            r#"{{"seq":5,"event":{{"type":"tool_call_completed","call_id":"{started_call}","status":"completed","action":{{"tool":"exec","command":"git","args":["status"],"cwd":".","files":[]}},"result":{{"tool":"exec","exit_code":0,"timed_out":false,"output_truncated":false,"stdout":"ok","stderr":""}}}}}}"#
        ));
        let ChatFrame::Event(completed) = completed else {
            panic!("expected an event frame: {completed:?}");
        };
        let ClientEvent::ToolCallCompleted {
            status,
            action,
            result,
            ..
        } = &completed.event
        else {
            panic!("expected tool_call_completed: {completed:?}");
        };
        assert_eq!(*status, ToolCallStatus::Completed);
        assert!(action.is_some());
        assert!(result.is_some());

        let done = frame(
            r#"{"seq":6,"event":{"type":"turn_completed","usage":{"input_tokens":1,"output_tokens":2,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        );
        assert_eq!(
            done,
            ChatFrame::Event(SequencedFrame {
                seq: 6,
                event: ClientEvent::TurnCompleted {
                    usage: TurnUsage {
                        input_tokens: 1,
                        output_tokens: 2,
                        cache_read_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                    }
                },
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
