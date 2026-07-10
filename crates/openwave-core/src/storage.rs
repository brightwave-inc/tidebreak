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
use crate::id::{ChatId, DocumentId, ProjectId};
use crate::model::{
    Chat, DocumentRecord, DocumentScope, DocumentUpsert, Message, Project, ToolCallRecord,
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

    /// Mark an exact `(revision, revision_token)` as indexed with `fingerprint`.
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

    /// Minimal in-memory `Store` — proves the trait is object-safe and usable
    /// behind `Arc<dyn Store>`, and exercises the signatures.
    #[derive(Default)]
    struct MemStore {
        projects: Mutex<HashMap<ProjectId, Project>>,
        documents: Mutex<HashMap<DocumentId, DocumentRecord>>,
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
            self.documents.lock().unwrap().insert(document.id, document);
            Ok(())
        }
        async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
            Ok(self.documents.lock().unwrap().get(&id).cloned())
        }
        async fn list_documents(&self, scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
            Ok(self
                .documents
                .lock()
                .unwrap()
                .values()
                .filter(|document| match scope {
                    DocumentScope::All => true,
                    DocumentScope::Unscoped => document.project_id.is_none(),
                    DocumentScope::Project(id) => document.project_id == Some(id),
                })
                .cloned()
                .collect())
        }
        async fn delete_document(&self, id: DocumentId) -> Result<()> {
            self.documents.lock().unwrap().remove(&id);
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
            let mut documents = self.documents.lock().unwrap();
            let record = match documents.get(&document.id) {
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
                    indexed_revision: None,
                    index_fingerprint: None,
                    created_at: document.updated_at,
                    updated_at: document.updated_at,
                    indexed_at: None,
                },
            };
            documents.insert(record.id, record.clone());
            Ok(record)
        }
        async fn mark_document_indexed(
            &self,
            id: DocumentId,
            revision: i64,
            revision_token: uuid::Uuid,
            fingerprint: &str,
            indexed_at: chrono::DateTime<chrono::Utc>,
        ) -> Result<bool> {
            let mut documents = self.documents.lock().unwrap();
            let Some(document) = documents.get_mut(&id) else {
                return Ok(false);
            };
            if document.content_revision != revision || document.revision_token != revision_token {
                return Ok(false);
            }
            document.indexed_revision = Some(revision);
            document.index_fingerprint = Some(fingerprint.to_string());
            document.indexed_at = Some(indexed_at);
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
    }
}
