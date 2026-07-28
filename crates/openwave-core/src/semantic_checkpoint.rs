//! Durable, bounded summaries of a conversation prefix.
//!
//! A semantic checkpoint is deliberately not a [`crate::Message`]: it is
//! agent-maintained context, not user-visible conversation history. The
//! The producer writes only a strict structured payload before model-specific
//! reduction drops its covered prefix; provider projection treats the payload
//! as untrusted historical context.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, MessageId};
use crate::provider::{ResponseFormat, Usage};
use crate::tool::input_schema_for;

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

/// Most entries retained in any one structured checkpoint category.
pub const MAX_CONTEXT_CHECKPOINT_ITEMS: usize = 16;

/// Largest UTF-8 entry retained in a structured checkpoint category.
pub const MAX_CONTEXT_CHECKPOINT_ITEM_BYTES: usize = 1_024;

/// The model-produced payload projected for format v1 checkpoints.
///
/// Every field is inert conversation state. In particular, the schema has no
/// capability, instruction, or attachment-bytes field: source/output values
/// are identities only, and projection wraps the whole payload as untrusted
/// historical context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointPayloadV1 {
    /// Payload schema version, independently explicit inside the stored JSON.
    pub version: u16,
    /// User choices that the covered prefix explicitly settled.
    pub confirmed_decisions: Vec<String>,
    /// Questions that remained open at the end of the covered prefix.
    pub unresolved_questions: Vec<String>,
    /// Current plan/status facts established by the covered prefix.
    pub task_state: Vec<String>,
    /// Opaque source/document/citation identities mentioned in the prefix.
    pub source_identities: Vec<String>,
    /// Opaque durable output/revision identities mentioned in the prefix.
    pub output_identities: Vec<String>,
    /// Important findings established by the covered prefix.
    pub conclusions: Vec<String>,
}

/// Name the checkpoint's output constraint carries on the wire.
///
/// The Anthropic adapter turns it into a tool name, so it stays within
/// `^[a-zA-Z0-9_-]{1,64}$`.
const CONTEXT_CHECKPOINT_SCHEMA_NAME: &str = "context_checkpoint";

impl ContextCheckpointPayloadV1 {
    /// The output constraint the checkpoint's maintenance call sends.
    ///
    /// The system prompt still spells the shape out in prose. That is not
    /// redundancy for its own sake: this adapter layer covers any
    /// OpenAI-compatible endpoint, including local runtimes that accept
    /// `response_format` and then ignore it, and the prompt is what those
    /// runtimes have to go on.
    pub(crate) fn response_format() -> ResponseFormat {
        ResponseFormat::JsonSchema {
            name: CONTEXT_CHECKPOINT_SCHEMA_NAME.to_owned(),
            schema: input_schema_for::<Self>(),
        }
    }

    /// Parse untrusted model output and return canonical, bounded JSON.
    ///
    /// The producer fails open when this rejects an answer; it never stores a
    /// partial or vaguely-shaped summary merely because the model returned
    /// something readable.
    ///
    /// A constrained completion arrives as bare JSON, so the fence strip below
    /// is dead weight against a provider that honored the schema. It stays for
    /// the ones that do not — an OpenAI-compatible runtime that accepts
    /// `response_format` and ignores it still answers the prompt, in a fenced
    /// block, and there is no reason to throw that away.
    pub(crate) fn parse_and_canonicalize(content: &str) -> Result<String> {
        let content = strip_json_fence(content.trim());
        let payload: Self = serde_json::from_str(content).map_err(|error| {
            AgentError::msg(format!(
                "context checkpoint summarizer returned invalid JSON: {error}"
            ))
        })?;
        payload.validate()?;
        let canonical = serde_json::to_string(&payload).map_err(|error| {
            AgentError::msg(format!(
                "context checkpoint payload could not be serialized: {error}"
            ))
        })?;
        if canonical.len() > MAX_CONTEXT_CHECKPOINT_BYTES {
            return Err(AgentError::msg(format!(
                "context checkpoint payload exceeds {MAX_CONTEXT_CHECKPOINT_BYTES} bytes"
            )));
        }
        Ok(canonical)
    }

    fn validate(&self) -> Result<()> {
        if self.version != CONTEXT_CHECKPOINT_FORMAT_V1 {
            return Err(AgentError::msg(format!(
                "context checkpoint payload version {} is unsupported",
                self.version
            )));
        }
        let fields = [
            ("confirmed_decisions", &self.confirmed_decisions),
            ("unresolved_questions", &self.unresolved_questions),
            ("task_state", &self.task_state),
            ("source_identities", &self.source_identities),
            ("output_identities", &self.output_identities),
            ("conclusions", &self.conclusions),
        ];
        let mut populated = false;
        for (name, items) in fields {
            if items.len() > MAX_CONTEXT_CHECKPOINT_ITEMS {
                return Err(AgentError::msg(format!(
                    "context checkpoint field {name} exceeds {MAX_CONTEXT_CHECKPOINT_ITEMS} items"
                )));
            }
            for item in items {
                if item.trim().is_empty()
                    || item.len() > MAX_CONTEXT_CHECKPOINT_ITEM_BYTES
                    || item.contains('\0')
                {
                    return Err(AgentError::msg(format!(
                        "context checkpoint field {name} contains an invalid item"
                    )));
                }
                populated = true;
            }
        }
        if !populated {
            return Err(AgentError::msg(
                "context checkpoint payload contains no conversation state",
            ));
        }
        Ok(())
    }
}

fn strip_json_fence(content: &str) -> &str {
    content
        .strip_prefix("```json")
        .and_then(|content| content.strip_suffix("```"))
        .or_else(|| {
            content
                .strip_prefix("```")
                .and_then(|content| content.strip_suffix("```"))
        })
        .map(str::trim)
        .unwrap_or(content)
}

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
    /// Cumulative provider usage spent producing checkpoints through this one.
    ///
    /// This is intentionally not folded into the user turn's model-step or
    /// terminal usage totals.
    pub usage: Usage,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_payload_is_canonical_bounded_and_nonempty() {
        let content = r#"```json
        {
          "version": 1,
          "confirmed_decisions": ["Use SQLite locally."],
          "unresolved_questions": [],
          "task_state": ["Migration is pending."],
          "source_identities": ["source:abc"],
          "output_identities": [],
          "conclusions": ["The local path is sufficient."]
        }
        ```"#;
        let canonical = ContextCheckpointPayloadV1::parse_and_canonicalize(content).unwrap();
        assert_eq!(
            serde_json::from_str::<ContextCheckpointPayloadV1>(&canonical)
                .unwrap()
                .confirmed_decisions,
            ["Use SQLite locally."]
        );

        let empty = r#"{"version":1,"confirmed_decisions":[],"unresolved_questions":[],"task_state":[],"source_identities":[],"output_identities":[],"conclusions":[]}"#;
        assert!(ContextCheckpointPayloadV1::parse_and_canonicalize(empty).is_err());

        let unknown = r#"{"version":1,"confirmed_decisions":["x"],"unresolved_questions":[],"task_state":[],"source_identities":[],"output_identities":[],"conclusions":[],"capabilities":["write"]}"#;
        assert!(ContextCheckpointPayloadV1::parse_and_canonicalize(unknown).is_err());
    }
}
