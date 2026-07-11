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
    Chat, DocumentGeneration, DocumentJob, DocumentJobStatus, DocumentListCursor, DocumentRecord,
    DocumentScope, DocumentSummaryRecord, DocumentUpsert, Message, Project, ToolCallRecord,
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

    /// Fetch one durable document job by id.
    async fn get_document_job(&self, _id: DocumentJobId) -> Result<Option<DocumentJob>> {
        document_storage_unavailable()
    }

    /// List a document's semantic job history, oldest first.
    async fn list_document_jobs(&self, _document_id: DocumentId) -> Result<Vec<DocumentJob>> {
        document_storage_unavailable()
    }

    /// Explicitly retry the current exact-generation failed index job.
    ///
    /// A matching failed job is reset to a fresh queued delivery using a
    /// store-owned timestamp and `max_attempts`. Repeating the request while that
    /// exact job is already nonterminal returns it unchanged. Succeeded,
    /// cancelled, superseded, missing, or differently fingerprinted jobs are not
    /// revived and return `None`.
    async fn retry_document_index_job(
        &self,
        _document_id: DocumentId,
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
    /// Store bytes under `id`, overwriting any existing blob.
    async fn put(&self, id: &str, bytes: Vec<u8>) -> Result<()>;

    /// Fetch bytes by `id`, or `None` if absent.
    async fn get(&self, id: &str) -> Result<Option<Vec<u8>>>;

    /// Delete a blob; a no-op if it doesn't exist.
    async fn delete(&self, id: &str) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use std::sync::Mutex;

    use futures::executor::block_on;

    use super::*;
    use crate::model::{DocumentJobKind, DocumentJobStatus, DocumentProcessingStatus};

    /// Minimal in-memory `Store` — proves the trait is object-safe and usable
    /// behind `Arc<dyn Store>`, and exercises the signatures.
    #[derive(Default)]
    struct MemDocumentState {
        documents: HashMap<DocumentId, DocumentRecord>,
        generations: HashMap<DocumentId, DocumentGeneration>,
        tombstones: HashSet<DocumentId>,
        pending_retirements: HashMap<DocumentId, DocumentGeneration>,
        jobs: HashMap<DocumentJobId, DocumentJob>,
    }

    #[derive(Default)]
    struct MemStore {
        projects: Mutex<HashMap<ProjectId, Project>>,
        document_state: Mutex<MemDocumentState>,
        chats: Mutex<HashMap<ChatId, Chat>>,
        settings: Mutex<HashMap<String, Value>>,
        events: Mutex<Vec<(ChatId, SequencedEvent)>>,
        tool_calls: Mutex<HashMap<crate::id::CallId, ToolCallRecord>>,
    }

    fn allocate_mem_generation(
        state: &mut MemDocumentState,
        id: DocumentId,
    ) -> Result<DocumentGeneration> {
        let content_revision = match state.generations.get(&id) {
            Some(current) => current
                .content_revision
                .checked_add(1)
                .ok_or_else(|| AgentError::Store(format!("document {id} revision overflow")))?,
            None => 1,
        };
        let generation = DocumentGeneration {
            content_revision,
            revision_token: uuid::Uuid::new_v4(),
        };
        state.generations.insert(id, generation);
        state.tombstones.remove(&id);
        Ok(generation)
    }

    fn reset_mem_document_job(
        job: &mut DocumentJob,
        max_attempts: i32,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        job.status = DocumentJobStatus::Queued;
        job.attempt_count = 0;
        job.max_attempts = max_attempts;
        job.available_at = now;
        job.lease_token = None;
        job.lease_expires_at = None;
        job.started_at = None;
        job.finished_at = None;
        job.last_error_code = None;
        job.last_error_detail = None;
        job.updated_at = now;
    }

    #[async_trait]
    impl Store for MemStore {
        async fn create_project(&self, project: &Project) -> Result<()> {
            self.projects
                .lock()
                .unwrap()
                .insert(project.id, project.clone());
            Ok(())
        }
        async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
            Ok(self.projects.lock().unwrap().get(&id).cloned())
        }
        async fn list_projects(&self) -> Result<Vec<Project>> {
            Ok(self.projects.lock().unwrap().values().cloned().collect())
        }
        async fn create_document(&self, document: &DocumentRecord) -> Result<()> {
            if document
                .project_id
                .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
            {
                return Err(AgentError::Store(
                    "document references an unknown project".into(),
                ));
            }
            let mut state = self.document_state.lock().unwrap();
            if state.generations.contains_key(&document.id) {
                return Err(AgentError::Store("document already exists".into()));
            }
            let mut document = document.clone();
            document.revision_token = uuid::Uuid::new_v4();
            state.generations.insert(document.id, document.generation());
            state.tombstones.remove(&document.id);
            state.documents.insert(document.id, document);
            Ok(())
        }
        async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
            Ok(self
                .document_state
                .lock()
                .unwrap()
                .documents
                .get(&id)
                .cloned())
        }
        async fn list_documents(&self, scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
            Ok(self
                .document_state
                .lock()
                .unwrap()
                .documents
                .values()
                .filter(|document| match scope {
                    DocumentScope::All => true,
                    DocumentScope::Unscoped => document.project_id.is_none(),
                    DocumentScope::Project(id) => document.project_id == Some(id),
                })
                .cloned()
                .collect())
        }
        async fn list_document_summaries(
            &self,
            scope: DocumentScope,
            after: Option<DocumentListCursor>,
            limit: u64,
        ) -> Result<Vec<DocumentSummaryRecord>> {
            let mut documents: Vec<_> = self
                .document_state
                .lock()
                .unwrap()
                .documents
                .values()
                .filter(|document| match scope {
                    DocumentScope::All => true,
                    DocumentScope::Unscoped => document.project_id.is_none(),
                    DocumentScope::Project(id) => document.project_id == Some(id),
                })
                .filter(|document| {
                    after.is_none_or(|cursor| {
                        document.created_at < cursor.created_at
                            || (document.created_at == cursor.created_at
                                && document.id.0 < cursor.id.0)
                    })
                })
                .map(document_summary)
                .collect();
            documents.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| right.id.0.cmp(&left.id.0))
            });
            documents.truncate(limit.try_into().unwrap_or(usize::MAX));
            Ok(documents)
        }
        async fn get_document_generation(
            &self,
            id: DocumentId,
        ) -> Result<Option<DocumentGeneration>> {
            Ok(self
                .document_state
                .lock()
                .unwrap()
                .generations
                .get(&id)
                .copied())
        }
        async fn delete_document(&self, id: DocumentId) -> Result<DocumentGeneration> {
            let mut state = self.document_state.lock().unwrap();
            let generation = if state.documents.contains_key(&id) || !state.tombstones.contains(&id)
            {
                let generation = allocate_mem_generation(&mut state, id)?;
                state.documents.remove(&id);
                state.tombstones.insert(id);
                state.pending_retirements.insert(id, generation);
                generation
            } else if let Some(generation) = state.generations.get(&id) {
                *generation
            } else {
                unreachable!("a tombstone always retains its generation")
            };
            state.jobs.retain(|_, job| job.document_id != id);
            Ok(generation)
        }

        async fn list_pending_document_retirements(
            &self,
            after: Option<DocumentId>,
            limit: u64,
        ) -> Result<Vec<(DocumentId, DocumentGeneration)>> {
            let state = self.document_state.lock().unwrap();
            let mut retirements: Vec<_> = state
                .pending_retirements
                .iter()
                .filter(|(id, _)| after.is_none_or(|after| id.0 > after.0))
                .map(|(id, generation)| (*id, *generation))
                .collect();
            retirements.sort_unstable_by_key(|(id, _)| id.0);
            retirements.truncate(limit.try_into().unwrap_or(usize::MAX));
            Ok(retirements)
        }

        async fn get_pending_document_retirement(
            &self,
            id: DocumentId,
        ) -> Result<Option<DocumentGeneration>> {
            Ok(self
                .document_state
                .lock()
                .unwrap()
                .pending_retirements
                .get(&id)
                .copied())
        }

        async fn complete_document_retirement(
            &self,
            id: DocumentId,
            generation: DocumentGeneration,
        ) -> Result<bool> {
            let mut state = self.document_state.lock().unwrap();
            if state.pending_retirements.get(&id) != Some(&generation) {
                return Ok(false);
            }
            state.pending_retirements.remove(&id);
            Ok(true)
        }
        async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
            crate::model::validate_source_regions(
                &document.canonical_text,
                &document.source_regions,
            )
            .map_err(|message| AgentError::Store(message.into()))?;
            if document.media_type.is_empty()
                || document.source_uri.as_deref() == Some("")
                || document
                    .project_id
                    .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
            {
                return Err(AgentError::Store("invalid document upsert".into()));
            }
            let mut state = self.document_state.lock().unwrap();
            if state
                .documents
                .get(&document.id)
                .is_some_and(|existing| existing.project_id != document.project_id)
            {
                return Err(AgentError::Store(format!(
                    "document {} cannot move between project corpora",
                    document.id
                )));
            }
            let created_at = state
                .documents
                .get(&document.id)
                .map_or(document.updated_at, |existing| existing.created_at);
            let generation = allocate_mem_generation(&mut state, document.id)?;
            let record = DocumentRecord {
                id: document.id,
                project_id: document.project_id,
                source_uri: document.source_uri.clone(),
                media_type: document.media_type.clone(),
                title: document.title.clone(),
                source_blob: None,
                canonical_text: document.canonical_text.clone(),
                canonical_fingerprint: None,
                source_regions: document.source_regions.clone(),
                content_revision: generation.content_revision,
                revision_token: generation.revision_token,
                processing_status: DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at,
                updated_at: document.updated_at,
                indexed_at: None,
            };
            state.documents.insert(record.id, record.clone());
            Ok(record)
        }
        async fn upsert_document_and_enqueue_index(
            &self,
            document: &DocumentUpsert,
            pipeline_fingerprint: &str,
            max_attempts: i32,
        ) -> Result<(DocumentRecord, DocumentJob)> {
            crate::model::validate_source_regions(
                &document.canonical_text,
                &document.source_regions,
            )
            .map_err(|message| AgentError::Store(message.into()))?;
            if pipeline_fingerprint.is_empty()
                || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                || max_attempts < 1
                || document.media_type.is_empty()
                || document.source_uri.as_deref() == Some("")
                || document
                    .project_id
                    .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
            {
                return Err(AgentError::Store("invalid document job enqueue".into()));
            }

            let mut state = self.document_state.lock().unwrap();
            if state
                .documents
                .get(&document.id)
                .is_some_and(|existing| existing.project_id != document.project_id)
            {
                return Err(AgentError::Store(format!(
                    "document {} cannot move between project corpora",
                    document.id
                )));
            }
            if let Some(existing) = state.documents.get(&document.id).filter(|existing| {
                existing.project_id == document.project_id
                    && existing.source_uri == document.source_uri
                    && existing.media_type == document.media_type
                    && existing.title == document.title
                    && existing.canonical_text == document.canonical_text
                    && existing.source_regions == document.source_regions
            }) {
                if let Some(job) = state.jobs.values().find(|job| {
                    job.document_id == existing.id
                        && job.content_revision == existing.content_revision
                        && job.revision_token == existing.revision_token
                        && job.kind == DocumentJobKind::Index
                        && job.pipeline_fingerprint == pipeline_fingerprint
                }) {
                    return Ok((existing.clone(), job.clone()));
                }
            }

            let workflow_now = chrono::Utc::now();
            let created_at = state
                .documents
                .get(&document.id)
                .map_or(document.updated_at, |existing| existing.created_at);
            let generation = allocate_mem_generation(&mut state, document.id)?;
            let record = DocumentRecord {
                id: document.id,
                project_id: document.project_id,
                source_uri: document.source_uri.clone(),
                media_type: document.media_type.clone(),
                title: document.title.clone(),
                source_blob: None,
                canonical_text: document.canonical_text.clone(),
                canonical_fingerprint: None,
                source_regions: document.source_regions.clone(),
                content_revision: generation.content_revision,
                revision_token: generation.revision_token,
                processing_status: DocumentProcessingStatus::Queued,
                indexed_revision: None,
                index_fingerprint: None,
                created_at,
                updated_at: document.updated_at,
                indexed_at: None,
            };
            state.documents.insert(record.id, record.clone());

            for job in state.jobs.values_mut().filter(|job| {
                job.document_id == record.id
                    && matches!(
                        job.status,
                        DocumentJobStatus::Queued
                            | DocumentJobStatus::Running
                            | DocumentJobStatus::RetryWait
                    )
            }) {
                job.status = DocumentJobStatus::Cancelled;
                job.lease_token = None;
                job.lease_expires_at = None;
                job.finished_at = Some(workflow_now);
                job.updated_at = workflow_now;
            }
            let job = DocumentJob {
                id: DocumentJobId::new(),
                document_id: record.id,
                content_revision: record.content_revision,
                revision_token: record.revision_token,
                kind: DocumentJobKind::Index,
                status: DocumentJobStatus::Queued,
                pipeline_fingerprint: pipeline_fingerprint.into(),
                attempt_count: 0,
                max_attempts,
                available_at: workflow_now,
                lease_token: None,
                lease_expires_at: None,
                started_at: None,
                finished_at: None,
                last_error_code: None,
                last_error_detail: None,
                created_at: workflow_now,
                updated_at: workflow_now,
            };
            state.jobs.insert(job.id, job.clone());
            Ok((record, job))
        }
        async fn ensure_document_index_job(
            &self,
            document_id: DocumentId,
            expected_generation: DocumentGeneration,
            pipeline_fingerprint: &str,
            max_attempts: i32,
            reason: DocumentIndexJobReason,
        ) -> Result<EnsureDocumentIndexJobOutcome> {
            if pipeline_fingerprint.is_empty()
                || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                || max_attempts < 1
            {
                return Err(AgentError::Store(
                    "invalid document index-job maintenance request".into(),
                ));
            }

            let mut state = self.document_state.lock().unwrap();
            let Some(mut document) = state.documents.get(&document_id).cloned() else {
                return Ok(EnsureDocumentIndexJobOutcome::MissingDocument);
            };

            if document.generation() != expected_generation {
                if reason.advances_generation()
                    && expected_generation
                        .content_revision
                        .checked_add(1)
                        .is_some_and(|revision| revision == document.content_revision)
                {
                    if let Some(job) = state.jobs.values().find(|job| {
                        job.document_id == document_id
                            && job.generation() == document.generation()
                            && job.kind == DocumentJobKind::Index
                            && job.pipeline_fingerprint == pipeline_fingerprint
                    }) {
                        return Ok(if job.status == DocumentJobStatus::Failed {
                            EnsureDocumentIndexJobOutcome::Failed(job.clone())
                        } else {
                            EnsureDocumentIndexJobOutcome::Existing(job.clone())
                        });
                    }
                }
                return Ok(EnsureDocumentIndexJobOutcome::GenerationChanged(
                    document.generation(),
                ));
            }

            let desired_job_id = state.jobs.values().find_map(|job| {
                (job.document_id == document_id
                    && job.generation() == document.generation()
                    && job.kind == DocumentJobKind::Index
                    && job.pipeline_fingerprint == pipeline_fingerprint)
                    .then_some(job.id)
            });
            if let Some(job_id) = desired_job_id {
                let job = state.jobs.get(&job_id).unwrap().clone();
                if matches!(
                    job.status,
                    DocumentJobStatus::Queued
                        | DocumentJobStatus::Running
                        | DocumentJobStatus::RetryWait
                ) || (reason == DocumentIndexJobReason::PipelineChanged
                    && job.status == DocumentJobStatus::Succeeded)
                {
                    return Ok(EnsureDocumentIndexJobOutcome::Existing(job));
                }
                if job.status == DocumentJobStatus::Failed {
                    return Ok(EnsureDocumentIndexJobOutcome::Failed(job));
                }
                if reason == DocumentIndexJobReason::DerivedStateMissing {
                    let now = chrono::Utc::now();
                    let job = state.jobs.get_mut(&job_id).unwrap();
                    reset_mem_document_job(job, max_attempts, now);
                    let job = job.clone();
                    document.processing_status = DocumentProcessingStatus::Queued;
                    document.indexed_revision = None;
                    document.index_fingerprint = None;
                    document.indexed_at = None;
                    state.documents.insert(document_id, document);
                    return Ok(EnsureDocumentIndexJobOutcome::Enqueued(job));
                }
            }

            if reason.advances_generation() {
                let generation = allocate_mem_generation(&mut state, document_id)?;
                document.content_revision = generation.content_revision;
                document.revision_token = generation.revision_token;
            }
            document.processing_status = DocumentProcessingStatus::Queued;
            document.indexed_revision = None;
            document.index_fingerprint = None;
            document.indexed_at = None;
            state.documents.insert(document_id, document.clone());

            let now = chrono::Utc::now();
            for job in state.jobs.values_mut().filter(|job| {
                job.document_id == document_id
                    && matches!(
                        job.status,
                        DocumentJobStatus::Queued
                            | DocumentJobStatus::Running
                            | DocumentJobStatus::RetryWait
                    )
            }) {
                job.status = DocumentJobStatus::Cancelled;
                job.lease_token = None;
                job.lease_expires_at = None;
                job.finished_at = Some(now);
                job.updated_at = now;
            }
            let job = DocumentJob {
                id: DocumentJobId::new(),
                document_id,
                content_revision: document.content_revision,
                revision_token: document.revision_token,
                kind: DocumentJobKind::Index,
                status: DocumentJobStatus::Queued,
                pipeline_fingerprint: pipeline_fingerprint.into(),
                attempt_count: 0,
                max_attempts,
                available_at: now,
                lease_token: None,
                lease_expires_at: None,
                started_at: None,
                finished_at: None,
                last_error_code: None,
                last_error_detail: None,
                created_at: now,
                updated_at: now,
            };
            state.jobs.insert(job.id, job.clone());
            Ok(EnsureDocumentIndexJobOutcome::Enqueued(job))
        }
        async fn get_document_job(&self, id: DocumentJobId) -> Result<Option<DocumentJob>> {
            Ok(self.document_state.lock().unwrap().jobs.get(&id).cloned())
        }
        async fn list_document_jobs(&self, document_id: DocumentId) -> Result<Vec<DocumentJob>> {
            let mut jobs: Vec<_> = self
                .document_state
                .lock()
                .unwrap()
                .jobs
                .values()
                .filter(|job| job.document_id == document_id)
                .cloned()
                .collect();
            jobs.sort_by(|left, right| {
                left.content_revision
                    .cmp(&right.content_revision)
                    .then_with(|| left.created_at.cmp(&right.created_at))
                    .then_with(|| left.id.0.cmp(&right.id.0))
            });
            Ok(jobs)
        }
        async fn retry_document_index_job(
            &self,
            document_id: DocumentId,
            pipeline_fingerprint: &str,
            max_attempts: i32,
        ) -> Result<Option<DocumentJob>> {
            if pipeline_fingerprint.is_empty()
                || pipeline_fingerprint.chars().count() > DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                || max_attempts < 1
            {
                return Err(AgentError::Store("invalid document job retry".into()));
            }
            let mut state = self.document_state.lock().unwrap();
            let Some(document) = state.documents.get(&document_id).cloned() else {
                return Ok(None);
            };
            let candidate_id = state
                .jobs
                .values()
                .find(|job| {
                    job.document_id == document_id
                        && job.content_revision == document.content_revision
                        && job.revision_token == document.revision_token
                        && job.kind == DocumentJobKind::Index
                        && job.pipeline_fingerprint == pipeline_fingerprint
                })
                .map(|job| job.id);
            let Some(candidate_id) = candidate_id else {
                return Ok(None);
            };
            let candidate = state.jobs.get(&candidate_id).unwrap().clone();
            if matches!(
                candidate.status,
                DocumentJobStatus::Queued
                    | DocumentJobStatus::Running
                    | DocumentJobStatus::RetryWait
            ) {
                let expected = if candidate.status == DocumentJobStatus::Running {
                    DocumentProcessingStatus::Processing
                } else {
                    DocumentProcessingStatus::Queued
                };
                if document.processing_status != expected {
                    return Err(AgentError::Store(format!(
                        "document job {} is {} but exact document {} is unexpectedly {}",
                        candidate.id,
                        candidate.status.as_str(),
                        document_id,
                        document.processing_status.as_str()
                    )));
                }
                return Ok(Some(candidate));
            }
            if candidate.status != DocumentJobStatus::Failed {
                return Ok(None);
            }
            if document.processing_status != DocumentProcessingStatus::Failed {
                return Err(AgentError::Store(format!(
                    "failed document job {} does not match failed document {}",
                    candidate.id, document_id
                )));
            }

            let now = chrono::Utc::now();
            let job = state.jobs.get_mut(&candidate_id).unwrap();
            job.status = DocumentJobStatus::Queued;
            job.attempt_count = 0;
            job.max_attempts = max_attempts;
            job.available_at = now;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.started_at = None;
            job.finished_at = None;
            job.last_error_code = None;
            job.last_error_detail = None;
            job.updated_at = now;
            let job = job.clone();
            let document = state.documents.get_mut(&document_id).unwrap();
            document.processing_status = DocumentProcessingStatus::Queued;
            document.indexed_revision = None;
            document.index_fingerprint = None;
            document.indexed_at = None;
            Ok(Some(job))
        }
        async fn claim_document_job(
            &self,
            now: chrono::DateTime<chrono::Utc>,
            lease_expires_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<Option<DocumentJob>> {
            if lease_expires_at <= now {
                return Err(AgentError::Store(
                    "document job lease expiry must be after claim time".into(),
                ));
            }
            let mut state = self.document_state.lock().unwrap();
            loop {
                let candidate_id = state
                    .jobs
                    .values()
                    .filter(|job| {
                        (matches!(
                            job.status,
                            DocumentJobStatus::Queued | DocumentJobStatus::RetryWait
                        ) && job.available_at <= now
                            && job.attempt_count < job.max_attempts)
                            || (job.status == DocumentJobStatus::Running
                                && job.lease_expires_at.is_some_and(|expiry| expiry <= now))
                    })
                    .min_by(|left, right| {
                        let left_due = left.lease_expires_at.unwrap_or(left.available_at);
                        let right_due = right.lease_expires_at.unwrap_or(right.available_at);
                        left_due
                            .cmp(&right_due)
                            .then_with(|| left.created_at.cmp(&right.created_at))
                            .then_with(|| left.id.0.cmp(&right.id.0))
                    })
                    .map(|job| job.id);
                let Some(candidate_id) = candidate_id else {
                    return Ok(None);
                };
                let candidate = state.jobs.get(&candidate_id).unwrap().clone();
                let expected_document_status = if candidate.status == DocumentJobStatus::Running {
                    DocumentProcessingStatus::Processing
                } else {
                    DocumentProcessingStatus::Queued
                };
                let identity_matches =
                    state
                        .documents
                        .get(&candidate.document_id)
                        .is_some_and(|document| {
                            document.content_revision == candidate.content_revision
                                && document.revision_token == candidate.revision_token
                        });
                if !identity_matches {
                    let job = state.jobs.get_mut(&candidate_id).unwrap();
                    job.status = DocumentJobStatus::Cancelled;
                    job.lease_token = None;
                    job.lease_expires_at = None;
                    job.finished_at = Some(now);
                    job.updated_at = now;
                    continue;
                }
                let current_status = state
                    .documents
                    .get(&candidate.document_id)
                    .unwrap()
                    .processing_status;
                if current_status != expected_document_status {
                    return Err(AgentError::Store(format!(
                        "document job {} is {} but exact document {} is unexpectedly {}",
                        candidate.id,
                        candidate.status.as_str(),
                        candidate.document_id,
                        current_status.as_str()
                    )));
                }

                if candidate.status == DocumentJobStatus::Running
                    && candidate.attempt_count >= candidate.max_attempts
                {
                    let job = state.jobs.get_mut(&candidate_id).unwrap();
                    job.status = DocumentJobStatus::Failed;
                    job.lease_token = None;
                    job.lease_expires_at = None;
                    job.finished_at = Some(now);
                    job.last_error_code = Some("lease_expired".into());
                    job.last_error_detail = Some("final worker lease expired".into());
                    job.updated_at = now;
                    state
                        .documents
                        .get_mut(&candidate.document_id)
                        .unwrap()
                        .processing_status = DocumentProcessingStatus::Failed;
                    continue;
                }

                let job = state.jobs.get_mut(&candidate_id).unwrap();
                job.status = DocumentJobStatus::Running;
                job.attempt_count = job.attempt_count.checked_add(1).ok_or_else(|| {
                    AgentError::Store(format!("document job {} attempt overflow", job.id))
                })?;
                job.lease_token = Some(uuid::Uuid::new_v4());
                job.lease_expires_at = Some(lease_expires_at);
                job.started_at.get_or_insert(now);
                if candidate.status == DocumentJobStatus::Running {
                    job.last_error_code = Some("lease_expired".into());
                    job.last_error_detail = Some("previous worker lease expired".into());
                }
                job.updated_at = now;
                let job = job.clone();
                state
                    .documents
                    .get_mut(&candidate.document_id)
                    .unwrap()
                    .processing_status = DocumentProcessingStatus::Processing;
                return Ok(Some(job));
            }
        }
        async fn heartbeat_document_job(
            &self,
            id: DocumentJobId,
            lease_token: uuid::Uuid,
            now: chrono::DateTime<chrono::Utc>,
            lease_expires_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            if lease_expires_at <= now {
                return Err(AgentError::Store(
                    "document job lease expiry must be after heartbeat time".into(),
                ));
            }
            let mut state = self.document_state.lock().unwrap();
            let Some(job) = state.jobs.get_mut(&id) else {
                return Ok(false);
            };
            if job.status != DocumentJobStatus::Running
                || job.lease_token != Some(lease_token)
                || job.lease_expires_at.is_none_or(|expiry| expiry <= now)
                || job.updated_at > now
                || job
                    .lease_expires_at
                    .is_some_and(|expiry| expiry >= lease_expires_at)
            {
                return Ok(false);
            }
            job.lease_expires_at = Some(lease_expires_at);
            job.updated_at = now;
            Ok(true)
        }
        async fn complete_document_index_job(
            &self,
            id: DocumentJobId,
            lease_token: uuid::Uuid,
            completed_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            let mut state = self.document_state.lock().unwrap();
            let Some(candidate) = state.jobs.get(&id).cloned() else {
                return Ok(false);
            };
            if candidate.status != DocumentJobStatus::Running
                || candidate.lease_token != Some(lease_token)
                || candidate
                    .lease_expires_at
                    .is_none_or(|expiry| expiry <= completed_at)
                || candidate.updated_at > completed_at
            {
                return Ok(false);
            }
            let document_matches =
                state
                    .documents
                    .get(&candidate.document_id)
                    .is_some_and(|document| {
                        document.content_revision == candidate.content_revision
                            && document.revision_token == candidate.revision_token
                            && document.processing_status == DocumentProcessingStatus::Processing
                    });
            if !document_matches {
                return Err(AgentError::Store(format!(
                    "running document job {} does not match its exact processing document {}",
                    candidate.id, candidate.document_id
                )));
            }

            let job = state.jobs.get_mut(&id).unwrap();
            job.status = DocumentJobStatus::Succeeded;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.finished_at = Some(completed_at);
            job.last_error_code = None;
            job.last_error_detail = None;
            job.updated_at = completed_at;
            let document = state.documents.get_mut(&candidate.document_id).unwrap();
            document.processing_status = DocumentProcessingStatus::Ready;
            document.indexed_revision = Some(candidate.content_revision);
            document.index_fingerprint = Some(candidate.pipeline_fingerprint);
            document.indexed_at = Some(completed_at);
            Ok(true)
        }
        async fn record_document_job_failure(
            &self,
            id: DocumentJobId,
            lease_token: uuid::Uuid,
            failed_at: chrono::DateTime<chrono::Utc>,
            retry_at: Option<chrono::DateTime<chrono::Utc>>,
            error_code: &str,
            error_detail: Option<&str>,
        ) -> Result<Option<DocumentJobStatus>> {
            let code_len = error_code.chars().count();
            if !(1..=DocumentJob::MAX_ERROR_CODE_LEN).contains(&code_len)
                || error_detail.is_some_and(|detail| {
                    !(1..=DocumentJob::MAX_ERROR_DETAIL_LEN).contains(&detail.chars().count())
                })
                || retry_at.is_some_and(|retry_at| retry_at <= failed_at)
            {
                return Err(AgentError::Store("invalid document job failure".into()));
            }
            let mut state = self.document_state.lock().unwrap();
            let Some(candidate) = state.jobs.get(&id).cloned() else {
                return Ok(None);
            };
            if candidate.status != DocumentJobStatus::Running
                || candidate.lease_token != Some(lease_token)
                || candidate
                    .lease_expires_at
                    .is_none_or(|expiry| expiry <= failed_at)
                || candidate.updated_at > failed_at
            {
                return Ok(None);
            }
            let document_matches =
                state
                    .documents
                    .get(&candidate.document_id)
                    .is_some_and(|document| {
                        document.content_revision == candidate.content_revision
                            && document.revision_token == candidate.revision_token
                            && document.processing_status == DocumentProcessingStatus::Processing
                    });
            if !document_matches {
                return Err(AgentError::Store(format!(
                    "running document job {} does not match its exact processing document {}",
                    candidate.id, candidate.document_id
                )));
            }

            let will_retry = retry_at.is_some() && candidate.attempt_count < candidate.max_attempts;
            let status = if will_retry {
                DocumentJobStatus::RetryWait
            } else {
                DocumentJobStatus::Failed
            };
            let job = state.jobs.get_mut(&id).unwrap();
            job.status = status;
            job.lease_token = None;
            job.lease_expires_at = None;
            job.last_error_code = Some(error_code.to_owned());
            job.last_error_detail = error_detail.map(str::to_owned);
            job.updated_at = failed_at;
            if let Some(retry_at) = retry_at.filter(|_| will_retry) {
                job.available_at = retry_at;
            } else {
                job.finished_at = Some(failed_at);
            }
            state
                .documents
                .get_mut(&candidate.document_id)
                .unwrap()
                .processing_status = if will_retry {
                DocumentProcessingStatus::Queued
            } else {
                DocumentProcessingStatus::Failed
            };
            Ok(Some(status))
        }
        async fn mark_document_indexed(
            &self,
            id: DocumentId,
            revision: i64,
            revision_token: uuid::Uuid,
            fingerprint: &str,
            indexed_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            if fingerprint.is_empty()
                || fingerprint.chars().count()
                    > crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
            {
                return Err(AgentError::Store(
                    "document index fingerprint must contain 1 to 512 characters".into(),
                ));
            }
            let mut state = self.document_state.lock().unwrap();
            let Some(document) = state.documents.get_mut(&id) else {
                return Ok(false);
            };
            if document.content_revision != revision || document.revision_token != revision_token {
                return Ok(false);
            }
            document.indexed_revision = Some(revision);
            document.index_fingerprint = Some(fingerprint.to_string());
            document.indexed_at = Some(indexed_at);
            document.processing_status = DocumentProcessingStatus::Ready;
            Ok(true)
        }
        async fn clear_document_index(
            &self,
            id: DocumentId,
            revision: i64,
            revision_token: uuid::Uuid,
        ) -> Result<bool> {
            let mut state = self.document_state.lock().unwrap();
            let Some(document) = state.documents.get_mut(&id) else {
                return Ok(false);
            };
            if document.content_revision != revision || document.revision_token != revision_token {
                return Ok(false);
            }
            document.indexed_revision = None;
            document.index_fingerprint = None;
            document.indexed_at = None;
            document.processing_status = DocumentProcessingStatus::Queued;
            Ok(true)
        }
        async fn create_chat(&self, chat: &Chat) -> Result<()> {
            self.chats.lock().unwrap().insert(chat.id, chat.clone());
            Ok(())
        }
        async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
            Ok(self.chats.lock().unwrap().get(&id).cloned())
        }
        async fn list_chats(&self) -> Result<Vec<Chat>> {
            Ok(self.chats.lock().unwrap().values().cloned().collect())
        }
        async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
            if let Some(chat) = self.chats.lock().unwrap().get_mut(&id) {
                chat.model = model;
            }
            Ok(())
        }
        async fn append_message(&self, _message: &Message) -> Result<()> {
            Ok(())
        }
        async fn list_messages(&self, _chat_id: ChatId) -> Result<Vec<Message>> {
            Ok(vec![])
        }
        async fn upsert_tool_call(&self, call: &ToolCallRecord) -> Result<()> {
            let mut calls = self.tool_calls.lock().unwrap();
            if let Some(existing) = calls.get_mut(&call.id) {
                existing.arguments = call.arguments.clone();
                existing.result = call.result.clone();
                existing.is_error = call.is_error;
                existing.completed_at = call.completed_at;
            } else {
                calls.insert(call.id, call.clone());
            }
            Ok(())
        }
        async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
            let mut calls: Vec<_> = self
                .tool_calls
                .lock()
                .unwrap()
                .values()
                .filter(|call| call.chat_id == chat_id)
                .cloned()
                .collect();
            calls.sort_by_key(|call| call.created_at);
            Ok(calls)
        }
        async fn get_setting(&self, key: &str) -> Result<Option<Value>> {
            Ok(self.settings.lock().unwrap().get(key).cloned())
        }
        async fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
            self.settings
                .lock()
                .unwrap()
                .insert(key.to_string(), value.clone());
            Ok(())
        }
        async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
            let mut events = self.events.lock().unwrap();
            let seq = events.iter().filter(|(id, _)| *id == chat_id).count() as i64 + 1;
            events.push((
                chat_id,
                SequencedEvent {
                    seq,
                    event: event.clone(),
                },
            ));
            Ok(seq)
        }
        async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, e)| *id == chat_id && e.seq > after)
                .map(|(_, e)| e.clone())
                .collect())
        }
    }

    fn document_summary(document: &DocumentRecord) -> DocumentSummaryRecord {
        DocumentSummaryRecord {
            id: document.id,
            project_id: document.project_id,
            source_uri: document.source_uri.clone(),
            media_type: document.media_type.clone(),
            title: document.title.clone(),
            content_revision: document.content_revision,
            processing_status: document.processing_status,
            indexed_revision: document.indexed_revision,
            index_fingerprint: document.index_fingerprint.clone(),
            created_at: document.created_at,
            updated_at: document.updated_at,
            indexed_at: document.indexed_at,
        }
    }

    #[test]
    fn mem_store_create_document_rejects_an_unknown_project() {
        let store = MemStore::default();
        let now = chrono::Utc::now();
        let document = DocumentRecord {
            id: DocumentId::new(),
            project_id: Some(ProjectId::new()),
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            source_blob: None,
            canonical_text: "orphan".into(),
            canonical_fingerprint: None,
            source_regions: Vec::new(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at: now,
            updated_at: now,
            indexed_at: None,
        };

        assert!(block_on(store.create_document(&document)).is_err());
        assert_eq!(block_on(store.get_document(document.id)).unwrap(), None);
        assert_eq!(
            block_on(store.get_document_generation(document.id)).unwrap(),
            None
        );
    }

    #[test]
    fn store_is_object_safe_and_roundtrips() {
        let store: Arc<dyn Store> = Arc::new(MemStore::default());
        let chat = Chat {
            id: ChatId::new(),
            project_id: None,
            title: None,
            model: None,
            workspace_dir: "/tmp/ws".into(),
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
        };
        block_on(store.create_chat(&chat)).unwrap();
        let fetched = block_on(store.get_chat(chat.id)).unwrap();
        assert_eq!(fetched.as_ref(), Some(&chat));

        block_on(store.set_setting("model", &serde_json::json!("claude"))).unwrap();
        assert_eq!(
            block_on(store.get_setting("model")).unwrap(),
            Some(serde_json::json!("claude"))
        );

        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///mem-store.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "atomic source and job".into(),
            source_regions: Vec::new(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
        };
        let first =
            block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
        let retry = DocumentUpsert {
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(2, 0).unwrap(),
            ..source.clone()
        };
        assert_eq!(
            block_on(store.upsert_document_and_enqueue_index(&retry, "pipeline-v1", 3)).unwrap(),
            first
        );
        assert_eq!(
            block_on(store.list_document_jobs(source.id)).unwrap(),
            vec![first.1.clone()]
        );

        let claim_at = first.1.available_at + chrono::Duration::seconds(1);
        let lease_expires_at = claim_at + chrono::Duration::minutes(5);
        let claimed = block_on(store.claim_document_job(claim_at, lease_expires_at))
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, first.1.id);
        let extended = lease_expires_at + chrono::Duration::minutes(5);
        assert!(block_on(store.heartbeat_document_job(
            claimed.id,
            claimed.lease_token.unwrap(),
            claim_at + chrono::Duration::minutes(1),
            extended,
        ))
        .unwrap());
        assert!(block_on(store.complete_document_index_job(
            claimed.id,
            claimed.lease_token.unwrap(),
            claim_at + chrono::Duration::minutes(2),
        ))
        .unwrap());

        let tombstone = block_on(store.delete_document(source.id)).unwrap();
        assert_eq!(tombstone.content_revision, 2);
        assert_eq!(
            block_on(store.delete_document(source.id)).unwrap(),
            tombstone
        );
        assert_eq!(
            block_on(store.get_document_generation(source.id)).unwrap(),
            Some(tombstone)
        );
        assert_eq!(block_on(store.get_document(source.id)).unwrap(), None);
        assert_eq!(block_on(store.get_document_job(first.1.id)).unwrap(), None);

        let retry_source = DocumentUpsert {
            canonical_text: "retry state".into(),
            source_regions: Vec::new(),
            ..source
        };
        let (recreated, retry_job) =
            block_on(store.upsert_document_and_enqueue_index(&retry_source, "pipeline-v1", 2))
                .unwrap();
        assert_eq!(recreated.content_revision, 3);
        let retry_claim_at = retry_job.available_at + chrono::Duration::seconds(1);
        let retry_claim = block_on(store.claim_document_job(
            retry_claim_at,
            retry_claim_at + chrono::Duration::minutes(5),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            block_on(store.record_document_job_failure(
                retry_claim.id,
                retry_claim.lease_token.unwrap(),
                retry_claim_at + chrono::Duration::minutes(1),
                Some(retry_claim_at + chrono::Duration::minutes(2)),
                "timeout",
                None,
            ))
            .unwrap(),
            Some(DocumentJobStatus::RetryWait)
        );
    }

    #[test]
    fn mem_store_rejects_moving_a_live_document_between_corpora() {
        let store: Arc<dyn Store> = Arc::new(MemStore::default());
        let project_a = Project {
            id: ProjectId::new(),
            title: None,
            workspace_dir: "/tmp/a".into(),
            created_at: chrono::Utc::now(),
        };
        let project_b = Project {
            id: ProjectId::new(),
            workspace_dir: "/tmp/b".into(),
            ..project_a.clone()
        };
        block_on(store.create_project(&project_a)).unwrap();
        block_on(store.create_project(&project_b)).unwrap();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: Some(project_a.id),
            source_uri: Some("file:///scoped.txt".into()),
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "project A source".into(),
            source_regions: Vec::new(),
            updated_at: chrono::Utc::now(),
        };
        let first =
            block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
        let moved = DocumentUpsert {
            project_id: Some(project_b.id),
            canonical_text: "must not move".into(),
            source_regions: Vec::new(),
            ..source
        };
        assert!(
            block_on(store.upsert_document_and_enqueue_index(&moved, "pipeline-v1", 3)).is_err()
        );
        assert_eq!(
            block_on(store.get_document(moved.id)).unwrap(),
            Some(first.0)
        );
    }

    #[test]
    fn mem_index_maintenance_requeues_or_advances_by_reason() {
        let store = MemStore::default();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: Some("file:///maintenance.txt".into()),
            media_type: "text/plain".into(),
            title: Some("maintenance".into()),
            canonical_text: "stable source".into(),
            source_regions: Vec::new(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(100, 0).unwrap(),
        };
        let (first_document, first_job) =
            block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
        let claim_at = first_job.available_at + chrono::Duration::seconds(1);
        let claimed =
            block_on(store.claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1)))
                .unwrap()
                .unwrap();
        assert!(block_on(store.complete_document_index_job(
            claimed.id,
            claimed.lease_token.unwrap(),
            claim_at + chrono::Duration::seconds(1),
        ))
        .unwrap());

        let missing = block_on(store.ensure_document_index_job(
            source.id,
            first_document.generation(),
            "pipeline-v1",
            5,
            DocumentIndexJobReason::DerivedStateMissing,
        ))
        .unwrap();
        let EnsureDocumentIndexJobOutcome::Enqueued(requeued) = missing else {
            panic!("missing state should requeue the exact succeeded job")
        };
        assert_eq!(requeued.id, first_job.id);
        assert_eq!(requeued.generation(), first_document.generation());
        assert_eq!(requeued.max_attempts, 5);

        let changed = block_on(store.ensure_document_index_job(
            source.id,
            first_document.generation(),
            "pipeline-v2",
            4,
            DocumentIndexJobReason::PipelineChanged,
        ))
        .unwrap();
        let EnsureDocumentIndexJobOutcome::Enqueued(changed_job) = changed else {
            panic!("pipeline change should enqueue an advanced generation")
        };
        assert_eq!(
            changed_job.content_revision,
            first_document.content_revision + 1
        );
        let repeated = block_on(store.ensure_document_index_job(
            source.id,
            first_document.generation(),
            "pipeline-v2",
            4,
            DocumentIndexJobReason::PipelineChanged,
        ))
        .unwrap();
        assert_eq!(
            repeated,
            EnsureDocumentIndexJobOutcome::Existing(changed_job.clone())
        );
        let changed_claim_at = changed_job.available_at + chrono::Duration::seconds(1);
        let changed_claim = block_on(store.claim_document_job(
            changed_claim_at,
            changed_claim_at + chrono::Duration::minutes(1),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(changed_claim.id, changed_job.id);
        assert!(block_on(store.complete_document_index_job(
            changed_claim.id,
            changed_claim.lease_token.unwrap(),
            changed_claim_at + chrono::Duration::seconds(1),
        ))
        .unwrap());
        let incomplete = block_on(store.ensure_document_index_job(
            source.id,
            changed_job.generation(),
            "pipeline-v2",
            6,
            DocumentIndexJobReason::DerivedStateIncomplete,
        ))
        .unwrap();
        let EnsureDocumentIndexJobOutcome::Enqueued(incomplete_job) = incomplete else {
            panic!("incomplete succeeded state should advance its generation")
        };
        assert_eq!(
            incomplete_job.content_revision,
            changed_job.content_revision + 1
        );
        let current = block_on(store.get_document(source.id)).unwrap().unwrap();
        assert_eq!(current.generation(), incomplete_job.generation());
        assert_eq!(current.canonical_text, source.canonical_text);
        assert_eq!(current.created_at, first_document.created_at);
        assert_eq!(current.updated_at, first_document.updated_at);
    }

    #[test]
    fn mem_store_generation_overflow_leaves_source_job_and_clock_unchanged() {
        let store = MemStore::default();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "maximum generation".into(),
            source_regions: Vec::new(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
        };
        let (record, job) =
            block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 3)).unwrap();
        let maximum = DocumentGeneration {
            content_revision: i64::MAX,
            revision_token: record.revision_token,
        };
        {
            let mut state = store.document_state.lock().unwrap();
            state.generations.insert(source.id, maximum);
            state
                .documents
                .get_mut(&source.id)
                .unwrap()
                .content_revision = i64::MAX;
        }

        assert!(block_on(store.delete_document(source.id)).is_err());
        let retained = block_on(store.get_document(source.id)).unwrap().unwrap();
        assert_eq!(retained.generation(), maximum);
        assert_eq!(
            block_on(store.get_document_generation(source.id)).unwrap(),
            Some(maximum)
        );
        assert_eq!(
            block_on(store.get_document_job(job.id)).unwrap(),
            Some(job.clone())
        );

        let succeeded = {
            let mut state = store.document_state.lock().unwrap();
            let now = chrono::Utc::now();
            let job = state.jobs.get_mut(&job.id).unwrap();
            job.content_revision = i64::MAX;
            job.status = DocumentJobStatus::Succeeded;
            job.attempt_count = 1;
            job.finished_at = Some(now);
            job.updated_at = now;
            job.clone()
        };
        assert!(block_on(store.ensure_document_index_job(
            source.id,
            maximum,
            "pipeline-v1",
            3,
            DocumentIndexJobReason::DerivedStateIncomplete,
        ))
        .is_err());
        assert_eq!(
            block_on(store.get_document(source.id))
                .unwrap()
                .unwrap()
                .generation(),
            maximum
        );
        assert_eq!(
            block_on(store.get_document_generation(source.id)).unwrap(),
            Some(maximum)
        );
        assert_eq!(
            block_on(store.get_document_job(succeeded.id)).unwrap(),
            Some(succeeded)
        );
    }

    #[test]
    fn mem_store_document_retirement_is_durable_state_with_exact_completion() {
        let store = MemStore::default();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "retire me".into(),
            source_regions: Vec::new(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
        };
        block_on(store.upsert_document(&source)).unwrap();

        let tombstone = block_on(store.delete_document(source.id)).unwrap();
        assert_eq!(
            block_on(store.list_pending_document_retirements(None, 10)).unwrap(),
            vec![(source.id, tombstone)]
        );
        assert_eq!(
            block_on(store.delete_document(source.id)).unwrap(),
            tombstone
        );

        let recreated = block_on(store.upsert_document(&DocumentUpsert {
            canonical_text: "new lifecycle".into(),
            source_regions: Vec::new(),
            ..source.clone()
        }))
        .unwrap();
        assert_eq!(
            block_on(store.list_pending_document_retirements(None, 10)).unwrap(),
            vec![(source.id, tombstone)]
        );
        assert_eq!(
            block_on(store.get_pending_document_retirement(source.id)).unwrap(),
            Some(tombstone)
        );
        assert!(block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());
        assert!(!block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());

        let current_tombstone = block_on(store.delete_document(source.id)).unwrap();
        assert_ne!(current_tombstone, tombstone);
        assert_eq!(
            current_tombstone.content_revision,
            recreated.content_revision + 1
        );
        assert!(!block_on(store.complete_document_retirement(source.id, tombstone)).unwrap());
        assert!(
            block_on(store.complete_document_retirement(source.id, current_tombstone)).unwrap()
        );
        assert!(
            !block_on(store.complete_document_retirement(source.id, current_tombstone)).unwrap()
        );
        assert!(block_on(store.list_pending_document_retirements(None, 10))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mem_store_pending_retirement_cursor_advances_and_can_wrap() {
        let store = MemStore::default();
        let ids = [1_u128, 2, 3].map(|value| DocumentId(uuid::Uuid::from_u128(value)));
        let generations = ids.map(|id| block_on(store.delete_document(id)).unwrap());

        assert_eq!(
            block_on(store.list_pending_document_retirements(None, 2)).unwrap(),
            vec![(ids[0], generations[0]), (ids[1], generations[1])]
        );
        assert_eq!(
            block_on(store.list_pending_document_retirements(Some(ids[1]), 2)).unwrap(),
            vec![(ids[2], generations[2])]
        );
        assert!(
            block_on(store.list_pending_document_retirements(Some(ids[2]), 2))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            block_on(store.list_pending_document_retirements(None, 1)).unwrap(),
            vec![(ids[0], generations[0])]
        );
    }

    #[test]
    fn mem_store_explicit_retry_only_revives_current_failed_index_job() {
        let store = MemStore::default();
        let source = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "retry me".into(),
            source_regions: Vec::new(),
            updated_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1, 0).unwrap(),
        };
        let (_, queued) =
            block_on(store.upsert_document_and_enqueue_index(&source, "pipeline-v1", 2)).unwrap();
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "pipeline-v1", 9)).unwrap(),
            Some(queued.clone())
        );

        let claim_at = queued.available_at + chrono::Duration::seconds(1);
        let running =
            block_on(store.claim_document_job(claim_at, claim_at + chrono::Duration::minutes(1)))
                .unwrap()
                .unwrap();
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "pipeline-v1", 9)).unwrap(),
            Some(running.clone())
        );
        assert_eq!(
            block_on(store.record_document_job_failure(
                running.id,
                running.lease_token.unwrap(),
                claim_at + chrono::Duration::seconds(1),
                None,
                "embedding_failed",
                Some("service unavailable"),
            ))
            .unwrap(),
            Some(DocumentJobStatus::Failed)
        );
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "other-pipeline", 4)).unwrap(),
            None
        );

        let retried = block_on(store.retry_document_index_job(source.id, "pipeline-v1", 4))
            .unwrap()
            .unwrap();
        assert_eq!(retried.id, queued.id);
        assert_eq!(retried.status, DocumentJobStatus::Queued);
        assert_eq!(retried.attempt_count, 0);
        assert_eq!(retried.max_attempts, 4);
        assert_eq!(retried.lease_token, None);
        assert_eq!(retried.lease_expires_at, None);
        assert_eq!(retried.started_at, None);
        assert_eq!(retried.finished_at, None);
        assert_eq!(retried.last_error_code, None);
        assert_eq!(retried.last_error_detail, None);
        let document = block_on(store.get_document(source.id)).unwrap().unwrap();
        assert_eq!(document.processing_status, DocumentProcessingStatus::Queued);
        assert_eq!(document.indexed_revision, None);
        assert_eq!(document.index_fingerprint, None);
        assert_eq!(document.indexed_at, None);
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "pipeline-v1", 8)).unwrap(),
            Some(retried.clone())
        );

        let retry_claim_at = retried.available_at + chrono::Duration::seconds(1);
        let retry_running = block_on(store.claim_document_job(
            retry_claim_at,
            retry_claim_at + chrono::Duration::minutes(1),
        ))
        .unwrap()
        .unwrap();
        assert!(block_on(store.complete_document_index_job(
            retry_running.id,
            retry_running.lease_token.unwrap(),
            retry_claim_at + chrono::Duration::seconds(1),
        ))
        .unwrap());
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "pipeline-v1", 4)).unwrap(),
            None
        );

        let replacement = DocumentUpsert {
            canonical_text: "replacement".into(),
            source_regions: Vec::new(),
            updated_at: source.updated_at + chrono::Duration::seconds(1),
            ..source.clone()
        };
        let (_, cancelled) =
            block_on(store.upsert_document_and_enqueue_index(&replacement, "pipeline-v2", 2))
                .unwrap();
        assert_eq!(
            block_on(store.retry_document_index_job(replacement.id, "pipeline-v1", 4)).unwrap(),
            None
        );
        block_on(store.upsert_document_and_enqueue_index(
            &DocumentUpsert {
                canonical_text: "newer replacement".into(),
                source_regions: Vec::new(),
                updated_at: source.updated_at + chrono::Duration::seconds(2),
                ..replacement
            },
            "pipeline-v3",
            2,
        ))
        .unwrap();
        assert_eq!(
            block_on(store.get_document_job(cancelled.id))
                .unwrap()
                .unwrap()
                .status,
            DocumentJobStatus::Cancelled
        );
        assert_eq!(
            block_on(store.retry_document_index_job(source.id, "pipeline-v2", 4)).unwrap(),
            None
        );
    }
}
