//! Foreground tools for inspecting sources owned by the current conversation.
//!
//! Semantic [`openwave_retrieval::SearchTool`] remains the efficient path for a
//! large corpus. These tools cover the smaller, more direct workflow: discover
//! the files attached to this conversation and read a bounded range of one
//! parsed source without waiting for its embedding stage.

use std::sync::Arc;

use async_trait::async_trait;
use openwave_core::{
    format_source_reference, ApprovalClass, AssistantCitationReference, ByteSpan, ChunkId,
    DocumentId, DocumentProcessingStatus, DocumentScope, Result, RetrievalEvidenceInput,
    RetrievalEvidenceSource, Store, Tool, ToolCtx, ToolOutput, ToolSpec,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub(crate) const LIST_SOURCES_TOOL: &str = "list_sources";
pub(crate) const READ_SOURCE_TOOL: &str = "read_source";

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
                          exact document id; then use read_source for direct text or search for \
                          relevant passages. Each source reports one status: `searchable` means \
                          search and read_source can find its text; `processing` means checking \
                          again shortly may change that; `stored_not_searchable` means the file \
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
            return Ok(ToolOutput::text(
                "No sources have been added to this conversation.",
            ));
        }

        let truncated = records.len() > MAX_LISTED_SOURCES as usize;
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
        )))
    }
}

#[async_trait]
impl Tool for ReadSourceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: READ_SOURCE_TOOL.into(),
            description: "Read a bounded text range from one source in this exact conversation. \
                          The result includes an opaque reference that can be copied into the \
                          answer to create a grounded citation."
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

        // Parsing and indexing are separate jobs. Direct reads become useful as
        // soon as canonical parser output exists, even while embedding is still
        // running, so a small source does not inherit the full RAG wait.
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
            RetrievalEvidenceInput::MAX_SNIPPET_BYTES,
        ) else {
            return Ok(ToolOutput::error("offset is past the end of the source"));
        };
        if window.text.contains('\0') {
            return Ok(ToolOutput::error(
                "this source range contains unsupported text",
            ));
        }

        let source_token = Uuid::new_v4();
        let source_reference = format_source_reference(AssistantCitationReference { source_token });
        let title = document
            .title
            .as_deref()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or("Untitled source");
        let content = format!(
            "Source: {title}\nDocument ID: {document_id}\nCharacters: {}..{} of {}\n\
             To cite this range, copy this source reference exactly into the answer: \
             {source_reference}\n\n{}",
            window.start_character, window.end_character, window.total_characters, window.text
        );
        let span = ByteSpan::new(window.start_byte, window.end_byte);
        let source_regions = document
            .source_regions
            .iter()
            .filter_map(|region| {
                let start = region.span.start.max(span.start);
                let end = region.span.end.min(span.end);
                (start < end).then(|| openwave_core::SourceRegion {
                    span: ByteSpan::new(start, end),
                    location: region.location.clone(),
                })
            })
            .collect();
        let generation = document.generation();
        let source = match document.source_uri.as_deref() {
            Some(uri)
                if !uri.is_empty()
                    && uri.len() <= RetrievalEvidenceInput::MAX_SOURCE_URI_BYTES
                    && !uri.contains('\0') =>
            {
                RetrievalEvidenceSource::Uri {
                    uri: uri.to_owned(),
                }
            }
            _ => RetrievalEvidenceSource::Inline,
        };
        let evidence = RetrievalEvidenceInput {
            rank: 1,
            source_token,
            document_id,
            generation,
            chunk_id: ChunkId::derive(document_id, span.start, span.end),
            span,
            snippet: window.text.to_owned(),
            heading_path: Vec::new(),
            source_regions,
            source,
        };
        Ok(ToolOutput::text(content).with_private_evidence(vec![evidence]))
    }
}

struct SourceWindow<'a> {
    start_character: usize,
    end_character: usize,
    total_characters: usize,
    start_byte: usize,
    end_byte: usize,
    text: &'a str,
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
        end_byte,
        text: &text[start_byte..end_byte],
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use openwave_core::{
        Chat, ChatId, DbStore, DocumentUpsert, ReasoningEffort, SourceLocation, SourceRegion, Store,
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
            attachment_revision: 0,
            root_attachments: Vec::new(),
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let canonical_text = "Aé🌊\nGrounded fact";
        let source = DocumentUpsert {
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
        let (document, _) = store
            .upsert_document_and_enqueue_index(&source, "test-pipeline", 3)
            .await
            .unwrap();
        (directory, store, chat, document.id)
    }

    #[test]
    fn source_window_uses_character_offsets_and_byte_budgets() {
        let window = source_window("Aé🌊Z", 1, 3, 6).unwrap();
        assert_eq!(window.text, "é🌊");
        assert_eq!(window.start_character, 1);
        assert_eq!(window.end_character, 3);
        assert_eq!(window.start_byte, 1);
        assert_eq!(window.end_byte, 7);
        assert_eq!(window.total_characters, 4);
        assert!(source_window("short", 5, 1, 8).is_none());
    }

    #[test]
    fn readiness_never_reports_an_unsearchable_source_as_usable() {
        use openwave_core::{DocumentProcessingStatus, SourceReadiness};

        // The case this exists for: the pipeline finished and found nothing.
        assert_eq!(
            SourceReadiness::of(DocumentProcessingStatus::Ready, false).as_str(),
            "stored_not_searchable"
        );
        assert_eq!(
            SourceReadiness::of(DocumentProcessingStatus::Ready, true).as_str(),
            "searchable"
        );
        // Not-yet-searchable while queued must stay distinguishable from
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
        // Freshly upserted sources are still queued, and the agent-facing
        // vocabulary says so without exposing the durable job lifecycle.
        assert!(listed.content.contains("\"status\": \"processing\""));
        assert!(!listed.content.contains("queued"));
        assert!(!listed.content.contains("\"ready\""));
    }

    #[tokio::test]
    async fn tools_are_exactly_conversation_scoped_and_reads_emit_evidence() {
        let (_directory, store, chat, document_id) = source_fixture().await;
        let other_chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
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
        assert!(read.content.contains("é🌊\n"));
        assert_eq!(read.private_evidence.len(), 1);
        let evidence = &read.private_evidence[0];
        assert_eq!(evidence.document_id, document_id);
        assert_eq!(evidence.snippet, "é🌊\n");
        assert_eq!(evidence.span, ByteSpan::new(1, 8));
        assert_eq!(
            evidence.source_regions,
            [SourceRegion {
                span: ByteSpan::new(1, 8),
                location: SourceLocation::Page {
                    number: NonZeroU32::new(2).unwrap(),
                },
            }]
        );
        assert!(read
            .content
            .contains(&format_source_reference(AssistantCitationReference {
                source_token: evidence.source_token,
            })));

        let denied = ReadSourceTool::new(store)
            .execute(&other_context, json!({ "document_id": document_id }))
            .await
            .unwrap();
        assert!(denied.is_error);
        assert_eq!(denied.content, "source not found in this conversation");
    }
}
