//! Durable, bounded summaries of a conversation prefix.
//!
//! A semantic checkpoint is deliberately not a [`crate::Message`]: it is
//! agent-maintained context, not user-visible conversation history. The
//! checkpoint producer and provider projection arrive in follow-up slices; this
//! module owns the persisted contract they share.

use chrono::{DateTime, Utc};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};

/// The only checkpoint payload format this build can read and write.
///
/// Future formats must use a new value and add an explicit reader before they
/// are accepted. Treating an unknown payload as current context could turn a
/// failed migration into a misleading model prompt.
pub const CONTEXT_CHECKPOINT_FORMAT_V1: u16 = 1;

/// Largest UTF-8 payload accepted for one checkpoint.
///
/// There is one current checkpoint per conversation. Bounding it keeps a
/// checkpoint from becoming another unbounded transcript and leaves room for
/// recent messages in a provider context window.
pub const MAX_CONTEXT_CHECKPOINT_BYTES: usize = 12 * 1024;

/// A versioned summary of durable conversation history through one message.
///
/// `source_message_id` is an inclusive boundary in this conversation's
/// durable message order. Stores must reject a boundary owned by another chat,
/// and only permit it to advance, so an old worker cannot overwrite newer
/// context after recovering from a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCheckpoint {
    /// Conversation exclusively owning this checkpoint.
    pub chat_id: ChatId,
    /// Inclusive durable-message boundary covered by `content`.
    pub source_message_id: MessageId,
    /// Version of the structured checkpoint payload.
    pub format_version: u16,
    /// Opaque, bounded checkpoint payload.
    pub content: String,
    /// Host-stamped time this current checkpoint was committed.
    pub created_at: DateTime<Utc>,
}

impl ContextCheckpoint {
    /// Reject payloads this version cannot safely retain or later project.
    pub fn validate(&self) -> Result<()> {
        if self.format_version != CONTEXT_CHECKPOINT_FORMAT_V1 {
            return Err(AgentError::Store(format!(
                "unsupported context checkpoint format {}",
                self.format_version
            )));
        }
        if self.content.is_empty() || self.content.trim().is_empty() {
            return Err(AgentError::Store(
                "context checkpoint content must not be empty".into(),
            ));
        }
        if self.content.len() > MAX_CONTEXT_CHECKPOINT_BYTES {
            return Err(AgentError::Store(format!(
                "context checkpoint content exceeds {MAX_CONTEXT_CHECKPOINT_BYTES} bytes"
            )));
        }
        if self.content.contains('\0') {
            return Err(AgentError::Store(
                "context checkpoint content must not contain null bytes".into(),
            ));
        }
        Ok(())
    }
}

/// Result of storing one chat's next semantic checkpoint.
///
/// Exact retries recover the existing record. A retry that targets the same
/// source boundary but changes its content is a conflict rather than an
/// arbitrary rewrite; a boundary behind the current one is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveContextCheckpointOutcome {
    /// The supplied checkpoint became the conversation's current checkpoint.
    Saved(ContextCheckpoint),
    /// An identical checkpoint had already committed.
    Existing(ContextCheckpoint),
    /// A later source boundary is already durable for this conversation.
    Stale(ContextCheckpoint),
    /// The source boundary matches, but the durable payload differs.
    Conflict(ContextCheckpoint),
}
