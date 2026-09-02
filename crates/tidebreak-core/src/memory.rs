//! Durable, reviewable memory records behind one backend boundary.
//!
//! The domain contract stays independent of any one database or retrieval
//! engine. Backends store markdown bodies verbatim, report every capability,
//! and keep owner scoping at every operation boundary.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

use crate::code::{CodeSessionId, CodeTurnId, RepoId, WorkspaceId};
use crate::id::{ChatId, MessageId, TurnId};
use crate::model::OwnerId;

/// Largest markdown body accepted for one memory record.
pub const MAX_MEMORY_BODY_BYTES: usize = 2 * 1_024;
/// Largest retrieval-hook title accepted for one memory record.
pub const MAX_MEMORY_TITLE_CHARS: usize = 160;
/// Most evidence references one record may carry.
pub const MAX_MEMORY_EVIDENCE: usize = 32;
/// Most typed links one record may carry.
pub const MAX_MEMORY_LINKS: usize = 16;
/// Default number of active records allowed in one scope.
pub const DEFAULT_MEMORY_ACTIVE_RECORD_CAP: usize = 64;
/// Default maximum size of one rendered scope digest.
pub const DEFAULT_MEMORY_DIGEST_BYTES: usize = 8 * 1_024;
/// Largest result page one lexical search may return.
pub const MAX_MEMORY_SEARCH_RESULTS: usize = 100;

macro_rules! memory_id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generate a fresh identifier.
            #[must_use]
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// Borrow the underlying UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Uuid::parse_str(value)?))
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
    };
}

memory_id_type!(
    /// Identifies one durable memory record.
    MemoryRecordId
);
memory_id_type!(
    /// Identifies one immutable record revision.
    MemoryRevisionId
);

/// One durable memory scope.
///
/// The enum is non-exhaustive because wider scopes may arrive later. A
/// repository scope binds to the registered repository, never a workspace.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryScope {
    /// Knowledge that follows the owner across every surface.
    Personal,
    /// Knowledge bound to one registered repository.
    Repo {
        /// Stable registered-repository identity.
        repo_id: RepoId,
    },
}

impl MemoryScope {
    /// Stable database token for the scope variant.
    #[must_use]
    pub const fn kind_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Repo { .. } => "repo",
        }
    }

    /// Repository identity carried by a repository scope.
    #[must_use]
    pub const fn repo_id(self) -> Option<RepoId> {
        match self {
            Self::Personal => None,
            Self::Repo { repo_id } => Some(repo_id),
        }
    }
}

/// What kind of durable knowledge a record carries.
// The variants stay undocumented so the `schemars` derive generates a plain
// `enum` list rather than `oneOf` + `const`, which the strict schema subset
// providers enforce cannot express.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Fact,
    Preference,
    Lesson,
    Reference,
}

impl MemoryKind {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Preference => "preference",
            Self::Lesson => "lesson",
            Self::Reference => "reference",
        }
    }

    /// Heading used by the deterministic digest renderer.
    #[must_use]
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Fact => "Facts",
            Self::Preference => "Preferences",
            Self::Lesson => "Lessons",
            Self::Reference => "References",
        }
    }
}

/// Lifecycle state for one memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// A weak pattern that needs observations from more distinct sessions.
    Tracking,
    /// A model-authored candidate waiting for user review.
    Proposed,
    /// An authoritative record eligible for context assembly.
    Active,
    /// A retained historical record that no longer carries authority.
    Archived,
    /// A dismissed proposal retained long enough to suppress repetition.
    Rejected,
}

impl MemoryStatus {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tracking => "tracking",
            Self::Proposed => "proposed",
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Rejected => "rejected",
        }
    }

    /// Whether this state contributes to the memory digest.
    #[must_use]
    pub const fn is_authoritative(self) -> bool {
        matches!(self, Self::Active)
    }

    /// Whether `next` follows the persisted lifecycle.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        self == next
            || matches!(
                (self, next),
                (
                    Self::Tracking,
                    Self::Proposed | Self::Rejected | Self::Archived
                ) | (
                    Self::Proposed,
                    Self::Active | Self::Rejected | Self::Archived
                ) | (Self::Active, Self::Archived)
            )
    }
}

/// Who authored a memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthor {
    /// The owner wrote or edited the record directly.
    User,
    /// Tidebreak's utility model derived the record.
    Model,
    /// A one-time import supplied the record.
    Import,
}

impl MemoryAuthor {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Model => "model",
            Self::Import => "import",
        }
    }
}

/// The product surface that produced a record, where known.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct MemoryOrigin {
    /// Originating work-mode conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<ChatId>,
    /// Originating work-mode turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    /// Originating code session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_session_id: Option<CodeSessionId>,
    /// Originating code turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_turn_id: Option<CodeTurnId>,
    /// Workspace attached to the originating code session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
}

/// One exact source that justifies a model-authored record.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryEvidence {
    /// One durable work-mode message.
    Message {
        /// Exact message identity.
        message_id: MessageId,
    },
    /// One durable code-journal event.
    CodeEvent {
        /// Session that owns the event sequence.
        session_id: CodeSessionId,
        /// One-based event sequence.
        seq: i64,
    },
}

/// Authorship and source evidence for one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryProvenance {
    /// Who authored the record.
    pub author: MemoryAuthor,
    /// Product origin, where known.
    pub origin: MemoryOrigin,
    /// Exact sources that justify the record.
    pub evidence: Vec<MemoryEvidence>,
}

/// Meaning of one link to another memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLinkRelation {
    /// The linked record is relevant but remains independent.
    Related,
    /// This proposal updates the linked active record.
    Updates,
    /// Activating this proposal supersedes the linked source record.
    Supersedes,
}

/// One typed link to another memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub struct MemoryLink {
    /// Linked record identity.
    pub record_id: MemoryRecordId,
    /// Why this record links to it.
    pub relation: MemoryLinkRelation,
}

/// One durable markdown memory with its typed envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryRecord {
    /// Stable record identity.
    pub id: MemoryRecordId,
    /// Personal or repository scope.
    pub scope: MemoryScope,
    /// Knowledge category.
    pub kind: MemoryKind,
    /// Review and authority state.
    pub status: MemoryStatus,
    /// One-line retrieval hook that says when the record matters.
    pub title: String,
    /// Markdown body, stored verbatim.
    pub body: String,
    /// Authorship, origin, and exact evidence.
    pub provenance: MemoryProvenance,
    /// Typed relationships to other records.
    pub links: Vec<MemoryLink>,
    /// Mechanical expiry, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Active replacement for an archived record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryRecordId>,
    /// Distinct supporting observations for a tracked hypothesis.
    pub observation_count: u32,
    /// Compare-and-swap revision. The first stored version is 1.
    pub revision: i64,
    /// Original creation time.
    pub created_at: DateTime<Utc>,
    /// Time of the latest committed mutation.
    pub updated_at: DateTime<Utc>,
}

impl MemoryRecord {
    /// Validate the backend-independent record shape.
    pub fn validate(&self) -> MemoryResult<()> {
        if self.title.trim().is_empty() {
            return Err(MemoryError::InvalidRecord(
                "memory title must not be empty".to_owned(),
            ));
        }
        if self.title.contains(['\r', '\n']) {
            return Err(MemoryError::InvalidRecord(
                "memory title must fit on one line".to_owned(),
            ));
        }
        if self.title.chars().count() > MAX_MEMORY_TITLE_CHARS {
            return Err(MemoryError::InvalidRecord(format!(
                "memory title exceeds {MAX_MEMORY_TITLE_CHARS} characters"
            )));
        }
        if self.body.trim().is_empty() {
            return Err(MemoryError::InvalidRecord(
                "memory body must not be empty".to_owned(),
            ));
        }
        if self.body.len() > MAX_MEMORY_BODY_BYTES {
            return Err(MemoryError::InvalidRecord(format!(
                "memory body exceeds {MAX_MEMORY_BODY_BYTES} bytes"
            )));
        }
        if self.provenance.evidence.len() > MAX_MEMORY_EVIDENCE {
            return Err(MemoryError::InvalidRecord(format!(
                "memory record exceeds {MAX_MEMORY_EVIDENCE} evidence references"
            )));
        }
        if self.provenance.author == MemoryAuthor::Model
            && self.provenance.evidence.is_empty()
            && !self.links.iter().any(|link| {
                matches!(
                    link.relation,
                    MemoryLinkRelation::Updates | MemoryLinkRelation::Supersedes
                )
            })
        {
            return Err(MemoryError::InvalidRecord(
                "model-authored memory requires resolvable evidence or source links".to_owned(),
            ));
        }
        if self.links.len() > MAX_MEMORY_LINKS {
            return Err(MemoryError::InvalidRecord(format!(
                "memory record exceeds {MAX_MEMORY_LINKS} links"
            )));
        }
        let mut linked = std::collections::HashSet::with_capacity(self.links.len());
        for link in &self.links {
            if link.record_id == self.id {
                return Err(MemoryError::InvalidRecord(
                    "memory record cannot link to itself".to_owned(),
                ));
            }
            if !linked.insert((link.record_id, link.relation)) {
                return Err(MemoryError::InvalidRecord(
                    "memory record contains a duplicate link".to_owned(),
                ));
            }
        }
        if self.status == MemoryStatus::Tracking && self.observation_count == 0 {
            return Err(MemoryError::InvalidRecord(
                "tracked memory requires at least one observation".to_owned(),
            ));
        }
        if self.revision < 1 {
            return Err(MemoryError::InvalidRecord(
                "memory revision must be at least 1".to_owned(),
            ));
        }
        if self.updated_at < self.created_at {
            return Err(MemoryError::InvalidRecord(
                "memory update time precedes its creation time".to_owned(),
            ));
        }
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= self.created_at)
        {
            return Err(MemoryError::InvalidRecord(
                "memory expiry must follow its creation time".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Mutable fields supplied when editing one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryRecordUpdate {
    /// Record to update.
    pub id: MemoryRecordId,
    /// Revision the editor read.
    pub expected_revision: i64,
    /// Replacement knowledge category.
    pub kind: MemoryKind,
    /// Replacement one-line retrieval hook.
    pub title: String,
    /// Replacement markdown body.
    pub body: String,
    /// Replacement provenance.
    pub provenance: MemoryProvenance,
    /// Replacement links.
    pub links: Vec<MemoryLink>,
    /// Replacement expiry.
    pub expires_at: Option<DateTime<Utc>>,
    /// Replacement observation count.
    pub observation_count: u32,
}

/// A compare-and-swap lifecycle mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryStatusChange {
    /// Record to mutate.
    pub id: MemoryRecordId,
    /// Revision the caller read.
    pub expected_revision: i64,
    /// Desired lifecycle state.
    pub status: MemoryStatus,
}

/// Filter for owner-scoped record listings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
pub struct MemoryListFilter {
    /// Exact scope, or every owner scope when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<MemoryScope>,
    /// Included states. An empty list includes every state.
    #[serde(default)]
    pub statuses: Vec<MemoryStatus>,
    /// Included kinds. An empty list includes every kind.
    #[serde(default)]
    pub kinds: Vec<MemoryKind>,
}

/// One full-content lexical-search request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemorySearchRequest {
    /// Case-insensitive search text.
    pub query: String,
    /// Exact scope, or every owner scope when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<MemoryScope>,
    /// Included states. An empty list includes every state.
    #[serde(default)]
    pub statuses: Vec<MemoryStatus>,
    /// Maximum results to return.
    pub limit: usize,
}

/// One injectable lexical-search result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemorySearchHit {
    /// Matching record.
    pub record_id: MemoryRecordId,
    /// Retrieval-hook title.
    pub title: String,
    /// Date of the latest committed mutation.
    pub updated_at: DateTime<Utc>,
    /// First matching markdown line, kept verbatim.
    pub matching_line: String,
    /// Deterministic lexical score. Larger values sort first.
    pub score: u32,
}

/// One deterministic active-record digest for a scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryDigest {
    /// Scope represented by this render.
    pub scope: MemoryScope,
    /// Rendered markdown.
    pub markdown: String,
    /// UTF-8 byte length of `markdown`.
    pub byte_len: usize,
    /// Maximum accepted render size.
    pub byte_cap: usize,
    /// Number of active records represented.
    pub record_count: usize,
}

/// One immutable historical snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryRevision {
    /// Stable revision-row identity.
    pub id: MemoryRevisionId,
    /// Record that owns the revision.
    pub record_id: MemoryRecordId,
    /// Monotonic per-record ordinal.
    pub ordinal: i64,
    /// Full record snapshot after the mutation committed.
    pub snapshot: MemoryRecord,
    /// Time the snapshot committed.
    pub created_at: DateTime<Utc>,
}

/// Whether a backend guarantees that a returned write is durable now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryWriteState {
    /// The backend committed the record and revision before returning.
    Committed,
    /// The backend accepted the write for asynchronous completion.
    Pending,
}

/// A record write plus the backend's durability guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryWriteReceipt {
    /// Durability guarantee at the backend boundary.
    pub state: MemoryWriteState,
    /// Record state accepted by the backend.
    pub record: MemoryRecord,
}

/// Intent supplied to an extraction-capable backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryIngestRequest {
    /// Scope to receive extracted records.
    pub scope: MemoryScope,
    /// Exact source origin.
    pub origin: MemoryOrigin,
    /// Source text supplied for bounded extraction.
    pub content: String,
}

/// Accepted extraction work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryIngestReceipt {
    /// Durability guarantee at the backend boundary.
    pub state: MemoryWriteState,
    /// Records written synchronously, if any.
    pub records: Vec<MemoryRecord>,
}

/// Whether a backend supports one declared operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCapLevel {
    /// The backend implements and verifies the capability.
    Supported,
    /// The backend is known not to implement the capability.
    Unsupported,
    /// The backend cannot state support honestly.
    Unknown,
}

/// Named backend capability used in visible degradation errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCapability {
    Extraction,
    LexicalSearch,
    SemanticSearch,
    Consolidation,
    ContextAssembly,
    RevisionHistory,
    VerifiedDelete,
    AsynchronousWrites,
    AgentEditableSurfaces,
}

impl std::fmt::Display for MemoryCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Extraction => "extraction",
            Self::LexicalSearch => "lexical search",
            Self::SemanticSearch => "semantic search",
            Self::Consolidation => "consolidation",
            Self::ContextAssembly => "context assembly",
            Self::RevisionHistory => "revision history",
            Self::VerifiedDelete => "verified delete",
            Self::AsynchronousWrites => "asynchronous writes",
            Self::AgentEditableSurfaces => "agent-editable surfaces",
        };
        formatter.write_str(name)
    }
}

/// Complete capability vector for one memory backend.
///
/// This type has no `Default`. Adding a flag breaks every backend until the
/// backend states a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemoryCaps {
    pub extraction: MemoryCapLevel,
    pub lexical_search: MemoryCapLevel,
    pub semantic_search: MemoryCapLevel,
    pub consolidation: MemoryCapLevel,
    pub context_assembly: MemoryCapLevel,
    pub revision_history: MemoryCapLevel,
    pub verified_delete: MemoryCapLevel,
    pub asynchronous_writes: MemoryCapLevel,
    pub agent_editable_surfaces: MemoryCapLevel,
}

/// Failure at the memory-backend boundary.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryError {
    /// The backend does not implement the requested capability.
    #[error("memory backend does not support {0}")]
    Unsupported(MemoryCapability),
    /// The caller supplied a malformed record or request.
    #[error("invalid memory record: {0}")]
    InvalidRecord(String),
    /// One exact evidence reference does not resolve for this owner.
    #[error("memory evidence does not resolve: {0}")]
    EvidenceNotFound(String),
    /// The requested scope does not exist for this owner.
    #[error("memory scope does not exist")]
    ScopeNotFound,
    /// The requested record does not exist for this owner.
    #[error("memory record not found")]
    NotFound,
    /// A new record reused an existing durable identity.
    #[error("memory record already exists")]
    AlreadyExists,
    /// A compare-and-swap edit lost to a newer revision.
    #[error("memory record has moved on to revision {current_revision}")]
    RevisionConflict {
        /// Revision that is current now.
        current_revision: i64,
    },
    /// A lifecycle edge is not allowed.
    #[error("memory status cannot move from {from:?} to {to:?}")]
    InvalidStatusTransition {
        from: MemoryStatus,
        to: MemoryStatus,
    },
    /// The scope already contains its maximum active-record count.
    #[error(
        "memory scope has reached its active-record cap of {cap}; consolidate overlapping records, archive a record that is no longer true, or update an existing record"
    )]
    ActiveRecordCapExceeded { cap: usize },
    /// The active title digest would exceed its byte budget.
    #[error(
        "memory digest would exceed its {cap}-byte cap; consolidate overlapping records, archive a record that is no longer true, or shorten a retrieval title"
    )]
    DigestCapExceeded { cap: usize },
    /// Persistence or backend-internal failure.
    #[error("memory backend error: {0}")]
    Backend(String),
}

/// Result returned by a memory backend.
pub type MemoryResult<T> = std::result::Result<T, MemoryError>;

/// Why one maintenance pass ended the way it did for an owner.
///
/// Mechanical expiry runs on every pass; the outcome names what happened to
/// the bounded consolidation step, which is the part that can wait, park, or
/// need a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MemorySweepOutcome {
    /// The utility model proposed a merge for review.
    Proposed,
    /// The utility model looked at a changed scope and found nothing to merge.
    Declined,
    /// The last proposal was dismissed and the record set has not changed.
    Parked,
    /// No scope's active record set changed since its last completed try.
    Unchanged,
    /// The owner had an active turn, so the model step waited.
    OwnerBusy,
    /// No utility model resolves; expiry still ran.
    NoModel,
    /// The per-owner rate bound held the model step for a later pass.
    RateLimited,
}

impl MemorySweepOutcome {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Declined => "declined",
            Self::Parked => "parked",
            Self::Unchanged => "unchanged",
            Self::OwnerBusy => "owner_busy",
            Self::NoModel => "no_model",
            Self::RateLimited => "rate_limited",
        }
    }

    /// Parse one stable database token.
    pub fn parse(token: &str) -> MemoryResult<Self> {
        match token {
            "proposed" => Ok(Self::Proposed),
            "declined" => Ok(Self::Declined),
            "parked" => Ok(Self::Parked),
            "unchanged" => Ok(Self::Unchanged),
            "owner_busy" => Ok(Self::OwnerBusy),
            "no_model" => Ok(Self::NoModel),
            "rate_limited" => Ok(Self::RateLimited),
            other => Err(MemoryError::Backend(format!(
                "invalid memory sweep outcome {other:?}"
            ))),
        }
    }
}

/// The maintenance sweep's last completed pass for one owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemorySweepRun {
    /// When the pass completed.
    pub ran_at: DateTime<Utc>,
    /// The scope the consolidation step considered, when one was picked.
    pub scope: Option<MemoryScope>,
    /// What happened to the consolidation step.
    pub outcome: MemorySweepOutcome,
    /// Records mechanically archived by this pass.
    pub expired: u32,
    /// Merge proposals this pass stored for review.
    pub proposed: u32,
}

/// Answer of `GET /memory/sweep`: the last run, or `None` before the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct MemorySweepStatus {
    /// The most recent completed pass for the caller.
    pub last_run: Option<MemorySweepRun>,
}

/// Durable per-scope sweep state: the standing-condition fingerprint and the
/// proposal the last completed try produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySweepScopeState {
    /// Scope this row fingerprints.
    pub scope: MemoryScope,
    /// Fingerprint of the active record set at the last completed try.
    pub fingerprint: String,
    /// Merge proposal the last completed try stored, if it stored one.
    pub proposal_id: Option<MemoryRecordId>,
    /// When the last utility-model step ran for this scope.
    pub last_model_step_at: Option<DateTime<Utc>>,
}

/// Storage, retrieval, review, and context assembly for durable memory.
#[async_trait]
pub trait MemoryBackend: Send + Sync {
    /// State every capability explicitly.
    fn caps(&self) -> MemoryCaps;

    /// Store one caller-supplied record verbatim and append its first revision.
    async fn put(&self, owner: &OwnerId, record: MemoryRecord) -> MemoryResult<MemoryWriteReceipt>;

    /// Ask an extraction-capable backend to derive records from source text.
    async fn ingest(
        &self,
        owner: &OwnerId,
        request: MemoryIngestRequest,
    ) -> MemoryResult<MemoryIngestReceipt>;

    /// Load one owner-scoped record.
    async fn get(&self, owner: &OwnerId, id: MemoryRecordId) -> MemoryResult<Option<MemoryRecord>>;

    /// List owner-scoped records in deterministic newest-first order.
    async fn list(
        &self,
        owner: &OwnerId,
        filter: MemoryListFilter,
    ) -> MemoryResult<Vec<MemoryRecord>>;

    /// Replace one record's editable envelope and body.
    async fn update(
        &self,
        owner: &OwnerId,
        update: MemoryRecordUpdate,
    ) -> MemoryResult<MemoryWriteReceipt>;

    /// Move one record through its lifecycle.
    async fn set_status(
        &self,
        owner: &OwnerId,
        change: MemoryStatusChange,
    ) -> MemoryResult<MemoryWriteReceipt>;

    /// Hard-delete one record and every revision.
    async fn delete(&self, owner: &OwnerId, id: MemoryRecordId) -> MemoryResult<bool>;

    /// Search complete markdown bodies and retrieval titles in process.
    async fn search(
        &self,
        owner: &OwnerId,
        request: MemorySearchRequest,
    ) -> MemoryResult<Vec<MemorySearchHit>>;

    /// Render every active title in one scope into a deterministic digest.
    async fn assemble_context(
        &self,
        owner: &OwnerId,
        scope: MemoryScope,
    ) -> MemoryResult<MemoryDigest>;

    /// Return immutable snapshots, oldest first.
    async fn revision_history(
        &self,
        owner: &OwnerId,
        id: MemoryRecordId,
    ) -> MemoryResult<Vec<MemoryRevision>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_are_constructed_exhaustively() {
        let caps = MemoryCaps {
            extraction: MemoryCapLevel::Unsupported,
            lexical_search: MemoryCapLevel::Supported,
            semantic_search: MemoryCapLevel::Unsupported,
            consolidation: MemoryCapLevel::Unsupported,
            context_assembly: MemoryCapLevel::Supported,
            revision_history: MemoryCapLevel::Supported,
            verified_delete: MemoryCapLevel::Supported,
            asynchronous_writes: MemoryCapLevel::Unsupported,
            agent_editable_surfaces: MemoryCapLevel::Supported,
        };
        assert_eq!(caps.lexical_search, MemoryCapLevel::Supported);
        assert_eq!(caps.semantic_search, MemoryCapLevel::Unsupported);
    }

    #[test]
    fn a_model_record_requires_evidence() {
        let now = Utc::now();
        let record = MemoryRecord {
            id: MemoryRecordId::new(),
            scope: MemoryScope::Personal,
            kind: MemoryKind::Fact,
            status: MemoryStatus::Proposed,
            title: "When preparing releases".to_owned(),
            body: "Use the release checklist.".to_owned(),
            provenance: MemoryProvenance {
                author: MemoryAuthor::Model,
                origin: MemoryOrigin::default(),
                evidence: Vec::new(),
            },
            links: Vec::new(),
            expires_at: None,
            superseded_by: None,
            observation_count: 0,
            revision: 1,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(
            record.validate(),
            Err(MemoryError::InvalidRecord(
                "model-authored memory requires resolvable evidence or source links".to_owned()
            ))
        );
    }
}
