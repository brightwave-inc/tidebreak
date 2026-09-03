//! Durable, bounded summaries of a conversation prefix.
//!
//! A semantic checkpoint is deliberately not a [`crate::Message`]: it is
//! agent-maintained context, not user-visible conversation history. The
//! producer writes only a strict structured payload before model-specific
//! reduction drops its covered prefix; provider projection treats the payload
//! as untrusted historical context.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};
use crate::id::{MessageId, SessionId};
use crate::provider::Usage;

/// Legacy checkpoint payload format. Still readable for projection; new writes
/// use [`CONTEXT_CHECKPOINT_FORMAT_V2`].
pub const CONTEXT_CHECKPOINT_FORMAT_V1: u16 = 1;

/// Current checkpoint payload format (structured fields + original requests).
///
/// Future formats must use a new value and add an explicit reader before they
/// are accepted. Treating an unknown payload as current context could turn a
/// failed migration into a misleading model prompt.
pub const CONTEXT_CHECKPOINT_FORMAT_V2: u16 = 2;

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

/// The model-produced payload projected for format v2 checkpoints.
///
/// Every field is inert conversation state. In particular, the schema has no
/// capability, instruction, or attachment-bytes field: source/output values
/// are identities only, and projection wraps the whole payload as untrusted
/// historical context. `original_requests` carries founding user asks across
/// re-compacts; the host merges them so the model cannot erase earlier asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointPayloadV2 {
    /// Payload schema version, independently explicit inside the stored JSON.
    pub version: u16,
    /// User asks from compacted prefixes, oldest first. Host-merged on write.
    pub original_requests: Vec<String>,
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

/// Legacy v1 shape kept for reading older rows during projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCheckpointPayloadV1 {
    pub version: u16,
    pub confirmed_decisions: Vec<String>,
    pub unresolved_questions: Vec<String>,
    pub task_state: Vec<String>,
    pub source_identities: Vec<String>,
    pub output_identities: Vec<String>,
    pub conclusions: Vec<String>,
}

impl From<ContextCheckpointPayloadV1> for ContextCheckpointPayloadV2 {
    fn from(value: ContextCheckpointPayloadV1) -> Self {
        Self {
            version: CONTEXT_CHECKPOINT_FORMAT_V2,
            original_requests: Vec::new(),
            confirmed_decisions: value.confirmed_decisions,
            unresolved_questions: value.unresolved_questions,
            task_state: value.task_state,
            source_identities: value.source_identities,
            output_identities: value.output_identities,
            conclusions: value.conclusions,
        }
    }
}

impl ContextCheckpointPayloadV2 {
    /// Parse untrusted model output and return canonical, bounded JSON.
    ///
    /// The producer fails open when this rejects an answer; it never stores a
    /// partial or vaguely-shaped summary merely because the model returned
    /// something readable.
    ///
    /// The checkpoint call sends no `response_format` — enforcing one costs the
    /// prompt cache it rides on — so the answer arrives as ordinary prose that
    /// happens to be JSON, sometimes in a fenced block. Stripping the fence and
    /// validating hard here is what replaces the wire-level constraint.
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

    /// Merge host-owned `original_requests` into a freshly parsed payload and
    /// re-canonicalize. Keeps earliest asks; stops appending when full.
    pub(crate) fn with_original_requests(
        content: &str,
        original_requests: Vec<String>,
    ) -> Result<String> {
        let content = strip_json_fence(content.trim());
        let mut payload: Self = serde_json::from_str(content).map_err(|error| {
            AgentError::msg(format!(
                "context checkpoint summarizer returned invalid JSON: {error}"
            ))
        })?;
        payload.version = CONTEXT_CHECKPOINT_FORMAT_V2;
        payload.original_requests = original_requests;
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
        if self.version != CONTEXT_CHECKPOINT_FORMAT_V2 {
            return Err(AgentError::msg(format!(
                "context checkpoint payload version {} is unsupported",
                self.version
            )));
        }
        validate_items("original_requests", &self.original_requests)?;
        let fields = [
            ("confirmed_decisions", &self.confirmed_decisions),
            ("unresolved_questions", &self.unresolved_questions),
            ("task_state", &self.task_state),
            ("source_identities", &self.source_identities),
            ("output_identities", &self.output_identities),
            ("conclusions", &self.conclusions),
        ];
        let mut populated = !self.original_requests.is_empty();
        for (name, items) in fields {
            validate_items(name, items)?;
            if !items.is_empty() {
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

fn validate_items(name: &str, items: &[String]) -> Result<()> {
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
    }
    Ok(())
}

/// Carry prior originals forward and append newly compacted user asks without
/// inventing replacements. When the list is full, keep the earliest asks.
pub fn merge_original_requests(prior: &[String], newly_compacted_asks: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(MAX_CONTEXT_CHECKPOINT_ITEMS);
    for item in prior {
        if out.len() >= MAX_CONTEXT_CHECKPOINT_ITEMS {
            break;
        }
        let trimmed = truncate_item(item);
        if trimmed.is_empty() || out.iter().any(|existing| existing == &trimmed) {
            continue;
        }
        out.push(trimmed);
    }
    for item in newly_compacted_asks {
        if out.len() >= MAX_CONTEXT_CHECKPOINT_ITEMS {
            break;
        }
        let trimmed = truncate_item(item);
        if trimmed.is_empty() || out.iter().any(|existing| existing == &trimmed) {
            continue;
        }
        out.push(trimmed);
    }
    out
}

fn truncate_item(item: &str) -> String {
    let trimmed = item.trim();
    if trimmed.len() <= MAX_CONTEXT_CHECKPOINT_ITEM_BYTES {
        return trimmed.to_owned();
    }
    let mut end = MAX_CONTEXT_CHECKPOINT_ITEM_BYTES;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].trim().to_owned()
}

/// Extract `original_requests` from a stored checkpoint payload.
pub fn original_requests_from_content(content: &str) -> Vec<String> {
    if let Ok(v2) = serde_json::from_str::<ContextCheckpointPayloadV2>(content) {
        return v2.original_requests;
    }
    Vec::new()
}

/// Unwrap a fenced code block, for runtimes that accept an output constraint
/// and then answer the prompt instead.
#[must_use]
pub fn strip_json_fence(content: &str) -> &str {
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
    pub chat_id: SessionId,
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
        if self.format_version != CONTEXT_CHECKPOINT_FORMAT_V1
            && self.format_version != CONTEXT_CHECKPOINT_FORMAT_V2
        {
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
    fn structured_payload_v2_is_canonical_bounded_and_nonempty() {
        let content = r#"```json
        {
          "version": 2,
          "original_requests": ["Choose SQLite locally."],
          "confirmed_decisions": ["Use SQLite locally."],
          "unresolved_questions": [],
          "task_state": ["Migration is pending."],
          "source_identities": ["source:abc"],
          "output_identities": [],
          "conclusions": ["The local path is sufficient."]
        }
        ```"#;
        let canonical = ContextCheckpointPayloadV2::parse_and_canonicalize(content).unwrap();
        assert_eq!(
            serde_json::from_str::<ContextCheckpointPayloadV2>(&canonical)
                .unwrap()
                .original_requests,
            ["Choose SQLite locally."]
        );

        let empty = r#"{"version":2,"original_requests":[],"confirmed_decisions":[],"unresolved_questions":[],"task_state":[],"source_identities":[],"output_identities":[],"conclusions":[]}"#;
        assert!(ContextCheckpointPayloadV2::parse_and_canonicalize(empty).is_err());

        let unknown = r#"{"version":2,"original_requests":[],"confirmed_decisions":["x"],"unresolved_questions":[],"task_state":[],"source_identities":[],"output_identities":[],"conclusions":[],"capabilities":["write"]}"#;
        assert!(ContextCheckpointPayloadV2::parse_and_canonicalize(unknown).is_err());
    }

    #[test]
    fn merge_original_requests_keeps_earliest_asks() {
        let prior = vec!["first".into(), "second".into()];
        let mut newly = Vec::new();
        for i in 0..20 {
            newly.push(format!("ask-{i}"));
        }
        let merged = merge_original_requests(&prior, &newly);
        assert_eq!(merged.len(), MAX_CONTEXT_CHECKPOINT_ITEMS);
        assert_eq!(merged[0], "first");
        assert_eq!(merged[1], "second");
        assert_eq!(merged[2], "ask-0");
    }
}
