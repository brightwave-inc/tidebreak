use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::id::{ChatId, DocumentId, ProjectId, TurnId};

/// What a caller can actually do with a source right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DocumentReadiness {
    /// The source contains text a reader can be given.
    Readable,
    /// Durably stored and citable by name, but holding no text to read.
    StoredNoText,
}

impl DocumentReadiness {
    /// Derive readiness from whether canonical text is present.
    #[must_use]
    pub const fn of(readable: bool) -> Self {
        if readable {
            Self::Readable
        } else {
            Self::StoredNoText
        }
    }

    /// Stable wire representation shared by agent tools.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Readable => "readable",
            Self::StoredNoText => "stored_no_text",
        }
    }
}

/// Content-addressed raw source retained for original-file access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentBlob {
    /// UUID key in the configured [`crate::BlobStore`].
    pub id: Uuid,
    /// SHA-256 digest of the exact source bytes.
    pub sha256: [u8; 32],
    /// Exact source byte length.
    pub byte_len: u64,
}

impl DocumentBlob {
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

/// Authoritative source content accepted after synchronous decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentSourceUpsert {
    /// Stable document identifier.
    pub id: DocumentId,
    /// Owning conversation for conversation-scoped sources.
    pub chat_id: Option<ChatId>,
    /// Owning project, or `None` for a legacy unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, when known.
    pub origin_uri: Option<String>,
    /// Media type used to select the parser.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Immutable source bytes already published to the blob store.
    pub source_blob: DocumentBlob,
    /// Parsed text-of-record.
    pub canonical_text: String,
    /// Source metadata timestamp; workflow timestamps remain store-owned.
    pub updated_at: DateTime<Utc>,
}

/// An authoritative source document and its synchronously decoded text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRecord {
    /// Stable document identifier.
    pub id: DocumentId,
    /// Owning conversation for conversation-scoped sources.
    pub chat_id: Option<ChatId>,
    /// Owning project, or `None` for a legacy unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub origin_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Retained original raw bytes, when available.
    pub source_blob: Option<DocumentBlob>,
    /// Parsed text-of-record used by source readers.
    pub canonical_text: String,
    /// When this record was first created.
    pub created_at: DateTime<Utc>,
    /// When authoritative content or metadata last changed.
    pub updated_at: DateTime<Utc>,
}

impl DocumentRecord {
    /// Whether this source holds text a reader can be given.
    #[must_use]
    pub fn is_readable(&self) -> bool {
        !self.canonical_text.is_empty()
    }
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

/// What a turn's staged write-back did to one file in a granted folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecFileChange {
    /// The folder had no such file before the turn.
    Created,
    /// The folder held different bytes, retained as this row's prior blob.
    Overwritten,
    /// The folder held the file and the turn removed it.
    Deleted,
}

impl ExecFileChange {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Overwritten => "overwritten",
            Self::Deleted => "deleted",
        }
    }

    /// Parse the stable representation written to the database.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "created" => Some(Self::Created),
            "overwritten" => Some(Self::Overwritten),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// Why one staged file was left out of the user's folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ExecFileRejectionReason {
    /// The destination changed after the turn's writable copy was staged.
    Stale,
    /// The bytes that would be destroyed could not be retained for undo.
    SnapshotUnavailable,
    /// The staged replacement exceeded the write-back ceiling.
    StagedFileTooLarge,
    /// A recoverable copy could not be placed in the operating system's trash.
    TrashUnavailable,
    /// The path or filesystem operation could not be used safely.
    Unavailable,
}

impl ExecFileRejectionReason {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stale => "stale",
            Self::SnapshotUnavailable => "snapshot_unavailable",
            Self::StagedFileTooLarge => "staged_file_too_large",
            Self::TrashUnavailable => "trash_unavailable",
            Self::Unavailable => "unavailable",
        }
    }

    /// Parse the stable representation written to the database.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stale" => Some(Self::Stale),
            "snapshot_unavailable" => Some(Self::SnapshotUnavailable),
            "staged_file_too_large" => Some(Self::StagedFileTooLarge),
            "trash_unavailable" => Some(Self::TrashUnavailable),
            "unavailable" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

/// Whether the prior bytes for one journaled change are actually recoverable.
///
/// This is recorded rather than inferred because "we did not snapshot" and "we
/// snapshotted nothing because nothing was there" are different answers to the
/// user. A file too big to stash is written anyway — refusing the agent's work
/// over a storage bound would be worse — but it is marked here so the change
/// summary can say plainly that this one cannot be reverted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecUndoState {
    /// The prior bytes are in the blob store, or there were none to keep.
    Available,
    /// The prior file exceeded [`MAX_EXEC_SNAPSHOT_BYTES`]; no undo for it.
    PriorTooLarge,
    /// The prior file existed but could not be read; no undo for it.
    PriorUnreadable,
}

impl ExecUndoState {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::PriorTooLarge => "prior_too_large",
            Self::PriorUnreadable => "prior_unreadable",
        }
    }

    /// Parse the stable representation written to the database.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "available" => Some(Self::Available),
            "prior_too_large" => Some(Self::PriorTooLarge),
            "prior_unreadable" => Some(Self::PriorUnreadable),
            _ => None,
        }
    }
}

/// Ceiling on the prior bytes retained for one file.
///
/// A snapshot is a whole second copy of the file in the blob store, so it is
/// capped tighter than what the overlay is willing to write back. A larger file
/// is still written; its row records [`ExecUndoState::PriorTooLarge`].
pub const MAX_EXEC_SNAPSHOT_BYTES: u64 = 32 * 1_024 * 1_024;

/// Turns of file-change history one chat retains.
///
/// This bound *is* the undo window, so it is deliberately a count of turns
/// rather than an age: a chat left alone for a month should still offer undo on
/// its last exchange, and a chat that has run a hundred turns since is one whose
/// early changes the user has long stopped thinking of as undoable. Snapshot
/// blobs for turns past this bound lose their last reference and are reclaimed
/// by the ordinary retirement path.
pub const EXEC_SNAPSHOT_RETAINED_TURNS: usize = 20;

/// One journaled file change from a turn's staged write-back.
///
/// `prior_blob_id` doubles as the prior digest: blob ids are content-derived, so
/// the id both addresses the retained bytes and identifies them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecFileSnapshotRecord {
    /// Absolute path of the granted folder the change landed in.
    pub folder_path: String,
    /// Path of the changed file relative to that folder.
    pub relative_path: String,
    /// What the write-back did.
    pub change: ExecFileChange,
    /// Content address of the bytes the folder held before the change, absent
    /// when there were none or when they could not be retained.
    pub prior_blob_id: Option<Uuid>,
    /// Length of those prior bytes, recorded even when they were not retained.
    pub prior_byte_len: Option<u64>,
    /// Lowercase hex SHA-256 of the bytes written, absent for a deletion.
    pub new_sha256: Option<String>,
    /// Whether this change can be reverted.
    pub undo: ExecUndoState,
}

/// One persisted [`ExecFileSnapshotRecord`] with its journal identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecFileSnapshot {
    /// Row identity.
    pub id: Uuid,
    /// Conversation whose turn made the change.
    pub chat_id: ChatId,
    /// Turn that made the change.
    pub turn_id: TurnId,
    /// When the write-back journaled it.
    pub recorded_at: DateTime<Utc>,
    /// The change itself.
    pub file: ExecFileSnapshotRecord,
}

/// One staged file that a turn could not safely materialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecFileRejectionRecord {
    /// Absolute path of the granted folder the change targeted.
    pub folder_path: String,
    /// Path of the staged file relative to that folder.
    pub relative_path: String,
    /// Server-owned classification of why the write did not land.
    pub reason: ExecFileRejectionReason,
}

/// One persisted [`ExecFileRejectionRecord`] with its journal identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecFileRejection {
    /// Row identity.
    pub id: Uuid,
    /// Conversation whose turn attempted the change.
    pub chat_id: ChatId,
    /// Turn that attempted the change.
    pub turn_id: TurnId,
    /// When the rejected write was journaled.
    pub recorded_at: DateTime<Utc>,
    /// The rejected staged file.
    pub file: ExecFileRejectionRecord,
}

/// Metadata returned by bounded document listings.
///
/// This deliberately excludes canonical content so list callers cannot
/// accidentally load large source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSummaryRecord {
    /// Stable document identifier.
    pub id: DocumentId,
    /// Owning conversation for conversation-scoped sources.
    pub chat_id: Option<ChatId>,
    /// Owning project, or `None` for a legacy unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub origin_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Exact retained source byte length, when original bytes are available.
    pub source_byte_len: Option<u64>,
    /// Whether the source holds text a reader can be given.
    pub readable: bool,
    /// When this record was first created.
    pub created_at: DateTime<Utc>,
    /// When authoritative content or metadata last changed.
    pub updated_at: DateTime<Utc>,
}

impl DocumentSummaryRecord {
    /// What a caller can do with this source right now.
    #[must_use]
    pub const fn readiness(&self) -> DocumentReadiness {
        DocumentReadiness::of(self.readable)
    }
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
/// Repeated writes use last-write-wins semantics while preserving `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentUpsert {
    /// Stable document identifier.
    pub id: DocumentId,
    /// Owning conversation for conversation-scoped sources.
    pub chat_id: Option<ChatId>,
    /// Owning project, or `None` for a legacy unscoped document.
    pub project_id: Option<ProjectId>,
    /// Source path or URL, or `None` for content supplied inline.
    pub origin_uri: Option<String>,
    /// Media type of the canonical content.
    pub media_type: String,
    /// Optional human-facing title.
    pub title: Option<String>,
    /// Parsed text-of-record.
    pub canonical_text: String,
    /// Time of this authoritative write.
    pub updated_at: DateTime<Utc>,
}

/// Corpus ownership filter for document listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DocumentScope {
    /// Every document, for maintenance only.
    All,
    /// Only explicitly legacy projectless, conversationless documents.
    Unscoped,
    /// Only documents owned by this project.
    Project(ProjectId),
    /// Only documents owned by this conversation.
    Chat(ChatId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    #[test]
    fn source_blob_identity_is_deterministic_and_content_addressed() {
        let first = DocumentBlob::from_bytes(b"same source bytes");
        assert_eq!(first, DocumentBlob::from_bytes(b"same source bytes"));
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
            DocumentBlob::from_digest(first.sha256, first.byte_len)
        );
        let mut invalid = first.clone();
        invalid.id = Uuid::new_v4();
        assert!(!invalid.has_content_addressed_id());
        assert_ne!(first.id, DocumentBlob::from_bytes(b"other").id);
    }
}
