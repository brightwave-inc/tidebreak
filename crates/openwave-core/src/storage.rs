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
    Chat, DocumentJob, DocumentListCursor, DocumentRecord, DocumentScope, DocumentSummaryRecord,
    DocumentUpsert, Message, Project, ToolCallRecord,
};

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
    /// database store enforces this with a cascading foreign key. The store
    /// replaces the supplied `revision_token` with a fresh token so a deleted
    /// document lifecycle cannot be recreated with stale identity.
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

    /// Delete an authoritative document record. Idempotent for unknown ids.
    async fn delete_document(&self, _id: DocumentId) -> Result<()> {
        document_storage_unavailable()
    }

    /// Create or replace authoritative document content.
    ///
    /// A new record starts at revision one. Replacing an existing record increments
    /// its revision atomically, preserves `created_at`, and clears the index
    /// watermark. Returns the committed record.
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
    /// owned by the store rather than copied from source metadata.
    async fn upsert_document_and_enqueue_index(
        &self,
        _document: &DocumentUpsert,
        _pipeline_fingerprint: &str,
        _max_attempts: i32,
    ) -> Result<(DocumentRecord, DocumentJob)> {
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
    use std::collections::HashMap;
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
            let mut document = document.clone();
            document.revision_token = uuid::Uuid::new_v4();
            self.document_state
                .lock()
                .unwrap()
                .documents
                .insert(document.id, document);
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
        async fn delete_document(&self, id: DocumentId) -> Result<()> {
            let mut state = self.document_state.lock().unwrap();
            state.documents.remove(&id);
            state.jobs.retain(|_, job| job.document_id != id);
            Ok(())
        }
        async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
            if document.media_type.is_empty()
                || document.source_uri.as_deref() == Some("")
                || document
                    .project_id
                    .is_some_and(|id| !self.projects.lock().unwrap().contains_key(&id))
            {
                return Err(AgentError::Store("invalid document upsert".into()));
            }
            let mut state = self.document_state.lock().unwrap();
            let record = match state.documents.get(&document.id) {
                Some(existing) => {
                    let content_revision =
                        existing.content_revision.checked_add(1).ok_or_else(|| {
                            AgentError::Store(format!("document {} revision overflow", document.id))
                        })?;
                    DocumentRecord {
                        id: document.id,
                        project_id: document.project_id,
                        source_uri: document.source_uri.clone(),
                        media_type: document.media_type.clone(),
                        title: document.title.clone(),
                        canonical_text: document.canonical_text.clone(),
                        content_revision,
                        revision_token: uuid::Uuid::new_v4(),
                        processing_status: DocumentProcessingStatus::Queued,
                        indexed_revision: None,
                        index_fingerprint: None,
                        created_at: existing.created_at,
                        updated_at: document.updated_at,
                        indexed_at: None,
                    }
                }
                None => DocumentRecord {
                    id: document.id,
                    project_id: document.project_id,
                    source_uri: document.source_uri.clone(),
                    media_type: document.media_type.clone(),
                    title: document.title.clone(),
                    canonical_text: document.canonical_text.clone(),
                    content_revision: 1,
                    revision_token: uuid::Uuid::new_v4(),
                    processing_status: DocumentProcessingStatus::Queued,
                    indexed_revision: None,
                    index_fingerprint: None,
                    created_at: document.updated_at,
                    updated_at: document.updated_at,
                    indexed_at: None,
                },
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
            if let Some(existing) = state.documents.get(&document.id).filter(|existing| {
                existing.project_id == document.project_id
                    && existing.source_uri == document.source_uri
                    && existing.media_type == document.media_type
                    && existing.title == document.title
                    && existing.canonical_text == document.canonical_text
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
            let record = match state.documents.get(&document.id) {
                Some(existing) => DocumentRecord {
                    id: document.id,
                    project_id: document.project_id,
                    source_uri: document.source_uri.clone(),
                    media_type: document.media_type.clone(),
                    title: document.title.clone(),
                    canonical_text: document.canonical_text.clone(),
                    content_revision: existing.content_revision.checked_add(1).ok_or_else(
                        || AgentError::Store(format!("document {} revision overflow", document.id)),
                    )?,
                    revision_token: uuid::Uuid::new_v4(),
                    processing_status: DocumentProcessingStatus::Queued,
                    indexed_revision: None,
                    index_fingerprint: None,
                    created_at: existing.created_at,
                    updated_at: document.updated_at,
                    indexed_at: None,
                },
                None => DocumentRecord {
                    id: document.id,
                    project_id: document.project_id,
                    source_uri: document.source_uri.clone(),
                    media_type: document.media_type.clone(),
                    title: document.title.clone(),
                    canonical_text: document.canonical_text.clone(),
                    content_revision: 1,
                    revision_token: uuid::Uuid::new_v4(),
                    processing_status: DocumentProcessingStatus::Queued,
                    indexed_revision: None,
                    index_fingerprint: None,
                    created_at: document.updated_at,
                    updated_at: document.updated_at,
                    indexed_at: None,
                },
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

        block_on(store.delete_document(source.id)).unwrap();
        assert_eq!(block_on(store.get_document(source.id)).unwrap(), None);
        assert_eq!(block_on(store.get_document_job(first.1.id)).unwrap(), None);
    }
}
