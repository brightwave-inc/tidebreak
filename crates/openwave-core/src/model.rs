//! The persisted conversation model.
//!
//! Mirrors the `chat` and `message` tables of `Store` schema v1. A
//! [`Chat`] is a durable conversation that owns a workspace directory; a
//! [`Message`] is one user input or assistant answer within it.
//!
//! Turns and steps are runtime concepts of the agent loop (schema v1 has no
//! table for them — they are referenced by `turn_id`), so they live with the
//! loop, not here.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use uuid::Uuid;

/// A half-open UTF-8 byte range `[start, end)` in canonical document text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteSpan {
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

impl ByteSpan {
    /// Construct a byte span.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    /// Number of bytes covered by the span.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span covers no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Format-specific location in the original source represented by canonical text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceLocation {
    /// One page in a paginated source. Page numbers are one-based.
    Page {
        /// One-based page number.
        number: NonZeroU32,
    },
}

/// Mapping from canonical text back to a location in the original source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegion {
    /// Document-global canonical-text span represented by this region.
    pub span: ByteSpan,
    /// Original source location for the span.
    pub location: SourceLocation,
}

/// Validate parser-produced source regions against their canonical text.
///
/// Regions must be ordered, nonempty, nonoverlapping, in bounds, and aligned to
/// UTF-8 boundaries. Gaps are valid for parser-inserted separators.
pub fn validate_source_regions(
    text: &str,
    regions: &[SourceRegion],
) -> std::result::Result<(), &'static str> {
    let mut previous_end = 0;
    for region in regions {
        if region.span.is_empty() {
            return Err("source regions must be nonempty");
        }
        if region.span.end > text.len() {
            return Err("source region falls outside canonical text");
        }
        if !text.is_char_boundary(region.span.start) || !text.is_char_boundary(region.span.end) {
            return Err("source region offsets must be UTF-8 character boundaries");
        }
        if region.span.start < previous_end {
            return Err("source regions must be ordered and nonoverlapping");
        }
        previous_end = region.span.end;
    }
    Ok(())
}

use crate::id::{ChatId, DocumentId, DocumentJobId, MessageId, ProjectId, TurnId};

/// An optional grouping of chats that share a workspace and (later) a document
/// corpus. A chat may belong to a project or stand alone — unlike some designs
/// that make a project mandatory, OpenWave keeps loose, projectless chats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// Human-facing title.
    pub title: Option<String>,
    /// Absolute path to the project's workspace/corpus root.
    pub workspace_dir: PathBuf,
    /// When the project was created.
    pub created_at: DateTime<Utc>,
}

/// User-visible lifecycle of the current authoritative document revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentProcessingStatus {
    /// Durable source exists and awaits processing or retry.
    Queued,
    /// A worker owns the current processing job.
    Processing,
    /// The current revision is fully represented in the derived index.
    Ready,
    /// Processing exhausted retries or hit a permanent failure.
    Failed,
}

impl DocumentProcessingStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// Semantic stage performed by a durable document job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentJobKind {
    /// Chunk and embed canonical content into the derived retrieval index.
    Index,
}

impl DocumentJobKind {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Index => "index",
        }
    }
}

/// Durable delivery state of one document-processing job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentJobStatus {
    /// Eligible to be claimed at `available_at`.
    Queued,
    /// Currently owned by the exact lease token and expiry on the job.
    Running,
    /// Failed transiently and becomes claimable again at `available_at`.
    RetryWait,
    /// Completed successfully.
    Succeeded,
    /// Exhausted retries or failed permanently.
    Failed,
    /// Superseded or explicitly cancelled.
    Cancelled,
}

impl DocumentJobStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::RetryWait => "retry_wait",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no worker may claim this job again without an explicit retry.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Exact, monotonically ordered source generation for one stable document id.
///
/// The revision clock survives hard source deletion, while the token identifies
/// one exact revision and prevents equal-revision corruption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentGeneration {
    /// Monotonic revision for this document id, including delete tombstones.
    pub content_revision: i64,
    /// Opaque identity for this exact generation.
    pub revision_token: Uuid,
}

/// An authoritative source document whose derived chunks live in the retrieval
/// index. Canonical text stays in the operational store so an index can be
/// rebuilt after an embedding or chunking change. Reprocessing with a different
/// parser additionally requires the original bytes, which belong in `BlobStore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRecord {
    /// Stable identifier shared with the retrieval index.
    pub id: DocumentId,
    /// Owning project, or `None` for an explicitly unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub source_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Parsed text-of-record used to rechunk, re-embed, and verify citations.
    pub canonical_text: String,
    /// Parser-produced mappings from canonical text to original source pages.
    pub source_regions: Vec<SourceRegion>,
    /// Monotonic content revision, starting at one and continuing through hard
    /// delete tombstones and later recreation of this document id.
    pub content_revision: i64,
    /// Opaque identity for this exact content revision.
    ///
    /// Paired with the integer clock as exact identity so equal-revision
    /// corruption cannot be mistaken for the same generation.
    pub revision_token: Uuid,
    /// Processing lifecycle of the current authoritative revision.
    pub processing_status: DocumentProcessingStatus,
    /// Revision currently represented in the retrieval index, if any.
    pub indexed_revision: Option<i64>,
    /// Chunker/embedder fingerprint for the indexed revision.
    ///
    /// Parser provenance is separate because reparsing requires original source
    /// bytes; canonical text alone can only be rechunked and re-embedded.
    pub index_fingerprint: Option<String>,
    /// When this record was first created.
    pub created_at: DateTime<Utc>,
    /// When authoritative content or metadata last changed.
    pub updated_at: DateTime<Utc>,
    /// When the current index watermark was recorded.
    pub indexed_at: Option<DateTime<Utc>>,
}

impl DocumentRecord {
    /// Exact generation represented by this live source record.
    #[must_use]
    pub const fn generation(&self) -> DocumentGeneration {
        DocumentGeneration {
            content_revision: self.content_revision,
            revision_token: self.revision_token,
        }
    }
}

/// One durable semantic processing stage bound to an exact document revision.
///
/// Expensive work happens outside the operational database transaction. Every
/// operational-state mutation must therefore present `lease_token` and still
/// match the job's `(document_id, content_revision, revision_token)`. This fences
/// stale database completion; derived stores such as the vector index also need
/// generation-aware publication before multi-worker execution is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentJob {
    /// Stable job identity.
    pub id: DocumentJobId,
    /// Authoritative document this job processes.
    pub document_id: DocumentId,
    /// Exact monotonic source revision claimed by this job.
    pub content_revision: i64,
    /// Exact lifecycle identity; prevents delete/recreate ABA completion in the
    /// operational store and identifies the generation derived stores must fence.
    pub revision_token: Uuid,
    /// Semantic pipeline stage.
    pub kind: DocumentJobKind,
    /// Durable delivery state.
    pub status: DocumentJobStatus,
    /// Identity of the parser/chunker/embedder configuration for this stage.
    pub pipeline_fingerprint: String,
    /// Claims already made, including the current claim when running.
    pub attempt_count: i32,
    /// Maximum claims before a retryable error becomes terminal.
    pub max_attempts: i32,
    /// Earliest time a queued/retry-wait job may be claimed.
    pub available_at: DateTime<Utc>,
    /// Exact claim identity required for heartbeat/completion writes.
    pub lease_token: Option<Uuid>,
    /// When the current claim becomes recoverably stale.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the first claim began.
    pub started_at: Option<DateTime<Utc>>,
    /// When this job entered a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub last_error_code: Option<String>,
    /// Bounded diagnostic detail for local operators.
    pub last_error_detail: Option<String>,
    /// When this semantic job was created.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl DocumentJob {
    /// Exact source generation this job is allowed to process.
    #[must_use]
    pub const fn generation(&self) -> DocumentGeneration {
        DocumentGeneration {
            content_revision: self.content_revision,
            revision_token: self.revision_token,
        }
    }

    /// Maximum persisted parser/chunker/embedder fingerprint length.
    pub const MAX_PIPELINE_FINGERPRINT_LEN: usize = 512;
    /// Maximum persisted stable failure-code length.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum persisted local diagnostic-detail length.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4096;
}

/// Metadata returned by bounded document listings.
///
/// This deliberately excludes canonical content and the revision token so list
/// callers cannot accidentally load either large source text or write-only
/// concurrency credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSummaryRecord {
    /// Stable identifier shared with the retrieval index.
    pub id: DocumentId,
    /// Owning project, or `None` for an explicitly unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub source_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Current authoritative source revision.
    pub content_revision: i64,
    /// Processing lifecycle of the current authoritative revision.
    pub processing_status: DocumentProcessingStatus,
    /// Revision currently represented in the retrieval index, if any.
    pub indexed_revision: Option<i64>,
    /// Chunker/embedder fingerprint for the indexed revision.
    pub index_fingerprint: Option<String>,
    /// When this record was first created.
    pub created_at: DateTime<Utc>,
    /// When authoritative content or metadata last changed.
    pub updated_at: DateTime<Utc>,
    /// When the current index watermark was recorded.
    pub indexed_at: Option<DateTime<Utc>>,
}

/// Stable position in a newest-first document listing.
///
/// Cursors use both creation time and id because creation timestamps need not
/// be unique. Records following this cursor compare strictly lower by the
/// descending `(created_at, id)` display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentListCursor {
    /// Creation time of the final item in the preceding page.
    pub created_at: DateTime<Utc>,
    /// Id of the final item in the preceding page.
    pub id: DocumentId,
}

/// Authoritative content to create or replace for a document.
///
/// The store owns revision and index-watermark transitions: the first upsert is
/// revision one; each replacement increments it and clears the prior watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentUpsert {
    /// Stable identifier shared with the retrieval index.
    pub id: DocumentId,
    /// Owning project, or `None` for an explicitly unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub source_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Parsed text-of-record.
    pub canonical_text: String,
    /// Parser-produced mappings from canonical text to original source pages.
    pub source_regions: Vec<SourceRegion>,
    /// Time of this authoritative write.
    pub updated_at: DateTime<Utc>,
}

/// Corpus ownership filter for document listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentScope {
    /// Every document, for maintenance and reindexing only.
    All,
    /// Only explicitly projectless documents.
    Unscoped,
    /// Only documents owned by this project.
    Project(ProjectId),
}

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The system prompt / instructions.
    System,
    /// Input from the human user.
    User,
    /// Output from the model.
    Assistant,
    /// A tool result fed back into the model.
    Tool,
}

/// A persistent conversation. Owns a workspace directory the agent operates in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    /// Stable identifier.
    pub id: ChatId,
    /// The project this chat belongs to, or `None` for a loose (projectless) chat.
    pub project_id: Option<ProjectId>,
    /// Human-facing title; `None` until one is set or derived.
    pub title: Option<String>,
    /// The model this chat runs against, or `None` to use the configured default.
    pub model: Option<String>,
    /// Absolute path to this chat's workspace directory.
    pub workspace_dir: PathBuf,
    /// When the chat was created.
    pub created_at: DateTime<Utc>,
}

/// One message in a chat: user input or assistant text.
///
/// Tool calls are not messages; they persist separately (the `tool_call` table)
/// and are correlated by `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Stable identifier.
    pub id: MessageId,
    /// The chat this message belongs to.
    pub chat_id: ChatId,
    /// The turn this message was produced in.
    pub turn_id: TurnId,
    /// Who authored it.
    pub role: Role,
    /// The text body.
    pub content: String,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

/// A persisted tool invocation — name, arguments, and (once finished) result.
///
/// Distinct from [`Message`]: the model transcript rebuilds `ToolUse` /
/// `ToolResult` blocks from these rows so cross-turn context keeps structured
/// tool activity, not just free text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Stable id (same as the live [`crate::id::CallId`] on the event stream).
    pub id: crate::id::CallId,
    /// Chat this call belongs to.
    pub chat_id: ChatId,
    /// Turn that produced the call.
    pub turn_id: TurnId,
    /// Provider-facing tool-use id (Anthropic `tool_use.id`, OpenAI `tool_call_id`).
    pub provider_id: String,
    /// Tool name.
    pub name: String,
    /// Parsed JSON arguments.
    pub arguments: serde_json::Value,
    /// Result text fed back to the model, once completed.
    pub result: Option<String>,
    /// Whether the tool reported a failure.
    #[serde(default)]
    pub is_error: bool,
    /// When the call was recorded (args known).
    pub created_at: DateTime<Utc>,
    /// When the result was written, if completed.
    pub completed_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    #[test]
    fn document_processing_enums_have_stable_snake_case_values() {
        assert_eq!(
            serde_json::to_string(&DocumentProcessingStatus::Processing).unwrap(),
            "\"processing\""
        );
        assert_eq!(
            serde_json::to_string(&DocumentJobKind::Index).unwrap(),
            "\"index\""
        );
        assert_eq!(
            serde_json::to_string(&DocumentJobStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert!(DocumentJobStatus::Succeeded.is_terminal());
        assert!(!DocumentJobStatus::Running.is_terminal());
    }

    fn page_region(start: usize, end: usize, page: u32) -> SourceRegion {
        SourceRegion {
            span: ByteSpan::new(start, end),
            location: SourceLocation::Page {
                number: NonZeroU32::new(page).unwrap(),
            },
        }
    }

    #[test]
    fn source_region_validation_accepts_ordered_regions_and_gaps() {
        let text = "aé gap z";
        assert_eq!(
            validate_source_regions(text, &[page_region(0, 3, 1), page_region(8, 9, 2)]),
            Ok(())
        );
    }

    #[test]
    fn source_region_validation_rejects_invalid_spans() {
        let text = "aéz";
        assert!(validate_source_regions(text, &[page_region(1, 2, 1)]).is_err());
        assert!(validate_source_regions(text, &[page_region(0, 0, 1)]).is_err());
        assert!(validate_source_regions(text, &[page_region(0, 99, 1)]).is_err());
        assert!(
            validate_source_regions(text, &[page_region(2, 4, 2), page_region(0, 1, 1)]).is_err()
        );
    }
}
