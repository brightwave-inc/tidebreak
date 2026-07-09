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
//! Only the entities that exist today are modeled here. Persistence for tool
//! calls, connections, documents, and skills is added alongside the slices that
//! introduce those record types.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::Result;
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::ChatId;
use crate::model::{Chat, Message};

/// Durable metadata and conversation state.
///
/// Implementations must be safe to share across threads (`Send + Sync`) and are
/// held behind `Arc<dyn Store>`, so this trait stays object-safe.
#[async_trait]
pub trait Store: Send + Sync {
    /// Persist a new chat.
    async fn create_chat(&self, chat: &Chat) -> Result<()>;

    /// Fetch a chat by id, or `None` if it doesn't exist.
    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>>;

    /// List chats, most-recently-created first.
    async fn list_chats(&self) -> Result<Vec<Chat>>;

    /// Append a message to its chat.
    async fn append_message(&self, message: &Message) -> Result<()>;

    /// List a chat's messages in creation order.
    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>>;

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
/// `provider.anthropic.api_key`). Backed by the OS keychain on desktop, a
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
        chats: Mutex<HashMap<ChatId, Chat>>,
        settings: Mutex<HashMap<String, Value>>,
        events: Mutex<Vec<(ChatId, SequencedEvent)>>,
    }

    #[async_trait]
    impl Store for MemStore {
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
        async fn append_message(&self, _message: &Message) -> Result<()> {
            Ok(())
        }
        async fn list_messages(&self, _chat_id: ChatId) -> Result<Vec<Message>> {
            Ok(vec![])
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
            title: None,
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
