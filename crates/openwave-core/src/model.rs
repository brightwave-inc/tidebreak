//! The persisted conversation model.
//!
//! Mirrors the conversation tables of `Store` schema v1. A
//! [`Chat`] is a durable conversation with an ordered, pathless host-root
//! projection; a
//! [`TurnRun`] is one durably scheduled agent turn, and a [`Message`] is one
//! user input or assistant answer within it. Steps remain runtime concepts of
//! the agent loop.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU32;
use uuid::Uuid;

use crate::provider::Usage;

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

use crate::id::{
    AgentRunId, CallId, ChatId, DocumentId, DocumentJobId, HostRootId, MessageId, ProjectId,
    RootAttachmentChangeId, TurnId,
};

/// Maximum number of host roots projected onto one project or conversation.
///
/// The host broker separately bounds and authorizes its live registry. This
/// product-side limit keeps API responses, turn snapshots, and future CAS
/// replacements predictably small.
pub const MAX_ROOT_ATTACHMENTS: usize = 32;

/// Largest attachment revision represented exactly by every supported client.
///
/// JSON numbers become JavaScript `number` values in the desktop renderer, so
/// revisions stay within the integer-safe range instead of silently losing CAS
/// precision at the product boundary.
pub const MAX_ATTACHMENT_REVISION: i64 = 9_007_199_254_740_991;

/// Why a root appears in one conversation's exact ordered projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentOrigin {
    /// Snapshotted from project defaults when the conversation was created.
    ProjectDefault,
    /// Added specifically to this conversation by trusted native control.
    Conversation,
}

/// One pathless root in a conversation's exact ordered projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRootAttachment {
    /// Opaque broker root identity. This value grants no authority by itself.
    pub root_id: HostRootId,
    /// Product-level provenance for ordering and future management UI.
    pub origin: RootAttachmentOrigin,
}

/// Desired broker and product state for one durable root-attachment change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentChangeAction {
    /// Make the root available to this conversation.
    Attach,
    /// Remove the root only from this conversation.
    Detach,
}

/// Product identity that owns the broker grant used by an attachment change.
///
/// This is derived by the store while the chat and its projection are locked;
/// callers cannot choose it in [`BeginRootAttachmentChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentSubjectKind {
    /// The root is owned by the chat's project.
    Project,
    /// The root is owned by this exact conversation.
    Conversation,
}

/// Durable phase of a product-side attachment state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAttachmentChangePhase {
    /// Product intent is durable and awaits broker reconciliation.
    AwaitingBroker,
    /// Broker reconciliation and the final product projection are durable.
    Completed,
    /// Broker reconciliation failed and the final product projection is durable.
    Failed,
}

/// Bounded transport-safe failure retained by an attachment change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentChangeFailure {
    /// Stable machine-readable failure category.
    pub code: String,
    /// Bounded diagnostic message safe to retain in product storage.
    pub message: String,
    /// Whether trusted native control may offer an explicit retry.
    pub retryable: bool,
}

impl RootAttachmentChangeFailure {
    /// Maximum UTF-8 bytes in a stable failure code.
    pub const MAX_CODE_LEN: usize = 64;
    /// Maximum UTF-8 bytes in a retained diagnostic message.
    pub const MAX_MESSAGE_LEN: usize = 256;

    /// Validate the bounded failure payload before persistence.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.code.is_empty() {
            return Err("root attachment failure code must not be empty");
        }
        if self.code.len() > Self::MAX_CODE_LEN {
            return Err("root attachment failure code exceeds the supported limit");
        }
        if self.code.contains('\0') {
            return Err("root attachment failure code contains a null byte");
        }
        if self.message.is_empty() {
            return Err("root attachment failure message must not be empty");
        }
        if self.message.len() > Self::MAX_MESSAGE_LEN {
            return Err("root attachment failure message exceeds the supported limit");
        }
        if self.message.contains('\0') {
            return Err("root attachment failure message contains a null byte");
        }
        Ok(())
    }
}

/// Caller-controlled identity and intent for beginning one attachment change.
///
/// Subject ownership, projection provenance, and projection position are
/// intentionally absent. The store derives them atomically from authoritative
/// chat state rather than trusting a native or HTTP caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeginRootAttachmentChange {
    /// Stable idempotency identity, also used as the broker operation UUID.
    pub id: RootAttachmentChangeId,
    /// Conversation whose exact broker attachment changes.
    pub chat_id: ChatId,
    /// Stable native reconciler allowed to finish this work.
    pub executor_id: Uuid,
    /// Opaque root identity; possession grants no authority.
    pub root_id: HostRootId,
    /// Desired attachment state.
    pub action: RootAttachmentChangeAction,
    /// CAS fence observed by the caller.
    pub expected_attachment_revision: i64,
    /// Caller operation time retained at database microsecond precision as
    /// immutable request identity.
    pub created_at: DateTime<Utc>,
}

impl BeginRootAttachmentChange {
    /// Validate caller-controlled fields before beginning durable work.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chat_id.as_uuid().is_nil() {
            return Err("root attachment change chat id must not be nil");
        }
        if self.executor_id.is_nil() {
            return Err("root attachment change executor id must not be nil");
        }
        if !(0..=MAX_ATTACHMENT_REVISION).contains(&self.expected_attachment_revision) {
            return Err("expected attachment revision is outside the supported range");
        }
        Ok(())
    }
}

/// Exact broker observation supplied when finishing an attachment change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RootAttachmentChangeTerminal {
    /// The broker durably applied or recovered the exact requested mutation.
    Completed {
        /// Whether that broker operation changed its live attachment set.
        broker_changed: bool,
        /// Broker attachment state observed by the terminal receipt.
        broker_currently_attached: bool,
    },
    /// The broker durably rejected or failed the exact requested mutation.
    Failed {
        /// Whether the failed broker operation reported changing live state.
        broker_changed: Option<bool>,
        /// Live attachment state when the broker could report it.
        broker_currently_attached: Option<bool>,
        /// Stable bounded failure retained for exact retries.
        failure: RootAttachmentChangeFailure,
    },
}

impl RootAttachmentChangeTerminal {
    /// Validate bounded terminal data before persistence.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Completed { .. } => Ok(()),
            Self::Failed { failure, .. } => failure.validate(),
        }
    }
}

/// Product-side durable state for one broker-backed attachment mutation.
///
/// The immutable subject and projection metadata are derived under the same
/// store lock as `before_revision` and the intent projection. Flat
/// optional terminal fields make exact state validation explicit at storage
/// boundaries and map directly to relational columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootAttachmentChange {
    pub id: RootAttachmentChangeId,
    pub chat_id: ChatId,
    pub executor_id: Uuid,
    pub root_id: HostRootId,
    pub action: RootAttachmentChangeAction,
    /// Broker grant subject derived from authoritative chat state.
    pub subject_kind: RootAttachmentSubjectKind,
    /// Project or conversation UUID paired with `subject_kind`.
    pub subject_id: Uuid,
    /// Projection provenance captured by the store, when the root existed.
    pub origin: Option<RootAttachmentOrigin>,
    /// Zero-based projection position captured by the store, when applicable.
    pub projection_position: Option<u32>,
    /// Whether the root appeared in the projection before this operation.
    pub projection_existed_before: bool,
    pub expected_revision: i64,
    /// Authoritative revision observed when begin committed.
    pub before_revision: i64,
    /// Revision after begin durably projected the operation's intent.
    pub intent_revision: i64,
    pub phase: RootAttachmentChangePhase,
    /// Final revision after completion or rollback; absent while awaiting broker.
    pub result_revision: Option<i64>,
    /// Whether the final projection differs from the projection before begin.
    pub projection_changed: Option<bool>,
    /// Historical broker mutation result, required for completed work and
    /// retained for failed work when the broker could report it.
    pub broker_changed: Option<bool>,
    /// Terminal broker attachment observation, required for completed work and
    /// retained for failed work when the broker could report it.
    pub broker_currently_attached: Option<bool>,
    /// Stable broker failure, present only for failed work.
    pub failure: Option<RootAttachmentChangeFailure>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl RootAttachmentChange {
    /// Validate identity, revision, projection, and terminal-state invariants.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.chat_id.as_uuid().is_nil() {
            return Err("root attachment change chat id must not be nil");
        }
        if self.executor_id.is_nil() {
            return Err("root attachment change executor id must not be nil");
        }
        if self.subject_id.is_nil() {
            return Err("root attachment change subject id must not be nil");
        }
        if self.subject_kind == RootAttachmentSubjectKind::Conversation
            && self.subject_id != *self.chat_id.as_uuid()
        {
            return Err("conversation attachment subject must match the chat");
        }
        for revision in [
            self.expected_revision,
            self.before_revision,
            self.intent_revision,
        ] {
            if !(0..=MAX_ATTACHMENT_REVISION).contains(&revision) {
                return Err("root attachment change revision is outside the supported range");
            }
        }
        if self
            .result_revision
            .is_some_and(|revision| !(0..=MAX_ATTACHMENT_REVISION).contains(&revision))
        {
            return Err("root attachment change result revision is outside the supported range");
        }
        if self.expected_revision != self.before_revision {
            return Err("root attachment change began from an unexpected revision");
        }
        if self.projection_position.is_some() != self.origin.is_some() {
            return Err("root attachment projection origin and position must appear together");
        }
        let projection_metadata_required =
            self.projection_existed_before || self.action == RootAttachmentChangeAction::Attach;
        if projection_metadata_required != self.origin.is_some() {
            return Err("root attachment prior projection metadata is inconsistent");
        }
        if self.action == RootAttachmentChangeAction::Attach
            && !self.projection_existed_before
            && self.origin != Some(RootAttachmentOrigin::Conversation)
        {
            return Err("a newly attached root must use conversation provenance");
        }
        if self
            .projection_position
            .is_some_and(|position| position as usize >= MAX_ROOT_ATTACHMENTS)
        {
            return Err("root attachment projection position exceeds the supported limit");
        }
        let intent_advanced =
            self.action == RootAttachmentChangeAction::Attach && !self.projection_existed_before;
        let terminal_may_advance = intent_advanced
            || (self.action == RootAttachmentChangeAction::Detach
                && self.projection_existed_before);
        let required_headroom = i64::from(intent_advanced) + i64::from(terminal_may_advance);
        if self.before_revision > MAX_ATTACHMENT_REVISION - required_headroom {
            return Err("root attachment change lacks reserved revision headroom");
        }
        let expected_intent_revision = self
            .before_revision
            .checked_add(i64::from(intent_advanced))
            .ok_or("root attachment intent revision overflowed")?;
        if self.intent_revision != expected_intent_revision {
            return Err("root attachment intent revision is inconsistent");
        }

        match self.phase {
            RootAttachmentChangePhase::AwaitingBroker => {
                if self.result_revision.is_some()
                    || self.projection_changed.is_some()
                    || self.broker_changed.is_some()
                    || self.broker_currently_attached.is_some()
                    || self.failure.is_some()
                    || self.finished_at.is_some()
                {
                    return Err("awaiting root attachment change has terminal fields");
                }
            }
            RootAttachmentChangePhase::Completed => {
                if self.result_revision.is_none()
                    || self.projection_changed.is_none()
                    || self.broker_changed.is_none()
                    || self.broker_currently_attached.is_none()
                    || self.failure.is_some()
                    || self.finished_at.is_none()
                {
                    return Err("completed root attachment change has invalid terminal fields");
                }
            }
            RootAttachmentChangePhase::Failed => {
                if self.result_revision.is_none()
                    || self.projection_changed.is_none()
                    || self.failure.is_none()
                    || self.finished_at.is_none()
                {
                    return Err("failed root attachment change has invalid terminal fields");
                }
                self.failure.as_ref().expect("checked above").validate()?;
            }
        }
        if self
            .finished_at
            .is_some_and(|finished_at| finished_at < self.created_at)
        {
            return Err("root attachment change finish time precedes creation");
        }
        if self.phase != RootAttachmentChangePhase::AwaitingBroker {
            let completed = self.phase == RootAttachmentChangePhase::Completed;
            let terminal_removal = (completed
                && self.action == RootAttachmentChangeAction::Detach
                && self.projection_existed_before)
                || (!completed && intent_advanced);
            let expected_result_revision = self
                .intent_revision
                .checked_add(i64::from(terminal_removal))
                .ok_or("root attachment result revision overflowed")?;
            let desired_attached = self.action == RootAttachmentChangeAction::Attach;
            let expected_projection_changed =
                completed && self.projection_existed_before != desired_attached;
            if self.result_revision != Some(expected_result_revision)
                || self.projection_changed != Some(expected_projection_changed)
            {
                return Err("root attachment terminal projection metadata is inconsistent");
            }
            if completed && self.broker_currently_attached != Some(desired_attached) {
                return Err("completed root attachment change contradicts broker state");
            }
            if !completed
                && self
                    .broker_currently_attached
                    .is_some_and(|attached| attached == desired_attached)
            {
                return Err("failed root attachment change contradicts broker state");
            }
        }
        Ok(())
    }
}

/// An optional grouping of chats that share project context and a document
/// corpus. A chat may belong to a project or stand alone — unlike some designs
/// that make a project mandatory, OpenWave keeps loose, projectless chats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// Human-facing title.
    pub title: Option<String>,
    /// CAS revision of the ordered root projection.
    pub attachment_revision: i64,
    /// Ordered opaque root defaults for conversations created in this project.
    /// These ids are product state, never host authorization.
    pub root_attachments: Vec<HostRootId>,
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
    /// Parse immutable raw source bytes into canonical text and provenance.
    Parse,
    /// Chunk and embed canonical content into the derived retrieval index.
    Index,
}

impl DocumentJobKind {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Index => "index",
        }
    }
}

/// Immutable raw source retained for reparsing one document revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSourceBlob {
    /// UUID key in the configured [`crate::BlobStore`].
    pub id: Uuid,
    /// SHA-256 digest of the exact source bytes.
    pub sha256: [u8; 32],
    /// Exact source byte length.
    pub byte_len: u64,
}

impl DocumentSourceBlob {
    const CONTENT_NAMESPACE: Uuid = Uuid::from_u128(0xd262_91eb_f9f7_5b4d_a65d_4a44_70f8_081f);

    /// Describe source bytes using a deterministic content-addressed UUID.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let sha256: [u8; 32] = Sha256::digest(bytes).into();
        Self::from_digest(
            sha256,
            u64::try_from(bytes.len()).expect("source byte length exceeds u64"),
        )
    }

    /// Describe retained source from its already-computed digest and byte length.
    #[must_use]
    pub fn from_digest(sha256: [u8; 32], byte_len: u64) -> Self {
        Self {
            id: Uuid::new_v5(&Self::CONTENT_NAMESPACE, &sha256),
            sha256,
            byte_len,
        }
    }

    /// Whether the blob key is the canonical content address for its digest.
    #[must_use]
    pub fn has_content_addressed_id(&self) -> bool {
        self.id == Uuid::new_v5(&Self::CONTENT_NAMESPACE, &self.sha256)
    }
}

/// Metadata and immutable bytes accepted for asynchronous document parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSourceUpsert {
    /// Stable document identifier.
    pub id: DocumentId,
    /// Owning project, or `None` for an explicitly unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, when known.
    pub source_uri: Option<String>,
    /// Media type used to select the parser.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Immutable source bytes already published to the blob store.
    pub source_blob: DocumentSourceBlob,
    /// Source metadata timestamp; workflow timestamps remain store-owned.
    pub updated_at: DateTime<Utc>,
}

/// Canonical parser output published by a successfully leased parse job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentParseOutput {
    /// Parsed text-of-record used by the indexing stage.
    pub canonical_text: String,
    /// Parser-produced mappings into the original source.
    pub source_regions: Vec<SourceRegion>,
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
    /// Immutable raw bytes for this revision, when retained.
    pub source_blob: Option<DocumentSourceBlob>,
    /// Parsed text-of-record used to rechunk, re-embed, and verify citations.
    pub canonical_text: String,
    /// Parser fingerprint that produced the canonical text, when tracked.
    pub canonical_fingerprint: Option<String>,
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

/// Durable deletion state for one content-addressed source blob.
///
/// Rows are coalesced by `blob_id`: dropping another reference resets the row
/// to queued, while establishing any live reference cancels it. Queued rows are
/// candidates, not deletion authorization: claim must atomically recheck the
/// indexed authoritative document references and cancel any referenced blob.
/// Exact worker leases fence a previous retirement episode from completing
/// after either transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRetirement {
    /// Globally content-addressed blob identity.
    pub blob_id: Uuid,
    /// Durable delivery state.
    pub status: BlobRetirementStatus,
    /// Claims already made, including the current claim when running.
    pub attempt_count: i32,
    /// Maximum claims before a retryable deletion failure becomes terminal.
    pub max_attempts: i32,
    /// Earliest time this retirement may be claimed.
    pub available_at: DateTime<Utc>,
    /// Exact claim identity required for heartbeat/completion writes.
    pub lease_token: Option<Uuid>,
    /// When the current claim becomes recoverably stale.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the current retirement episode was first claimed.
    pub started_at: Option<DateTime<Utc>>,
    /// When this retirement entered a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub last_error_code: Option<String>,
    /// Bounded diagnostic detail for local operators.
    pub last_error_detail: Option<String>,
    /// When this blob first became a retirement candidate.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl BlobRetirement {
    /// Default number of deletion claims before explicit intervention.
    pub const DEFAULT_MAX_ATTEMPTS: i32 = 5;
    /// Maximum persisted stable failure-code length.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum persisted local diagnostic-detail length.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4096;
}

/// Durable delivery state for one coalesced blob retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlobRetirementStatus {
    /// Due for authoritative reference validation at `available_at`; only an
    /// unreferenced candidate may become running.
    Queued,
    /// Currently owned by the exact lease token and expiry on the row.
    Running,
    /// Failed transiently and becomes claimable again at `available_at`.
    RetryWait,
    /// The unreferenced blob was deleted or was already absent.
    Succeeded,
    /// Deletion exhausted retries or failed permanently.
    Failed,
    /// A live-reference write or claim-time check cancelled this episode. This
    /// does not assert that the blob remains referenced forever; a later final
    /// reference drop may reset the coalesced row to queued.
    Cancelled,
}

impl BlobRetirementStatus {
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

    /// Whether no worker may claim this row without an explicit requeue.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
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

/// A persistent conversation with an exact, ordered host-root projection.
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
    /// CAS revision of this conversation's exact root projection.
    pub attachment_revision: i64,
    /// Ordered opaque roots available for future broker-backed operations.
    /// Live broker authorization remains mandatory and may revoke access at any
    /// time, regardless of this projection.
    pub root_attachments: Vec<ChatRootAttachment>,
    /// When the chat was created.
    pub created_at: DateTime<Utc>,
}

pub(crate) fn validate_project_root_projection(project: &Project) -> Result<(), &'static str> {
    if !(0..=MAX_ATTACHMENT_REVISION).contains(&project.attachment_revision) {
        return Err("project attachment revision is outside the supported range");
    }
    if project.root_attachments.len() > MAX_ROOT_ATTACHMENTS {
        return Err("project root attachment count exceeds the supported limit");
    }
    if !project.root_attachments.is_empty() && project.attachment_revision == 0 {
        return Err("a nonempty project root projection must have a positive revision");
    }
    let unique = project
        .root_attachments
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != project.root_attachments.len() {
        return Err("project root attachments must be unique");
    }
    Ok(())
}

pub(crate) fn validate_chat_root_projection(chat: &Chat) -> Result<(), &'static str> {
    if !(0..=MAX_ATTACHMENT_REVISION).contains(&chat.attachment_revision) {
        return Err("chat attachment revision is outside the supported range");
    }
    if chat.root_attachments.len() > MAX_ROOT_ATTACHMENTS {
        return Err("chat root attachment count exceeds the supported limit");
    }
    if !chat.root_attachments.is_empty() && chat.attachment_revision == 0 {
        return Err("a nonempty chat root projection must have a positive revision");
    }
    let unique = chat
        .root_attachments
        .iter()
        .map(|attachment| attachment.root_id)
        .collect::<std::collections::HashSet<_>>();
    if unique.len() != chat.root_attachments.len() {
        return Err("chat root attachments must be unique");
    }
    if chat.project_id.is_none()
        && chat
            .root_attachments
            .iter()
            .any(|attachment| attachment.origin == RootAttachmentOrigin::ProjectDefault)
    {
        return Err("a standalone chat cannot contain project-default roots");
    }
    let mut conversation_root_seen = false;
    for attachment in &chat.root_attachments {
        match attachment.origin {
            RootAttachmentOrigin::ProjectDefault if conversation_root_seen => {
                return Err("project-default roots must precede conversation roots");
            }
            RootAttachmentOrigin::Conversation => conversation_root_seen = true,
            RootAttachmentOrigin::ProjectDefault => {}
        }
    }
    Ok(())
}

pub(crate) fn validate_chat_root_projection_against_project(
    chat: &Chat,
    project_roots: &[HostRootId],
) -> Result<(), &'static str> {
    validate_chat_root_projection(chat)?;
    if chat.project_id.is_none() && !project_roots.is_empty() {
        return Err("a standalone chat cannot snapshot project roots");
    }
    if chat.root_attachments.len() < project_roots.len() {
        return Err("chat is missing project root defaults");
    }
    for (expected, actual) in project_roots.iter().zip(&chat.root_attachments) {
        if actual.root_id != *expected || actual.origin != RootAttachmentOrigin::ProjectDefault {
            return Err("chat project root snapshot does not match current project defaults");
        }
    }
    if chat.root_attachments[project_roots.len()..]
        .iter()
        .any(|attachment| attachment.origin != RootAttachmentOrigin::Conversation)
    {
        return Err("chat-specific roots must follow project defaults");
    }
    Ok(())
}

/// One durable foreground or sandboxed background execution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    /// Stable idempotency identity.
    pub id: crate::id::AgentRunId,
    /// Conversation that owns this run and its events.
    pub chat_id: ChatId,
    /// Foreground coordinator that owns this run. Always absent at depth zero.
    pub parent_id: Option<crate::id::AgentRunId>,
    /// Exact tool-call identity that requested a sandbox child.
    pub spawn_call_id: Option<crate::id::CallId>,
    /// Where and how the run executes.
    pub execution: AgentRunExecution,
    /// Explicit bounded hierarchy depth. OpenWave v1 permits only zero or one.
    pub depth: u8,
    /// Durable lifecycle state.
    pub status: AgentRunStatus,
    /// Exact delegated task for a sandbox run. Foreground runs have no task.
    pub input: Option<String>,
    /// When the run was durably accepted.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl AgentRun {
    /// Recursive agent spawning is deliberately excluded from the initial model.
    pub const MAX_DEPTH: u8 = 1;
    /// Maximum persisted delegated task length.
    pub const MAX_INPUT_LEN: usize = 65_536;
}

/// Execution boundary for an [`AgentRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunExecution {
    /// Conversation coordinator advanced by foreground turn work.
    Foreground,
    /// Isolated background work advanced by the sandbox scheduler.
    Sandbox,
}

impl AgentRunExecution {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Durable lifecycle of an [`AgentRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentRunStatus {
    /// Foreground coordinator is available to own chat turns.
    Active,
    /// Sandboxed work was accepted and awaits a bounded scheduler slot.
    Queued,
    /// One exact scheduler lease currently owns the run.
    Running,
    /// The run checkpointed and released its worker for a durable dependency.
    Waiting,
    /// Replay-safe work awaits another scheduler claim.
    RetryWait,
    /// The run submitted its final result successfully.
    Completed,
    /// The run failed permanently or cannot be replayed safely.
    Failed,
    /// The run was cancelled and has quiesced.
    Cancelled,
}

impl AgentRunStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no worker may advance this run without a new explicit command.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Durable execution state of one user turn.
///
/// A turn is accepted once under its stable [`TurnId`], then claimed under an
/// exact lease before model or tool work begins. Keeping this state separate
/// from messages lets API acceptance, worker ownership, and terminal resolution
/// be fenced without treating append-only conversation content as a job queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRun {
    /// Stable turn and idempotency identity.
    pub id: TurnId,
    /// Conversation this turn belongs to.
    pub chat_id: ChatId,
    /// Foreground coordinator that owns this conversation segment.
    pub agent_run_id: AgentRunId,
    /// Exact persisted user message that supplied this turn's initial input.
    pub input_message_id: MessageId,
    /// Exact designated terminal assistant message committed with successful
    /// completion. The composite database FK enforces its message/chat/turn
    /// identity; [`Store::complete_turn_run`](crate::storage::Store::complete_turn_run)
    /// enforces the assistant role because a foreign key cannot bind a literal
    /// role value.
    pub output_message_id: Option<MessageId>,
    /// Model selected when the turn was accepted.
    pub model: String,
    /// Durable delivery state.
    pub status: TurnRunStatus,
    /// Failure attempts already started. Client-execution resumptions stay
    /// within the same attempt and do not consume this retry budget.
    pub attempt_count: i32,
    /// Maximum failure attempts permitted for this turn.
    pub max_attempts: i32,
    /// Worker lease segments already issued, including resumptions within one
    /// failure attempt.
    pub claim_count: i32,
    /// Model calls committed through the latest durable client checkpoint.
    /// Workers use this baseline when a later lease segment resumes.
    pub model_steps: i32,
    /// Provider usage committed through the latest durable client checkpoint.
    /// The terminal event carries the final total after the last live segment.
    pub usage: Usage,
    /// Earliest time queued, retry-wait, or resuming work may be claimed.
    pub available_at: DateTime<Utc>,
    /// Exact claim identity required for heartbeat and resolution writes.
    pub lease_token: Option<Uuid>,
    /// When the current claim becomes stale.
    pub lease_expires_at: Option<DateTime<Utc>>,
    /// When the first claim began.
    pub started_at: Option<DateTime<Utc>>,
    /// When this turn entered a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub last_error_code: Option<String>,
    /// Bounded diagnostic detail for local operators.
    pub last_error_detail: Option<String>,
    /// Durable generation revision captured before model work begins.
    pub steer_revision: i64,
    /// When the most recent durable steer application committed.
    pub last_steer_applied_at: Option<DateTime<Utc>>,
    /// When this turn was accepted.
    pub created_at: DateTime<Utc>,
    /// When its durable state last changed.
    pub updated_at: DateTime<Utc>,
}

impl TurnRun {
    /// New turns are not automatically replayed after an ambiguous side effect.
    /// A later resumable-checkpoint slice may opt specific turns into more.
    pub const DEFAULT_MAX_ATTEMPTS: i32 = 1;
    /// Maximum persisted model identifier length.
    pub const MAX_MODEL_LEN: usize = 512;
    /// Maximum persisted machine-readable error code length.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum persisted diagnostic detail length.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4096;
}

/// Durable delivery state of a [`TurnRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnRunStatus {
    /// Accepted durably and eligible to be claimed at `available_at`.
    Queued,
    /// Currently owned by the exact lease token and expiry on the turn.
    Running,
    /// Cancellation was requested while a worker still owns the exact lease.
    /// The chat remains occupied until that worker acknowledges quiescence or
    /// the expired lease is cleaned up.
    Cancelling,
    /// The worker checkpointed safely and released its lease while one exact
    /// durable client call executes on the host.
    WaitingForClient,
    /// Cancellation was requested after the client call may have started. The
    /// chat stays occupied until that exact call reports a terminal result.
    CancellingClient,
    /// The blocking client call resolved and the checkpoint is eligible for a
    /// fresh worker lease without consuming another failure attempt.
    Resuming,
    /// Failed safely before an ambiguous side effect and awaits another claim.
    RetryWait,
    /// Produced a final answer successfully.
    Completed,
    /// Failed permanently or cannot be replayed safely.
    Failed,
    /// Cancelled before producing a final answer.
    Cancelled,
}

impl TurnRunStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::WaitingForClient => "waiting_for_client",
            Self::CancellingClient => "cancelling_client",
            Self::Resuming => "resuming",
            Self::RetryWait => "retry_wait",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether no worker may claim this turn without an explicit transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Immutable request for one tool operation that must execute in a trusted
/// client rather than the server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientToolCallRequest {
    /// Caller-supplied idempotency identity.
    pub id: CallId,
    /// Owning conversation.
    pub chat_id: ChatId,
    /// Turn checkpointed for this call.
    pub turn_id: TurnId,
    /// Provider/tool namespace that produced the request.
    pub provider_id: String,
    /// Tool name understood by the trusted client.
    pub name: String,
    /// Canonical model-supplied arguments.
    pub arguments: serde_json::Value,
}

impl ClientToolCallRequest {
    /// Whether this request fits the durable client-execution contract.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let labels_valid = [self.provider_id.as_str(), self.name.as_str()]
            .into_iter()
            .all(|value| {
                !value.is_empty()
                    && value.len() <= ToolCallRecord::MAX_LABEL_LEN
                    && !value.contains('\0')
            });
        self.id.0 != Uuid::nil()
            && self.chat_id.0 != Uuid::nil()
            && self.turn_id.0 != Uuid::nil()
            && labels_valid
            && serde_json::to_vec(&self.arguments)
                .is_ok_and(|arguments| arguments.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES)
    }
}

/// Progress atomically committed with one client-execution checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCheckpointProgress {
    /// Model calls consumed since the preceding durable checkpoint.
    pub model_steps: i32,
    /// Provider usage incurred since the preceding durable checkpoint.
    pub usage: Usage,
}

/// Immutable receipt for one turn parked on a client tool call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnClientWait {
    /// Exact durable client call.
    pub call_id: CallId,
    /// Turn that released its worker lease.
    pub turn_id: TurnId,
    /// Owning conversation.
    pub chat_id: ChatId,
    /// Worker lease segment that created the checkpoint.
    pub park_lease_token: Uuid,
    /// Failure attempt containing the checkpoint.
    pub attempt_count: i32,
    /// Exact lease-segment ordinal containing the checkpoint.
    pub claim_count: i32,
    /// Exact progress delta committed by this checkpoint.
    pub progress: TurnCheckpointProgress,
    /// Durable wait lifecycle.
    pub status: TurnClientWaitStatus,
    /// Store-owned time when parking committed.
    pub parked_at: DateTime<Utc>,
    /// Store-owned time when the wait stopped blocking the turn.
    pub closed_at: Option<DateTime<Utc>>,
}

/// Durable lifecycle of a turn/client-call checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnClientWaitStatus {
    /// The exact client call still blocks the turn.
    Waiting,
    /// The exact client call resolved and made the turn resumable.
    Resumed,
    /// Cancellation won and the turn will not resume from this checkpoint.
    Cancelled,
}

impl TurnClientWaitStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Resumed => "resumed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One durably accepted steering instruction for an active turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSteer {
    /// Caller-supplied idempotency identity.
    pub id: crate::id::TurnSteerId,
    /// Exact turn that receives this instruction.
    pub turn_id: TurnId,
    /// Owning chat, duplicated so the database can enforce turn/message scope.
    pub chat_id: ChatId,
    /// Byte-exact user instruction.
    pub content: String,
    /// Whether delivery should preempt the current model stream.
    pub interrupt: bool,
    /// Durable delivery state.
    pub status: TurnSteerStatus,
    /// Exact worker lease that applied the instruction.
    pub applied_lease_token: Option<Uuid>,
    /// User message committed atomically with application.
    ///
    /// When present, this carries the same UUID as `id`, so one caller identity
    /// names both the instruction and its eventual conversation message.
    pub message_id: Option<MessageId>,
    /// When the instruction was accepted.
    pub created_at: DateTime<Utc>,
    /// When it was applied or rejected.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl TurnSteer {
    /// Maximum accepted instruction size in Unicode scalar values.
    pub const MAX_CONTENT_LEN: usize = 65_536;
}

/// Durable delivery state of a [`TurnSteer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnSteerStatus {
    /// Accepted and waiting for the exact live worker to apply it.
    Pending,
    /// User message and delivery receipt committed atomically.
    Applied,
    /// The turn stopped accepting instructions before this one could be applied.
    Rejected,
}

impl TurnSteerStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
        }
    }
}

/// Retry intent attached to one exact turn-attempt failure.
///
/// Workers must retain this value across an ambiguous database commit. A new
/// backoff timestamp is a different failure request and is rejected if the
/// original request already committed under the same lease token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnFailureRetry {
    /// Do not claim this turn again automatically.
    Permanent,
    /// Make the turn eligible for another claim at the exact requested time.
    RetryAt(DateTime<Utc>),
}

/// Immutable proof that one exact claimed attempt recorded a failure.
///
/// The mutable turn can advance to a later attempt after a retryable failure,
/// so this receipt is the durable idempotency record for ambiguous retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFailureReceipt {
    /// Exact claim identity that submitted the failure.
    pub lease_token: Uuid,
    /// Turn resolved by the claim.
    pub turn_id: TurnId,
    /// Attempt number recorded in the immutable claim receipt.
    pub attempt_count: i32,
    /// Cumulative model calls consumed when the failure committed.
    pub model_steps: i32,
    /// Cumulative provider usage when the failure committed.
    pub usage: Usage,
    /// Requested retry time, retained even when exhaustion made the failure
    /// terminal. `None` represents an explicitly permanent failure.
    pub requested_retry_at: Option<DateTime<Utc>>,
    /// Stable machine-readable failure category.
    pub error_code: String,
    /// Bounded diagnostic detail for local operators.
    pub error_detail: Option<String>,
    /// Fresh operational time at which the first resolution committed.
    pub resolved_at: DateTime<Utc>,
    /// Historical result of this resolution (`retry_wait` or `failed`).
    pub result_status: TurnRunStatus,
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

/// Where an accepted tool invocation must execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallExecution {
    /// Execute inside the server-owned agent runtime.
    Server,
    /// Execute through a separately leased trusted client surface.
    Client,
}

impl ToolCallExecution {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
        }
    }
}

/// Durable lifecycle of one tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Canonical arguments are committed and execution has not resolved.
    Pending,
    /// Execution returned a successful model-facing result.
    Completed,
    /// Execution returned a stable failure and model-facing result.
    Failed,
    /// Execution was intentionally cancelled with a model-facing result.
    Cancelled,
}

impl ToolCallStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether execution has a durable final result.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// Exact terminal payload used for idempotent tool-call resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallResolution {
    /// Successful execution.
    Completed { result: String },
    /// Failed execution with stable machine and optional diagnostic detail.
    Failed {
        result: String,
        error_code: String,
        error_detail: Option<String>,
    },
    /// Intentional cancellation, including a user declining a native prompt.
    Cancelled { result: String },
}

impl ToolCallResolution {
    /// Terminal state represented by this payload.
    #[must_use]
    pub const fn status(&self) -> ToolCallStatus {
        match self {
            Self::Completed { .. } => ToolCallStatus::Completed,
            Self::Failed { .. } => ToolCallStatus::Failed,
            Self::Cancelled { .. } => ToolCallStatus::Cancelled,
        }
    }

    /// Model-facing result for this terminal outcome.
    #[must_use]
    pub fn result(&self) -> &str {
        match self {
            Self::Completed { result }
            | Self::Failed { result, .. }
            | Self::Cancelled { result } => result,
        }
    }
}

/// A persisted tool invocation — canonical identity, arguments, and lifecycle.
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
    /// Which trusted surface owns execution.
    pub execution: ToolCallExecution,
    /// Durable execution state.
    pub status: ToolCallStatus,
    /// Result text fed back to the model once terminal.
    pub result: Option<String>,
    /// Stable machine-readable failure code, only for `failed`.
    pub error_code: Option<String>,
    /// Bounded diagnostic failure detail, only for `failed`.
    pub error_detail: Option<String>,
    /// Exact client executor that owns the pending lease or resolved the call.
    pub client_executor_id: Option<Uuid>,
    /// Expiry of the exact client executor lease.
    pub client_lease_expires_at: Option<DateTime<Utc>>,
    /// When the call was recorded (args known).
    pub created_at: DateTime<Utc>,
    /// When the terminal outcome was written.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl ToolCallRecord {
    /// Maximum UTF-8 bytes accepted for provider identity or tool name.
    pub const MAX_LABEL_LEN: usize = 256;
    /// Maximum serialized canonical argument bytes.
    pub const MAX_ARGUMENT_BYTES: usize = 128 * 1024;
    /// Maximum model-facing terminal result bytes.
    pub const MAX_RESULT_BYTES: usize = 512 * 1024;
    /// Maximum stable failure code bytes.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum diagnostic failure detail bytes.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4 * 1024;
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
            serde_json::to_string(&DocumentJobKind::Parse).unwrap(),
            "\"parse\""
        );
        assert_eq!(
            serde_json::to_string(&DocumentJobStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert_eq!(
            serde_json::to_string(&BlobRetirementStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert!(DocumentJobStatus::Succeeded.is_terminal());
        assert!(!DocumentJobStatus::Running.is_terminal());
        assert!(BlobRetirementStatus::Cancelled.is_terminal());
        assert!(!BlobRetirementStatus::Queued.is_terminal());
        assert_eq!(
            serde_json::to_string(&TurnRunStatus::RetryWait).unwrap(),
            "\"retry_wait\""
        );
        assert!(TurnRunStatus::Completed.is_terminal());
        assert!(!TurnRunStatus::Running.is_terminal());
    }

    #[test]
    fn root_attachment_change_validation_enforces_derived_and_terminal_shape() {
        let chat_id = ChatId::new();
        let mut change = RootAttachmentChange {
            id: RootAttachmentChangeId::new(),
            chat_id,
            executor_id: Uuid::new_v4(),
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            action: RootAttachmentChangeAction::Attach,
            subject_kind: RootAttachmentSubjectKind::Conversation,
            subject_id: *chat_id.as_uuid(),
            origin: Some(RootAttachmentOrigin::Conversation),
            projection_position: Some(0),
            projection_existed_before: false,
            expected_revision: 0,
            before_revision: 0,
            intent_revision: 1,
            phase: RootAttachmentChangePhase::AwaitingBroker,
            result_revision: None,
            projection_changed: None,
            broker_changed: None,
            broker_currently_attached: None,
            failure: None,
            created_at: Utc::now(),
            finished_at: None,
        };
        assert_eq!(change.validate(), Ok(()));

        change.subject_id = Uuid::new_v4();
        assert!(change.validate().is_err());
        change.subject_id = *chat_id.as_uuid();
        change.intent_revision = 0;
        assert!(change.validate().is_err());
        change.intent_revision = 1;
        change.expected_revision = MAX_ATTACHMENT_REVISION - 1;
        change.before_revision = MAX_ATTACHMENT_REVISION - 1;
        change.intent_revision = MAX_ATTACHMENT_REVISION;
        assert!(change.validate().is_err());
        change.expected_revision = 0;
        change.before_revision = 0;
        change.intent_revision = 1;
        change.phase = RootAttachmentChangePhase::Completed;
        assert!(change.validate().is_err());
        change.result_revision = Some(1);
        change.projection_changed = Some(true);
        change.broker_changed = Some(true);
        change.broker_currently_attached = Some(true);
        change.finished_at = Some(change.created_at);
        assert_eq!(change.validate(), Ok(()));
        change.result_revision = Some(2);
        assert!(change.validate().is_err());
    }

    #[test]
    fn root_attachment_failure_is_bounded_by_utf8_bytes() {
        let valid = RootAttachmentChangeFailure {
            code: "broker_denied".into(),
            message: "root attachment was denied".into(),
            retryable: false,
        };
        assert_eq!(valid.validate(), Ok(()));

        let mut contains_null = valid.clone();
        contains_null.code.push('\0');
        assert!(contains_null.validate().is_err());
        contains_null = valid.clone();
        contains_null.message.push('\0');
        assert!(contains_null.validate().is_err());

        let mut oversized = valid;
        oversized.message = "é".repeat(RootAttachmentChangeFailure::MAX_MESSAGE_LEN / 2 + 1);
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn source_blob_identity_is_deterministic_and_content_addressed() {
        let first = DocumentSourceBlob::from_bytes(b"same source bytes");
        assert_eq!(first, DocumentSourceBlob::from_bytes(b"same source bytes"));
        assert_eq!(first.byte_len, 17);
        assert_eq!(
            first.sha256,
            <[u8; 32]>::from(Sha256::digest(b"same source bytes"))
        );
        assert_eq!(
            first.id,
            Uuid::parse_str("bb06b189-790a-5087-89fd-767534773c0f").unwrap()
        );
        assert!(first.has_content_addressed_id());
        assert_eq!(
            first,
            DocumentSourceBlob::from_digest(first.sha256, first.byte_len)
        );
        let mut invalid = first.clone();
        invalid.id = Uuid::new_v4();
        assert!(!invalid.has_content_addressed_id());
        assert_ne!(first.id, DocumentSourceBlob::from_bytes(b"other").id);
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
