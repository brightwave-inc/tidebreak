//! Foreground tools for inspecting sources the current conversation can read.
//!
//! These tools discover files attached to the current conversation, together
//! with the files held by the project it belongs to, and read a bounded range
//! of one parsed source.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tidebreak_core::{
    citation_authoring_instruction, ApprovalClass, CallId, DocumentId, DocumentScope,
    DocumentSummaryRecord, Result, Store, Tool, ToolCtx, ToolOutput, ToolSpec,
};
use uuid::Uuid;

pub(crate) const LIST_DOCUMENTS_TOOL: &str = "list_documents";
pub(crate) const READ_DOCUMENT_TOOL: &str = "read_document";
pub(crate) const READ_TOOL_RESULT_TOOL: &str = "read_tool_result";

/// Listed per scope rather than over the union, so a project holding hundreds
/// of files cannot crowd a conversation's own three out of the listing.
const MAX_LISTED_DOCUMENTS: u64 = 100;
const DEFAULT_READ_CHARACTERS: usize = 12_000;
const MAX_READ_CHARACTERS: usize = 32_000;

/// List bounded metadata for the sources the active conversation can read.
pub(crate) struct ListDocumentsTool {
    store: Arc<dyn Store>,
}

impl ListDocumentsTool {
    pub(crate) fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    /// One scope's newest sources, and whether the scope held more.
    async fn listed(&self, scope: DocumentScope) -> Result<(Vec<DocumentSummaryRecord>, bool)> {
        let mut records = self
            .store
            .list_document_summaries(scope, None, MAX_LISTED_DOCUMENTS + 1)
            .await?;
        let truncated = records.len() > MAX_LISTED_DOCUMENTS as usize;
        records.truncate(MAX_LISTED_DOCUMENTS as usize);
        Ok((records, truncated))
    }
}

/// Read a bounded Unicode-character range from one parsed conversation source.
pub(crate) struct ReadDocumentTool {
    store: Arc<dyn Store>,
}

impl ReadDocumentTool {
    pub(crate) fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

/// Read past the point a large tool result was cut for the model.
///
/// A result is bounded twice: the record keeps what it may, and one turn is fed
/// only what it can afford to read. Without this the remainder was reachable by
/// nobody, which made the cut a dead end rather than a bookmark.
pub(crate) struct ReadToolResultTool {
    store: Arc<dyn Store>,
}

impl ReadToolResultTool {
    pub(crate) fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadToolResultArgs {
    call_id: Uuid,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    max_characters: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadDocumentArgs {
    document_id: Uuid,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    max_characters: Option<usize>,
}

#[async_trait]
impl Tool for ListDocumentsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: LIST_DOCUMENTS_TOOL.into(),
            description: "List files this conversation can read, newest first. These are the \
                          files added to this exact conversation, plus the files held by the \
                          project it belongs to, which every conversation in that project \
                          shares. Use this when the user refers to an added or attached file \
                          without an exact document id; then use read_document for direct text. \
                          Each source reports one status: `readable` means read_document can \
                          return its text; `stored_no_text` means the file is kept and can be \
                          named but holds no text to find, so do not claim to have read it."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        if serde_json::from_value::<EmptyArgs>(args).is_err() {
            return Ok(ToolOutput::error(
                "invalid arguments: expected an empty object",
            ));
        }
        let (conversation, conversation_truncated) =
            match self.listed(DocumentScope::Chat(ctx.chat_id)).await {
                Ok(listed) => listed,
                Err(_) => {
                    return Ok(ToolOutput::error(
                        "could not list this conversation's sources",
                    ))
                }
            };
        // Resolved now rather than snapshotted when the conversation was
        // created: a file added to the project today is meant to reach the
        // conversation that has been running since yesterday.
        let (project, project_truncated) = match ctx.project_id {
            Some(project_id) => match self.listed(DocumentScope::Project(project_id)).await {
                Ok(listed) => listed,
                Err(_) => return Ok(ToolOutput::error("could not list this project's files")),
            },
            None => (Vec::new(), false),
        };

        if conversation.is_empty() && project.is_empty() {
            let message = if ctx.project_id.is_some() {
                "No files have been added to this conversation or to its project."
            } else {
                "No sources have been added to this conversation."
            };
            return Ok(ToolOutput::text(message)
                // Projected empty rather than not projected: "no sources" and "the
                // renderer was told nothing" are different facts, and only the card
                // can tell them apart.
                .with_entries(Vec::new()));
        }

        // The readiness word is the one thing a reader has to see here: a
        // source that holds no readable text is a source the next answer will
        // quietly fail to use.
        //
        // The conversation's own files come first in both projections. A busy
        // project should not push the file the user just attached to the
        // bottom of the list.
        let entries = conversation
            .iter()
            .map(|record| entry(record, SourceOrigin::Conversation))
            .chain(
                project
                    .iter()
                    .map(|record| entry(record, SourceOrigin::Project)),
            )
            .collect();
        let visible = conversation
            .iter()
            .map(|record| row(record, SourceOrigin::Conversation))
            .chain(
                project
                    .iter()
                    .map(|record| row(record, SourceOrigin::Project)),
            )
            .collect::<Vec<_>>();
        let body = json!({
            "order": "newest_first",
            "sources": visible,
            "truncated": conversation_truncated || project_truncated,
        });
        Ok(ToolOutput::text(format!(
            "Sources this conversation can read:\n{}",
            serde_json::to_string_pretty(&body).expect("bounded source metadata always serializes")
        ))
        .with_entries(entries))
    }
}

/// Where a listed source came from, which is the one thing the model has to
/// know beyond its text: a project file is shared with sibling conversations,
/// so what is said about it is said in front of all of them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceOrigin {
    Conversation,
    Project,
}

impl SourceOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::Project => "project",
        }
    }
}

fn row(record: &DocumentSummaryRecord, origin: SourceOrigin) -> Value {
    json!({
        "document_id": record.id,
        "title": record.title,
        "media_type": record.media_type,
        "scope": origin.as_str(),
        // Deliberately the combined readiness rather than the raw
        // lifecycle: a source that parsed to nothing is `ready`,
        // and reporting that would send the agent to search a
        // document it can never match.
        "status": record.readiness().as_str(),
    })
}

fn entry(record: &DocumentSummaryRecord, origin: SourceOrigin) -> tidebreak_core::ResultEntry {
    let readiness = readiness_label(record.readiness().as_str());
    let meta = match origin {
        SourceOrigin::Conversation => readiness.to_string(),
        SourceOrigin::Project => format!("Project file · {readiness}"),
    };
    tidebreak_core::ResultEntry::new(
        tidebreak_core::ResultEntryKind::Source,
        record.title.as_deref().unwrap_or("Untitled source"),
    )
    .with_media_type(record.media_type.clone())
    .with_meta(meta)
}

#[async_trait]
impl Tool for ReadDocumentTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: READ_DOCUMENT_TOOL.into(),
            description: "Read a bounded, line-numbered text range from one source this \
                          conversation can read — either a file added to this conversation or \
                          a file held by its project. Use the shown document id and line range \
                          in citations."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "A document_id returned by list_documents or search."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Zero-based Unicode character offset (default 0)."
                    },
                    "max_characters": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_CHARACTERS,
                        "description": "Maximum Unicode characters to return (default 12000)."
                    }
                },
                "required": ["document_id"],
                "additionalProperties": false
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args = match serde_json::from_value::<ReadDocumentArgs>(args) {
            Ok(args) if !args.document_id.is_nil() => args,
            _ => return Ok(ToolOutput::error("invalid read_document arguments")),
        };
        let max_characters = args.max_characters.unwrap_or(DEFAULT_READ_CHARACTERS);
        if !(1..=MAX_READ_CHARACTERS).contains(&max_characters) {
            return Ok(ToolOutput::error(format!(
                "max_characters must be between 1 and {MAX_READ_CHARACTERS}"
            )));
        }

        let document_id = DocumentId::from(args.document_id);
        let document = match self.store.get_document(document_id).await {
            // Owned by this conversation, or held by the project this
            // conversation belongs to. Ownership is exclusive, so exactly one
            // of these can hold for any document.
            Ok(Some(document))
                if (document.chat_id == Some(ctx.chat_id) && document.project_id.is_none())
                    || (document.project_id.is_some() && document.project_id == ctx.project_id) =>
            {
                document
            }
            Ok(_) => return Ok(ToolOutput::error("source not found in this conversation")),
            Err(_) => return Ok(ToolOutput::error("could not read the source")),
        };

        if document.canonical_text.is_empty() {
            return Ok(ToolOutput::error(
                "this source has no readable text; attached raw bytes are available under \
                 documents/ in the exec workspace when they fit the workspace file limit",
            ));
        }

        let Some(window) = source_window(
            &document.canonical_text,
            args.offset,
            max_characters,
            MAX_READ_CHARACTERS * 4,
        ) else {
            return Ok(ToolOutput::error("offset is past the end of the source"));
        };
        if window.text.contains('\0') {
            return Ok(ToolOutput::error(
                "this source range contains unsupported text",
            ));
        }

        let title = document
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Untitled source");
        let start_line = document.canonical_text[..window.start_byte]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1;
        let numbered = number_source_lines(window.text, start_line);
        let end_line = start_line + window.text.bytes().filter(|byte| *byte == b'\n').count();
        let content = format!(
            "Source: {title}\nDocument ID: {document_id}\nCharacters: {}..{} of {}\n\
             Lines: {start_line}-{end_line}\n{}\n\n{}",
            window.start_character,
            window.end_character,
            window.total_characters,
            citation_authoring_instruction(document_id),
            numbered
        );
        Ok(
            ToolOutput::text(content).with_entries(vec![tidebreak_core::ResultEntry::new(
                tidebreak_core::ResultEntryKind::Source,
                title,
            )
            .with_media_type(document.media_type.clone())
            // Which slice of the source was read, because a source read in
            // twelve-thousand-character windows produces twelve identical rail
            // lines and the range is the only thing telling them apart.
            .with_meta(format!(
                "characters {}–{} of {}",
                window.start_character, window.end_character, window.total_characters
            ))]),
        )
    }
}

/// The readiness word a source row shows.
///
/// Mapped rather than passed through: the tool's vocabulary is written for the
/// model and says so — `stored_no_text` is a contract term, not
/// something to put in front of a reader.
fn readiness_label(readiness: &str) -> &'static str {
    match readiness {
        "readable" => "Readable",
        "stored_no_text" => "No text",
        _ => "Unknown",
    }
}

struct SourceWindow<'a> {
    start_character: usize,
    end_character: usize,
    total_characters: usize,
    start_byte: usize,
    text: &'a str,
}

fn number_source_lines(text: &str, start_line: usize) -> String {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| format!("{:>6} | {line}", start_line + index))
        .collect()
}

fn source_window(
    text: &str,
    offset: usize,
    max_characters: usize,
    max_bytes: usize,
) -> Option<SourceWindow<'_>> {
    let total_characters = text.chars().count();
    if offset >= total_characters {
        return None;
    }
    let start_byte = text
        .char_indices()
        .nth(offset)
        .map_or(text.len(), |(index, _)| index);
    let remaining = &text[start_byte..];
    let mut characters = 0;
    let mut bytes = 0;
    for character in remaining.chars() {
        if characters == max_characters || bytes + character.len_utf8() > max_bytes {
            break;
        }
        characters += 1;
        bytes += character.len_utf8();
    }
    let end_byte = start_byte + bytes;
    Some(SourceWindow {
        start_character: offset,
        end_character: offset + characters,
        total_characters,
        start_byte,
        text: &text[start_byte..end_byte],
    })
}

#[async_trait]
impl Tool for ReadToolResultTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: READ_TOOL_RESULT_TOOL.into(),
            description: "Read more of a tool result that was cut short for length. \
                          Use the call_id named in a `[truncated: …]` notice, and \
                          `offset` to continue past what you already read. Only calls \
                          from this exact conversation are readable."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "call_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "The call_id from the truncation notice."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Zero-based Unicode character offset (default 0)."
                    },
                    "max_characters": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_READ_CHARACTERS,
                        "description": "Maximum Unicode characters to return (default 12000)."
                    }
                },
                "required": ["call_id"],
                "additionalProperties": false
            }),
        }
    }

    fn approval_class(&self) -> ApprovalClass {
        // The bytes were already produced in this conversation and already
        // partly shown; reading further is not a new capability.
        ApprovalClass::ReadOnly
    }

    async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let args = match serde_json::from_value::<ReadToolResultArgs>(args) {
            Ok(args) if !args.call_id.is_nil() => args,
            _ => return Ok(ToolOutput::error("invalid read_tool_result arguments")),
        };
        let max_characters = args.max_characters.unwrap_or(DEFAULT_READ_CHARACTERS);
        if !(1..=MAX_READ_CHARACTERS).contains(&max_characters) {
            return Ok(ToolOutput::error(format!(
                "max_characters must be between 1 and {MAX_READ_CHARACTERS}"
            )));
        }

        // Scoped by listing this conversation's calls: a call id from another
        // chat is simply not found, so the tool cannot reach across chats even
        // if the model guesses an id.
        let calls = match self.store.list_tool_calls(ctx.chat_id).await {
            Ok(calls) => calls,
            Err(_) => return Ok(ToolOutput::error("could not read that tool result")),
        };
        let Some(call) = calls
            .into_iter()
            .find(|call| call.id == CallId::from(args.call_id))
        else {
            return Ok(ToolOutput::error(
                "no tool call with that call_id in this conversation",
            ));
        };
        let Some(result) = call.result.filter(|result| !result.is_empty()) else {
            return Ok(ToolOutput::error("that tool call recorded no result"));
        };

        let Some(window) = source_window(
            &result,
            args.offset,
            max_characters,
            MAX_READ_CHARACTERS * 4,
        ) else {
            return Ok(ToolOutput::error("offset is past the end of the result"));
        };
        let remaining = window.total_characters.saturating_sub(window.end_character);
        let mut content = format!(
            "Result of {} (characters {}\u{2013}{} of {}):\n{}",
            historical_safe_name(&call.name),
            window.start_character,
            window.end_character,
            window.total_characters,
            window.text
        );
        if remaining > 0 {
            content.push_str(&format!(
                "\n\n[{remaining} characters remain; continue with offset {}]",
                window.end_character
            ));
        }
        Ok(ToolOutput::text(content))
    }
}

/// The tool's own name is model-supplied in principle, so a result header uses
/// the same closed vocabulary the rest of the renderer boundary does.
fn historical_safe_name(name: &str) -> &'static str {
    match tidebreak_core::RendererToolName::from(name) {
        tidebreak_core::RendererToolName::Other => "a tool",
        _ => "the tool call",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use tidebreak_core::{
        Chat, ChatId, DbStore, DocumentUpsert, ReasoningEffort, Store, ToolCallRecord, TurnId,
    };

    use super::*;

    async fn source_fixture() -> (tempfile::TempDir, Arc<DbStore>, Chat, DocumentId) {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("source-tools.db").display()
            ))
            .await
            .unwrap(),
        );
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("Workspace".into()),
            model: None,
            reasoning_effort: None::<ReasoningEffort>,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            chat_id: Some(chat.id),
            project_id: None,
            origin_uri: None,
            media_type: "text/plain".into(),
            title: Some("brief.txt".into()),
            canonical_text: "Aé🌊\nGrounded fact".into(),
            updated_at: Utc::now(),
        };
        let document = store.upsert_document(&source).await.unwrap();
        (directory, store, chat, document.id)
    }

    #[test]
    fn source_window_uses_character_offsets_and_byte_budgets() {
        let window = source_window("Aé🌊Z", 1, 3, 6).unwrap();
        assert_eq!(window.text, "é🌊");
        assert_eq!(window.start_character, 1);
        assert_eq!(window.end_character, 3);
        assert_eq!(window.start_byte, 1);
        assert_eq!(window.total_characters, 4);
        assert!(source_window("short", 5, 1, 8).is_none());
    }

    #[tokio::test]
    async fn an_opaque_attachment_points_read_document_at_exec_documents() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let opaque = store
            .upsert_document(&DocumentUpsert {
                id: DocumentId::new(),
                chat_id: Some(chat.id),
                project_id: None,
                origin_uri: None,
                media_type: "application/pdf".into(),
                title: Some("opaque.pdf".into()),
                canonical_text: String::new(),
                updated_at: Utc::now(),
            })
            .await
            .unwrap();
        let output = ReadDocumentTool::new(store)
            .execute(
                &ToolCtx::without_private_scratch(chat.id, None),
                json!({ "document_id": opaque.id }),
            )
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(output.content.contains("documents/"));
        assert!(!output.content.contains("connected folder"));
    }

    #[tokio::test]
    async fn a_truncated_result_can_be_read_past_the_cut_but_only_in_its_own_chat() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let other_chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&other_chat).await.unwrap();
        let turn = TurnId::new();
        store
            .accept_turn(turn, chat.id, "gpt-5", "run something long")
            .await
            .unwrap();
        let call_id = CallId::new();
        let full: String = (0..2_000)
            .map(|index| char::from(b'a' + (index % 26) as u8))
            .collect();
        store
            .accept_tool_call(&ToolCallRecord {
                id: call_id,
                chat_id: chat.id,
                turn_id: turn,
                provider_id: "call-1".into(),
                name: "exec".into(),
                arguments: json!({ "command": "cargo" }),
                raw_arguments: None,
                execution: tidebreak_core::ToolCallExecution::Server,
                status: tidebreak_core::ToolCallStatus::Pending,
                result: None,
                result_preview: None,

                provider_replay: None,
                error_code: None,
                error_detail: None,
                client_executor_id: None,
                client_lease_expires_at: None,
                created_at: Utc::now(),
                resolved_at: None,
            })
            .await
            .unwrap();
        // Resolved through the ordinary path, so the record holds what the real
        // flow would have written.
        store
            .resolve_server_tool_call(
                call_id,
                &tidebreak_core::ToolCallResolution::Completed {
                    result: full.clone(),
                },
                Utc::now(),
            )
            .await
            .unwrap();

        let tool = ReadToolResultTool::new(store.clone());
        let context = ToolCtx::without_private_scratch(chat.id, None);

        // A window from the start reports how much is left and where to resume.
        let head = tool
            .execute(
                &context,
                json!({ "call_id": call_id.0, "max_characters": 500 }),
            )
            .await
            .unwrap();
        assert!(!head.is_error, "{}", head.content);
        assert!(head.content.contains(&full[..100]));
        assert!(head
            .content
            .contains("characters remain; continue with offset 500"));

        // And the tail is genuinely reachable, which is the whole point: the cut
        // is a bookmark rather than a loss.
        let tail = tool
            .execute(&context, json!({ "call_id": call_id.0, "offset": 1_900 }))
            .await
            .unwrap();
        assert!(!tail.is_error);
        assert!(tail.content.contains(&full[1_900..]));

        // Scoping is by construction: another conversation cannot reach this
        // call even holding its exact id.
        let intruder = tool
            .execute(
                &ToolCtx::without_private_scratch(other_chat.id, None),
                json!({ "call_id": call_id.0 }),
            )
            .await
            .unwrap();
        assert!(intruder.is_error);
        assert!(!intruder.content.contains(&full[..100]));

        // Bad addresses fail as errors the model can act on, not as panics.
        for arguments in [
            json!({ "call_id": Uuid::nil() }),
            json!({ "call_id": Uuid::new_v4() }),
            json!({ "call_id": call_id.0, "offset": 99_999 }),
            json!({ "call_id": call_id.0, "max_characters": 0 }),
        ] {
            assert!(tool.execute(&context, arguments).await.unwrap().is_error);
        }
    }

    #[test]
    fn readiness_never_reports_an_unreadable_source_as_usable() {
        use tidebreak_core::DocumentReadiness;

        assert_eq!(DocumentReadiness::of(false).as_str(), "stored_no_text");
        assert_eq!(DocumentReadiness::of(true).as_str(), "readable");
    }

    #[tokio::test]
    async fn listed_sources_report_readiness_rather_than_the_raw_lifecycle() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let context = ToolCtx::without_private_scratch(chat.id, None);

        let listed = ListDocumentsTool::new(store.clone())
            .execute(&context, json!({}))
            .await
            .unwrap();
        assert!(!listed.is_error);
        // Canonical text is readable the moment it is upserted.
        assert!(listed.content.contains("\"status\": \"readable\""));
        assert!(!listed.content.contains("queued"));
        assert!(!listed.content.contains("\"ready\""));
    }

    #[tokio::test]
    async fn a_project_file_reaches_a_conversation_that_predates_it() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let project = tidebreak_core::Project {
            id: tidebreak_core::ProjectId::new(),
            title: Some("Q3".into()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_project(&project).await.unwrap();
        let elsewhere = tidebreak_core::Project {
            id: tidebreak_core::ProjectId::new(),
            title: Some("Q4".into()),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_project(&elsewhere).await.unwrap();

        // Added to the project *after* the conversation already exists, which
        // is the case a creation-time snapshot would get wrong.
        let shared = store
            .upsert_document(&DocumentUpsert {
                id: DocumentId::new(),
                chat_id: None,
                project_id: Some(project.id),
                origin_uri: None,
                media_type: "text/plain".into(),
                title: Some("handbook.txt".into()),
                canonical_text: "Shared across the project".into(),
                updated_at: Utc::now(),
            })
            .await
            .unwrap();

        let context = ToolCtx::without_private_scratch(chat.id, Some(project.id));
        let listed = ListDocumentsTool::new(store.clone())
            .execute(&context, json!({}))
            .await
            .unwrap();
        assert!(!listed.is_error, "{}", listed.content);
        assert!(listed.content.contains("handbook.txt"));
        assert!(listed.content.contains("\"scope\": \"project\""));
        // The conversation's own file is still listed, and still first: a
        // project's files must not displace what the user just attached.
        let own = listed.content.find("brief.txt").expect("own file listed");
        assert!(own < listed.content.find("handbook.txt").unwrap());

        let read = ReadDocumentTool::new(store.clone())
            .execute(&context, json!({ "document_id": shared.id }))
            .await
            .unwrap();
        assert!(!read.is_error, "{}", read.content);
        assert!(read.content.contains("Shared across the project"));

        // Sharing reaches exactly the project holding the file. A conversation
        // in another project, or in none, gets nothing — holding the id does
        // not help it.
        for outsider in [Some(elsewhere.id), None] {
            let denied = ReadDocumentTool::new(store.clone())
                .execute(
                    &ToolCtx::without_private_scratch(chat.id, outsider),
                    json!({ "document_id": shared.id }),
                )
                .await
                .unwrap();
            assert!(denied.is_error);
            assert!(!denied.content.contains("Shared across the project"));
        }
    }

    #[tokio::test]
    async fn tools_are_exactly_conversation_scoped_and_reads_show_line_locators() {
        let (_directory, store, chat, document_id) = source_fixture().await;
        let other_chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: Default::default(),
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&other_chat).await.unwrap();
        let context = ToolCtx::without_private_scratch(chat.id, None);
        let other_context = ToolCtx::without_private_scratch(other_chat.id, None);

        let listed = ListDocumentsTool::new(store.clone())
            .execute(&context, json!({}))
            .await
            .unwrap();
        assert!(!listed.is_error);
        assert!(listed.content.contains("brief.txt"));
        assert!(listed.content.contains(&document_id.to_string()));
        let isolated = ListDocumentsTool::new(store.clone())
            .execute(&other_context, json!({}))
            .await
            .unwrap();
        assert_eq!(
            isolated.content,
            "No sources have been added to this conversation."
        );

        let read = ReadDocumentTool::new(store.clone())
            .execute(
                &context,
                json!({
                    "document_id": document_id,
                    "offset": 1,
                    "max_characters": 3,
                }),
            )
            .await
            .unwrap();
        assert!(!read.is_error);
        assert!(read.content.contains("     1 | é🌊\n"));
        assert!(read
            .content
            .contains(&format!("doc={document_id} lines=N-M")));

        let denied = ReadDocumentTool::new(store)
            .execute(&other_context, json!({ "document_id": document_id }))
            .await
            .unwrap();
        assert!(denied.is_error);
        assert_eq!(denied.content, "source not found in this conversation");
    }
}
