//! The storage seams every client sits on.
//!
//! Three traits, deliberately backend-agnostic so a profile can wire different
//! implementations without touching callers:
//!
//! - [`Store`] — durable metadata/state (chats, messages, settings). The
//!   default impl is SQLite; the same trait maps to Postgres for self-host.
//! - [`SecretProvider`] — credentials (model API keys, connection tokens). These
//!   live in the OS keychain (desktop) or a KMS/Vault (server) and are **never**
//!   written to the [`Store`]; the store only holds opaque secret references.
//! - [`BlobStore`] — bytes (documents, images, exports), served locally or from
//!   object storage.
//!
//! Only the entities that exist today are modeled here. Persistence for
//! connections, documents, and skills is added alongside the slices that
//! introduce those record types.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{ChatId, DocumentId, DocumentJobId, ProjectId};
use crate::model::{
    Chat, DocumentGeneration, DocumentJob, DocumentJobKind, DocumentJobStatus, DocumentListCursor,
    DocumentParseOutput, DocumentRecord, DocumentScope, DocumentSourceUpsert,
    DocumentSummaryRecord, DocumentUpsert, Message, Project, ToolCallRecord,
};

/// Why maintenance determined that a document needs an index job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentIndexJobReason {
    /// The operational watermark is current, but the derived generation is absent.
    DerivedStateMissing,
    /// The desired generation exists only partially and cannot be safely reused.
    DerivedStateIncomplete,
    /// The configured chunking/embedding pipeline differs from the indexed one.
    PipelineChanged,
}

impl DocumentIndexJobReason {
    /// Whether this repair must publish under a fresh generation fence.
    #[must_use]
    #[allow(dead_code)]
    pub(crate) const fn advances_generation(self) -> bool {
        matches!(self, Self::DerivedStateIncomplete | Self::PipelineChanged)
    }
}

/// Result of atomically ensuring one desired document index job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureDocumentIndexJobOutcome {
    /// A new job was inserted or an exact terminal job was reset to queued.
    Enqueued(DocumentJob),
    /// The desired current job already exists and requires no state change.
    Existing(DocumentJob),
    /// The desired current job failed and requires an explicit user retry.
    Failed(DocumentJob),
    /// Canonical content is still owned by the current parse stage.
    Parsing(DocumentJob),
    /// The source document no longer exists.
    MissingDocument,
    /// The caller inspected an obsolete source generation.
    GenerationChanged(DocumentGeneration),
}

/// Result of atomically ensuring canonical output from one parser pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureDocumentParseJobOutcome {
    /// A new Parse job was inserted for the desired generation.
    Enqueued(DocumentJob),
    /// The desired current Parse job already exists and remains live.
    Existing(DocumentJob),
    /// The desired current Parse job exhausted its attempts.
    Failed(DocumentJob),
    /// Canonical output already came from the desired parser.
    CanonicalCurrent,
    /// Reparse was requested for a document without retained source bytes.
    SourceUnavailable,
    /// The source document no longer exists.
    MissingDocument,
    /// The caller inspected an obsolete source generation.
    GenerationChanged(DocumentGeneration),
}

fn document_storage_unavailable<T>() -> Result<T> {
    Err(AgentError::Store(
        "document storage is not implemented by this Store".into(),
    ))
}

/// Durable metadata and conversation state.
///
/// Implementations must be safe to share across threads (`Send + Sync`) and are
/// held behind `Arc<dyn Store>`, so this trait stays object-safe.
#[async_trait]
pub trait Store: Send + Sync {
    /// Persist a new project.
    async fn create_project(&self, project: &Project) -> Result<()>;

    /// Fetch a project by id, or `None` if it doesn't exist.
    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>>;

    /// List projects, most-recently-created first.
    async fn list_projects(&self) -> Result<Vec<Project>>;

    /// Persist a new authoritative document record.
    ///
    /// `project_id`, when present, must identify an existing project. The default
    /// database store enforces this with a restricting foreign key, so projects
    /// cannot be deleted while they still own documents. The store replaces the
    /// supplied `revision_token` with a fresh token so a deleted document
    /// lifecycle cannot be recreated with stale identity. A live document's
    /// ownership is immutable: callers must delete it before recreating the same
    /// id in another corpus.
    async fn create_document(&self, _document: &DocumentRecord) -> Result<()> {
        document_storage_unavailable()
    }

    /// Fetch an authoritative document by id, or `None` if it does not exist.
    async fn get_document(&self, _id: DocumentId) -> Result<Option<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List documents in `scope`, most-recently-created first.
    async fn list_documents(&self, _scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        document_storage_unavailable()
    }

    /// List document metadata in deterministic newest-first order.
    ///
    /// At most `limit` records are returned. When `after` is present, results
    /// begin strictly after its `(created_at, id)` tuple in descending display
    /// order. Implementations must not load canonical text or revision tokens.
    async fn list_document_summaries(
        &self,
        _scope: DocumentScope,
        _after: Option<DocumentListCursor>,
        _limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        document_storage_unavailable()
    }

    /// List document ids in `scope` without requiring canonical content.
    ///
    /// The default preserves compatibility for external stores; database-backed
    /// implementations should project only the id column for maintenance scans.
    async fn list_document_ids(&self, scope: DocumentScope) -> Result<Vec<DocumentId>> {
        Ok(self
            .list_documents(scope)
            .await?
            .into_iter()
            .map(|document| document.id)
            .collect())
    }

    /// Read the newest durable generation, including a hard-delete tombstone.
    async fn get_document_generation(&self, _id: DocumentId) -> Result<Option<DocumentGeneration>> {
        document_storage_unavailable()
    }

    /// List durable tombstone watermarks whose retrieval retirement is unfinished.
    ///
    /// Results are ordered by document id, strictly after `after` when present,
    /// and bounded by `limit`. A worker can advance past a poison entry and wrap
    /// to the beginning by issuing a later scan with `after = None`.
    async fn list_pending_document_retirements(
        &self,
        _after: Option<DocumentId>,
        _limit: u64,
    ) -> Result<Vec<(DocumentId, DocumentGeneration)>> {
        document_storage_unavailable()
    }

    /// Read the exact retirement watermark currently pending for one document.
    async fn get_pending_document_retirement(
        &self,
        _id: DocumentId,
    ) -> Result<Option<DocumentGeneration>> {
        document_storage_unavailable()
    }

    /// Mark one exact tombstone generation's retrieval retirement complete.
    ///
    /// Returns `false` when that exact generation is no longer pending. A live
    /// recreation may coexist with an older pending retirement watermark.
    async fn complete_document_retirement(
        &self,
        _id: DocumentId,
        _generation: DocumentGeneration,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Hard-delete source content and return its durable tombstone generation.
    ///
    /// The generation clock is retained without source content. Repeated deletion
    /// returns the same tombstone; deleting a never-seen id creates revision one.
    async fn delete_document(&self, _id: DocumentId) -> Result<DocumentGeneration> {
        document_storage_unavailable()
    }

    /// Create or replace authoritative document content.
    ///
    /// A never-seen id starts at revision one. Replacing or recreating an id
    /// increments its retained generation atomically, preserves `created_at` only
    /// for a live replacement, and clears the index watermark. `project_id`, when
    /// present, must identify an existing project. A live document cannot move
    /// between the unscoped and project corpora, or between projects; direct
    /// upserts enforce the same ownership rules as the enqueueing write path.
    async fn upsert_document(&self, _document: &DocumentUpsert) -> Result<DocumentRecord> {
        document_storage_unavailable()
    }

    /// Atomically persist a new source revision and enqueue its index job.
    ///
    /// Any older nonterminal job for the document is cancelled in the same
    /// transaction. The returned job is bound to the returned record's exact
    /// `(content_revision, revision_token)` identity. Repeating identical source
    /// content and pipeline fingerprint returns that exact revision/job without
    /// allocating another, including its original `max_attempts` and terminal
    /// status. Intentional reprocessing/retry is an explicit job-state transition,
    /// not another source write. `DocumentUpsert::updated_at` is source metadata
    /// and is deliberately excluded from retry identity. Workflow timestamps are
    /// owned by the store rather than copied from source metadata. `project_id`
    /// must identify an existing project when present, and ownership of a live
    /// document is immutable until that document is deleted.
    async fn upsert_document_and_enqueue_index(
        &self,
        _document: &DocumentUpsert,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        document_storage_unavailable()
    }

    /// Atomically accept immutable raw source bytes and enqueue their parse job.
    ///
    /// The blob must already be durably published. Repeating an identical source
    /// and parser fingerprint returns the exact existing generation and job.
    /// Any source or parser change advances the generation, clears canonical and
    /// index state, and cancels older nonterminal work in the same transaction.
    async fn accept_document_source_and_enqueue_parse(
        &self,
        _document: &DocumentSourceUpsert,
        _parser_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
        document_storage_unavailable()
    }

    /// Atomically publish canonical parser output and enqueue the index stage.
    ///
    /// The transition succeeds only for the exact live, unexpired parse lease.
    /// On success the parse job becomes terminal, canonical state becomes
    /// authoritative, and one index job is queued in the same transaction.
    async fn complete_document_parse_job_and_enqueue_index(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
        _output: &DocumentParseOutput,
        _index_fingerprint: &str,
        _index_max_attempts: i32,
    ) -> Result<Option<(DocumentRecord, DocumentJob)>> {
        document_storage_unavailable()
    }

    /// Atomically establish the current index job requested by an auditor.
    ///
    /// `expected_generation` is a compare-and-swap fence around the auditor's
    /// observation. Missing derived state requeues the exact current generation;
    /// incomplete derived state and a changed pipeline advance the source
    /// generation once without changing source fields. Failed jobs remain failed
    /// until an explicit retry.
    async fn ensure_document_index_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
        _reason: DocumentIndexJobReason,
    ) -> Result<EnsureDocumentIndexJobOutcome> {
        document_storage_unavailable()
    }

    /// Atomically establish the desired Parse job for retained source bytes.
    ///
    /// The caller's observed generation is a compare-and-swap fence. Missing
    /// work for pending canonical output is repaired in that generation; a
    /// parser change advances the generation once, clears derived canonical and
    /// index state, and enqueues Parse without changing retained source fields.
    /// Failed work remains failed until an explicit retry.
    async fn ensure_document_parse_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<EnsureDocumentParseJobOutcome> {
        document_storage_unavailable()
    }

    /// Fetch one durable document job by id.
    async fn get_document_job(&self, _id: DocumentJobId) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// List a document's semantic job history, oldest first.
    async fn list_document_jobs(&self, _document_id: DocumentId) -> Result<Vec<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Explicitly retry one exact-generation failed semantic job.
    ///
    /// A matching failed job is reset to a fresh queued delivery using a
    /// store-owned timestamp and `max_attempts`. Repeating the request while that
    /// exact job is already nonterminal returns it unchanged. The observed
    /// generation, semantic kind, fingerprint, and document stage must all still
    /// agree. Succeeded, cancelled, superseded, missing, or mismatched jobs are
    /// not revived and return `None`.
    async fn retry_document_job(
        &self,
        _document_id: DocumentId,
        _expected_generation: DocumentGeneration,
        _kind: DocumentJobKind,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Atomically claim the oldest due document job and its exact source revision.
    ///
    /// A successful claim increments `attempt_count`, installs a fresh lease,
    /// moves the matching document to `processing`, and returns the running job.
    /// Expired running leases are reclaimed while attempts remain; an expired
    /// final attempt atomically fails the exact current job/document and scanning
    /// continues. Superseded candidates are terminally cancelled rather than
    /// left to block the active-job slot. An exact-identity document with an
    /// impossible lifecycle status is reported as corruption, never cancelled.
    /// `retry_wait` remains user-visible as `queued` during backoff.
    async fn claim_document_job(
        &self,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Extend a live lease owned by `lease_token` without resurrecting expiry.
    ///
    /// Returns `false` if the job is not running, the token differs, the lease
    /// already expired, or the proposed expiry does not extend the current one.
    async fn heartbeat_document_job(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _now: chrono::DateTime<chrono::Utc>,
        _lease_expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Atomically succeed a live index job and publish its exact document
    /// revision as ready in the operational store.
    ///
    /// Returns `false` when the job is no longer running under the exact,
    /// unexpired lease or its timestamp would regress durable state.
    async fn complete_document_index_job(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _completed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Atomically record a live job failure and its matching document state.
    ///
    /// A future `retry_at` moves a job with attempts remaining to `retry_wait`
    /// and its document to `queued`; no retry time, or an exhausted attempt
    /// budget, moves both to terminal `failed`. Returns the resulting job status,
    /// or `None` when the exact live lease no longer owns the job.
    async fn record_document_job_failure(
        &self,
        _id: DocumentJobId,
        _lease_token: uuid::Uuid,
        _failed_at: chrono::DateTime<chrono::Utc>,
        _retry_at: Option<chrono::DateTime<chrono::Utc>>,
        _error_code: &str,
        _error_detail: Option<&str>,
    ) -> Result<Option<DocumentJobStatus>> {
        document_storage_unavailable()
    }

    /// Mark an exact `(revision, revision_token)` as indexed with `fingerprint`.
    ///
    /// `fingerprint` must not be empty.
    ///
    /// Returns `false` without modifying the row when the document is missing,
    /// the lifecycle token differs, or a newer content revision won the race.
    async fn mark_document_indexed(
        &self,
        _id: DocumentId,
        _revision: i64,
        _revision_token: uuid::Uuid,
        _fingerprint: &str,
        _indexed_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Clear the index watermark for an exact content revision and lifecycle.
    ///
    /// Returns `false` without modifying the row when the document or exact
    /// revision identity is no longer current.
    async fn clear_document_index(
        &self,
        _id: DocumentId,
        _revision: i64,
        _revision_token: uuid::Uuid,
    ) -> Result<bool> {
        document_storage_unavailable()
    }

    /// Persist a new chat.
    ///
    /// A chat's `project_id`, when set, is not checked against an existing
    /// project here — there is no database foreign key on the link (SQLite can't
    /// add one to an existing table). Callers that accept a `project_id` from
    /// outside must verify the project exists first (the server does so at its
    /// API edge) to avoid persisting a dangling reference.
    async fn create_chat(&self, chat: &Chat) -> Result<()>;

    /// Fetch a chat by id, or `None` if it doesn't exist.
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>>;

    /// List chats, most-recently-created first.
    async fn list_chats(&self) -> Result<Vec<Chat>>;

    /// Set (or clear, with `None`) a chat's model override. A no-op if the chat
    /// doesn't exist.
    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()>;

    /// Append a message to its chat.
    async fn append_message(&self, message: &Message) -> Result<()>;

    /// List a chat's messages in creation order.
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>>;

    /// Upsert a tool call (insert on first sight, update result on completion).
    async fn upsert_tool_call(&self, call: &ToolCallRecord) -> Result<()>;

    /// List a chat's tool calls in creation order.
    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>>;

    /// Read a setting (profile, model prefs, approval policy), or `None`.
    async fn get_setting(&self, key: &str) -> Result<Option<Value>>;

    /// Write a setting.
    async fn set_setting(&self, key: &str, value: &Value) -> Result<()>;

    /// Append an event to a chat's journal, returning its assigned sequence
    /// number. Sequence numbers are per-chat and monotonic (starting at 1),
    /// so a client can replay the stream with [`list_events`](Self::list_events).
    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64>;

    /// List a chat's journaled events with `seq` greater than `after`, in
    /// sequence order. Pass `0` to replay from the start.
    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>>;
}

/// Credential custody: secrets keyed by a stable reference string (e.g.
/// `provider.anthropic.credential`). Backed by the OS keychain on desktop, a
/// KMS/Vault on a server — never the [`Store`].
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Fetch a secret by key, or `None` if unset.
    async fn get_secret(&self, key: &str) -> Result<Option<String>>;

    /// Store (or overwrite) a secret.
    async fn set_secret(&self, key: &str, value: &str) -> Result<()>;

    /// Remove a secret; a no-op if it doesn't exist.
    async fn delete_secret(&self, key: &str) -> Result<()>;
}

/// Opaque byte storage for documents, images, and exports.
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Publish immutable bytes under `id`.
    ///
    /// Repeating the same publication is a no-op; publishing different bytes
    /// under an existing id fails without changing the stored value. Callers
    /// allocate a new id when content changes.
    async fn put(&self, id: &str, bytes: Vec<u8>) -> Result<()>;

    /// Fetch bytes by `id`, or `None` if absent.
    async fn get(&self, id: &str) -> Result<Option<Vec<u8>>>;

    /// Delete a blob; a no-op if it doesn't exist.
    async fn delete(&self, id: &str) -> Result<()>;
}

#[cfg(test)]
mod tests;
