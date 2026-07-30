//! Foreground tools for inspecting sources owned by the current conversation.
//!
//! These tools discover files attached to the current conversation and read a
//! bounded range of one parsed source.

use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{
    citation_authoring_instruction, ApprovalClass, CallId, DocumentId, DocumentProcessingStatus,
    DocumentScope, Result, Store, Tool, ToolCtx, ToolOutput, ToolSpec,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) const LIST_SOURCES_TOOL: &str = "list_sources";
pub(crate) const READ_SOURCE_TOOL: &str = "read_source";
pub(crate) const READ_TOOL_RESULT_TOOL: &str = "read_tool_result";

const MAX_LISTED_SOURCES: u64 = 100;
const DEFAULT_READ_CHARACTERS: usize = 12_000;
const MAX_READ_CHARACTERS: usize = 32_000;

/// List bounded metadata for sources owned by the active conversation.
pub(crate) struct ListSourcesTool {
    store: Arc<dyn Store>,
}

impl ListSourcesTool {
    pub(crate) fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

/// Read a bounded Unicode-character range from one parsed conversation source.
pub(crate) struct ReadSourceTool {
    store: Arc<dyn Store>,
}

impl ReadSourceTool {
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
struct ReadSourceArgs {
    document_id: Uuid,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    max_characters: Option<usize>,
}

#[async_trait]
impl Tool for ListSourcesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: LIST_SOURCES_TOOL.into(),
            description: "List files added as sources to this exact conversation, newest first. \
                          Use this when the user refers to an added or attached file without an \
                          exact document id; then use read_source for direct text. Each source \
                          reports one status: `readable` means read_source can return its text; \
                          `processing` means checking again shortly may change that; `stored_no_text` means the file \
                          is kept and can be named but holds no text to find, so do not claim to \
                          have read it; `failed` means it could not be prepared."
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
        let records = match self
            .store
            .list_document_summaries(
                DocumentScope::Chat(ctx.chat_id),
                None,
                MAX_LISTED_SOURCES + 1,
            )
            .await
        {
            Ok(records) => records,
            Err(_) => {
                return Ok(ToolOutput::error(
                    "could not list this conversation's sources",
                ))
            }
        };
        if records.is_empty() {
            return Ok(
                ToolOutput::text("No sources have been added to this conversation.")
                    // Projected empty rather than not projected: "no sources" and "the
                    // renderer was told nothing" are different facts, and only the card
                    // can tell them apart.
                    .with_entries(Vec::new()),
            );
        }

        let truncated = records.len() > MAX_LISTED_SOURCES as usize;
        // The readiness word is the one thing a reader has to see here: a
        // source that is still processing, or that holds no readable text,
        // is a source the next answer will quietly fail to use.
        let entries = records
            .iter()
            .take(MAX_LISTED_SOURCES as usize)
            .map(|record| {
                openwave_core::ResultEntry::new(
                    openwave_core::ResultEntryKind::Source,
                    record.title.as_deref().unwrap_or("Untitled source"),
                )
                .with_media_type(record.media_type.clone())
                .with_meta(readiness_label(record.readiness().as_str()))
            })
            .collect();
        let visible = records
            .into_iter()
            .take(MAX_LISTED_SOURCES as usize)
            .map(|record| {
                json!({
                    "document_id": record.id,
                    "title": record.title,
                    "media_type": record.media_type,
                    // Deliberately the combined readiness rather than the raw
                    // lifecycle: a source that parsed to nothing is `ready`,
                    // and reporting that would send the agent to search a
                    // document it can never match.
                    "status": record.readiness().as_str(),
                })
            })
            .collect::<Vec<_>>();
        let body = json!({
            "order": "newest_first",
            "sources": visible,
            "truncated": truncated,
        });
        Ok(ToolOutput::text(format!(
            "Sources in this conversation:\n{}",
            serde_json::to_string_pretty(&body).expect("bounded source metadata always serializes")
        ))
        .with_entries(entries))
    }
}

#[async_trait]
impl Tool for ReadSourceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: READ_SOURCE_TOOL.into(),
            description: "Read a bounded, line-numbered text range from one source in this exact \
                          conversation. Use the shown document id and line range in citations."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "format": "uuid",
                        "description": "A document_id returned by list_sources or search."
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
        let args = match serde_json::from_value::<ReadSourceArgs>(args) {
            Ok(args) if !args.document_id.is_nil() => args,
            _ => return Ok(ToolOutput::error("invalid read_source arguments")),
        };
        let max_characters = args.max_characters.unwrap_or(DEFAULT_READ_CHARACTERS);
        if !(1..=MAX_READ_CHARACTERS).contains(&max_characters) {
            return Ok(ToolOutput::error(format!(
                "max_characters must be between 1 and {MAX_READ_CHARACTERS}"
            )));
        }

        let document_id = DocumentId::from(args.document_id);
        let document = match self.store.get_document(document_id).await {
            Ok(Some(document))
                if document.chat_id == Some(ctx.chat_id) && document.project_id.is_none() =>
            {
                document
            }
            Ok(_) => return Ok(ToolOutput::error("source not found in this conversation")),
            Err(_) => return Ok(ToolOutput::error("could not read the source")),
        };

        // Direct reads become useful as soon as canonical parser output exists.
        if document.source_blob.is_some() && document.canonical_fingerprint.is_none() {
            let status = match document.processing_status {
                DocumentProcessingStatus::Failed => "could not be prepared",
                _ => "is still being prepared",
            };
            return Ok(ToolOutput::error(format!("source {status}")));
        }
        if document.canonical_text.is_empty() {
            return Ok(ToolOutput::error(
                "this source has no readable text; use a connected folder for opaque files",
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
            ToolOutput::text(content).with_entries(vec![openwave_core::ResultEntry::new(
                openwave_core::ResultEntryKind::Source,
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
        "processing" => "Processing",
        "stored_no_text" => "No text",
        "failed" => "Failed",
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
    match openwave_core::RendererToolName::from(name) {
        openwave_core::RendererToolName::Other => "a tool",
        _ => "the tool call",
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use openwave_core::{
        ByteSpan, Chat, ChatId, DbStore, DocumentUpsert, ReasoningEffort, SourceLocation,
        SourceRegion, Store, ToolCallRecord, TurnId,
    };
    use serde_json::json;
    use std::num::NonZeroU32;

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
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let canonical_text = "Aé🌊\nGrounded fact";
        let source = DocumentUpsert {
            canonical_fingerprint: None,
            id: DocumentId::new(),
            chat_id: Some(chat.id),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: Some("brief.txt".into()),
            canonical_text: canonical_text.into(),
            source_regions: vec![SourceRegion {
                span: ByteSpan::new(0, canonical_text.len()),
                location: SourceLocation::Page {
                    number: NonZeroU32::new(2).unwrap(),
                },
            }],
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
    async fn a_truncated_result_can_be_read_past_the_cut_but_only_in_its_own_chat() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let other_chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            attachment_revision: 0,
            root_attachments: Vec::new(),
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
                execution: openwave_core::ToolCallExecution::Server,
                status: openwave_core::ToolCallStatus::Pending,
                result: None,
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
                &openwave_core::ToolCallResolution::Completed {
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
        use openwave_core::{DocumentProcessingStatus, SourceReadiness};

        // The case this exists for: the pipeline finished and found nothing.
        assert_eq!(
            SourceReadiness::of(DocumentProcessingStatus::Ready, false).as_str(),
            "stored_no_text"
        );
        assert_eq!(
            SourceReadiness::of(DocumentProcessingStatus::Ready, true).as_str(),
            "readable"
        );
        // Not-yet-readable while queued must stay distinguishable from
        // finished-and-empty, because only one of them is worth waiting on.
        for pending in [
            DocumentProcessingStatus::Queued,
            DocumentProcessingStatus::Processing,
        ] {
            assert_eq!(SourceReadiness::of(pending, false).as_str(), "processing");
        }
        assert_eq!(
            SourceReadiness::of(DocumentProcessingStatus::Failed, false).as_str(),
            "failed"
        );
    }

    #[tokio::test]
    async fn listed_sources_report_readiness_rather_than_the_raw_lifecycle() {
        let (_directory, store, chat, _document_id) = source_fixture().await;
        let context = ToolCtx::without_private_scratch(chat.id, None);

        let listed = ListSourcesTool::new(store.clone())
            .execute(&context, json!({}))
            .await
            .unwrap();
        assert!(!listed.is_error);
        // Canonical text is readable the moment it is upserted, and the
        // agent-facing vocabulary says so without exposing the durable job
        // lifecycle.
        assert!(listed.content.contains("\"status\": \"readable\""));
        assert!(!listed.content.contains("queued"));
        assert!(!listed.content.contains("\"ready\""));
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
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&other_chat).await.unwrap();
        let context = ToolCtx::without_private_scratch(chat.id, None);
        let other_context = ToolCtx::without_private_scratch(other_chat.id, None);

        let listed = ListSourcesTool::new(store.clone())
            .execute(&context, json!({}))
            .await
            .unwrap();
        assert!(!listed.is_error);
        assert!(listed.content.contains("brief.txt"));
        assert!(listed.content.contains(&document_id.to_string()));
        let isolated = ListSourcesTool::new(store.clone())
            .execute(&other_context, json!({}))
            .await
            .unwrap();
        assert_eq!(
            isolated.content,
            "No sources have been added to this conversation."
        );

        let read = ReadSourceTool::new(store.clone())
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

        let denied = ReadSourceTool::new(store)
            .execute(&other_context, json!({ "document_id": document_id }))
            .await
            .unwrap();
        assert!(denied.is_error);
        assert_eq!(denied.content, "source not found in this conversation");
    }
}
