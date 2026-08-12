use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::id::{ChatId, DocumentId, MessageId, TurnId};
use crate::image::ImageRef;
use crate::provider::MessageReasoning;

use super::chat_settings::Role;
use super::documents::DocumentBlob;

/// One message in a chat: user input or assistant text.
///
/// Tool calls are not messages; they persist separately (the `tool_call` table)
/// and are correlated by `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Stable identifier.
    pub id: MessageId,
    /// The chat this message belongs to.
    pub chat_id: ChatId,
    /// The turn this message was produced in.
    pub turn_id: TurnId,
    /// Who authored it.
    pub role: Role,
    /// The text body.
    pub content: String,
    /// The model-facing text body when it intentionally differs from
    /// [`Self::content`].
    ///
    /// The renderer never receives this field. It carries durable per-message
    /// context such as attachment routes, explicit skill invocation, and voice
    /// transcription guidance without changing the cache-sensitive operating
    /// prompt. `None` means the model sees `content` byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_content: Option<String>,
    /// Provider-native replay blocks this message's step produced, with the
    /// route that minted them.
    ///
    /// Persisted so an assistant message keeps reasoning or signed tool state
    /// across a reload and can replay it to the same model on a later turn.
    /// Empty for every user message.
    #[serde(default, skip_serializing_if = "MessageReasoning::is_empty")]
    pub reasoning: MessageReasoning,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

impl Message {
    /// The text reconstructed into the provider transcript.
    #[must_use]
    pub fn content_for_model(&self) -> &str {
        self.llm_content.as_deref().unwrap_or(&self.content)
    }

    /// Append context to the model projection without changing what the
    /// renderer shows.
    pub(crate) fn append_model_context(&mut self, context: &str) {
        self.llm_content
            .get_or_insert_with(|| self.content.clone())
            .push_str(context);
    }
}

/// Maximum number of attachments one message may carry.
///
/// The bound keeps one submit and its transcript projection finite. It also
/// bounds the ordinal columns, which the schema range-checks.
pub const MAX_MESSAGE_ATTACHMENTS: usize = 16;

/// Maximum size of one attachment copied into an exec workspace.
///
/// This is also the provider-neutral per-file workspace transfer limit. Keeping
/// the value beside attachment context ensures the route announced to the model
/// cannot disagree with the execution boundary.
pub const MAX_EXEC_WORKSPACE_FILE_BYTES: usize = 16 * 1_024 * 1_024;

/// Stable, collision-safe filename for one attachment in `documents/`.
///
/// The document id suffix makes the path independently derivable from a single
/// attachment record, so transcript reconstruction and lazy workspace
/// materialization cannot assign different names.
#[must_use]
pub fn exec_attachment_file_name(title: Option<&str>, document_id: DocumentId) -> String {
    const MAX_TITLE_BYTES: usize = 120;

    let leaf = title
        .unwrap_or("attachment")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment");
    let mut safe = String::new();
    for character in leaf.chars() {
        let character = if character.is_alphanumeric() || matches!(character, '-' | '_' | '.') {
            character
        } else if character.is_whitespace() {
            '-'
        } else {
            '_'
        };
        if safe.len() + character.len_utf8() > MAX_TITLE_BYTES {
            break;
        }
        safe.push(character);
    }
    let safe = safe.trim_matches(['.', '-', '_']);
    let safe = if safe.is_empty() { "attachment" } else { safe };
    let suffix = document_id.to_string();
    match safe.rsplit_once('.') {
        Some((stem, extension))
            if !stem.is_empty()
                && !extension.is_empty()
                && extension.len() <= 16
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()) =>
        {
            format!("{stem}--{suffix}.{extension}")
        }
        _ => format!("{safe}--{suffix}"),
    }
}

/// One image attached to a persisted message.
///
/// This is the durable half of an attachment: identity only, never bytes and
/// never a filesystem path. `ordinal` is the position the user submitted the
/// image at, and it is what makes a reloaded transcript reproduce the original
/// turn instead of an arbitrary permutation of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAttachment {
    /// The message this image is attached to.
    pub message_id: MessageId,
    /// The chat that owns the message, denormalized so conversation deletion
    /// and retention can scan attachments without joining through messages.
    pub chat_id: ChatId,
    /// Zero-based position within the message, unique per message.
    pub ordinal: i32,
    /// Blob identity, media type, and bounded dimensions.
    pub image: ImageRef,
    /// When the attachment was recorded.
    pub created_at: DateTime<Utc>,
}

impl MessageAttachment {
    /// Validate the bounds the schema also enforces.
    ///
    /// # Errors
    ///
    /// Returns a static reason when the ordinal is negative or past
    /// [`MAX_MESSAGE_ATTACHMENTS`], the blob id is nil, or the image itself
    /// fails [`ImageRef::validate`].
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ordinal < 0 || self.ordinal as usize >= MAX_MESSAGE_ATTACHMENTS {
            return Err("message attachment ordinal is out of range");
        }
        if self.image.blob_id.is_nil() {
            return Err("message attachment blob id must not be nil");
        }
        self.image.validate()
    }
}

/// One source document attached to a persisted message.
///
/// The document remains the authoritative owner of its bytes and decoded text.
/// Store projections carry enough of the current record to compose durable
/// model context at acceptance and to describe the attachment to renderer and
/// execution surfaces later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDocumentAttachment {
    /// The message this document is attached to.
    pub message_id: MessageId,
    /// The chat that owns both the message and document.
    pub chat_id: ChatId,
    /// Zero-based position among the message's file attachments.
    pub ordinal: i32,
    /// The attached document.
    pub document_id: DocumentId,
    /// Human-facing name captured from the document record.
    pub title: Option<String>,
    /// Media type captured from the document record.
    pub media_type: String,
    /// Retained original bytes, when the document has them.
    pub source_blob: Option<DocumentBlob>,
    /// Whether `read_document` can return decoded canonical text.
    pub readable: bool,
    /// When the attachment was recorded.
    pub created_at: DateTime<Utc>,
}

impl MessageDocumentAttachment {
    /// Validate the bounds the schema also enforces.
    ///
    /// # Errors
    ///
    /// Returns a static reason when the ordinal is out of range, the document
    /// id is nil, or the media type is empty.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ordinal < 0 || self.ordinal as usize >= MAX_MESSAGE_ATTACHMENTS {
            return Err("message document attachment ordinal is out of range");
        }
        if self.document_id.0.is_nil() {
            return Err("message document attachment id must not be nil");
        }
        if self.media_type.is_empty() {
            return Err("message document attachment media type must not be empty");
        }
        if self
            .source_blob
            .as_ref()
            .is_some_and(|blob| !blob.has_content_addressed_id())
        {
            return Err("message document attachment source blob is invalid");
        }
        Ok(())
    }
}

const MAX_ANNOUNCED_FILES: usize = 8;
const MAX_ANNOUNCED_IMAGES: usize = 8;
const VOICE_INPUT_CONTEXT: &str = "[Voice input: The user dictated this message and it was transcribed from speech, so some words may be transcribed incorrectly — especially names, technical terms, homophones, and punctuation. Interpret accordingly, and ask for clarification if a key term seems garbled.]";

/// Build the durable model projection for one user message.
///
/// The visible text remains the user's exact input. Host-derived context is
/// kept in a separate projection so the operating prompt stays stable across
/// ordinary turn-to-turn changes and the transcript never renders internal
/// attachment routes or voice guidance.
pub(crate) fn user_message_llm_content(
    content: &str,
    images: &[ImageRef],
    documents: &[MessageDocumentAttachment],
    invoked_skills: &[String],
    voice_input_used: bool,
) -> Option<String> {
    if images.is_empty() && documents.is_empty() && invoked_skills.is_empty() && !voice_input_used {
        return None;
    }

    let mut sections = vec!["# Important context".to_owned()];
    if voice_input_used {
        sections.push(VOICE_INPUT_CONTEXT.to_owned());
    }
    if !invoked_skills.is_empty() {
        sections.push(format!(
            "The user explicitly invoked these skills for this message: {}. Read each one's `.tidebreak/skills/<name>/SKILL.md` before doing the work and follow it — these are instructions for this message, not optional catalog entries.",
            invoked_skills.join(", ")
        ));
    }
    if !images.is_empty() || !documents.is_empty() {
        sections.push(attachment_context(images, documents));
    }
    sections.push("# User message".to_owned());
    sections.push(content.to_owned());
    Some(sections.join("\n\n"))
}

fn attachment_context(images: &[ImageRef], documents: &[MessageDocumentAttachment]) -> String {
    let mut lines = vec!["<attachments>".to_owned()];
    for (index, image) in images.iter().take(MAX_ANNOUNCED_IMAGES).enumerate() {
        lines.push(format!(
            "image_{}: id={}; media_type={}; byte_size={}; this is image content block {}",
            index + 1,
            image.blob_id,
            image.media_type.as_str(),
            image.byte_len,
            index + 1
        ));
    }
    for document in documents.iter().take(MAX_ANNOUNCED_FILES) {
        let metadata = serde_json::json!({
            "document_id": document.document_id,
            "title": document.title.as_deref().unwrap_or("Attachment"),
            "media_type": document.media_type,
            "byte_size": document.source_blob.as_ref().map(|blob| blob.byte_len),
        });
        lines.push(format!(
            "file: {}",
            serde_json::to_string(&metadata).expect("attachment metadata is serializable")
        ));
        lines.push(format!("  route: {}", attachment_route(document)));
    }
    let omitted = images.len().saturating_sub(MAX_ANNOUNCED_IMAGES)
        + documents.len().saturating_sub(MAX_ANNOUNCED_FILES);
    if omitted > 0 {
        lines.push(format!("{omitted} more attachment(s) omitted."));
    }
    lines.push("</attachments>".to_owned());
    lines.join("\n")
}

fn attachment_route(document: &MessageDocumentAttachment) -> String {
    if document.readable {
        return format!(
            "readable via read_document(document_id=\"{}\")",
            document.document_id
        );
    }
    let Some(source_blob) = document.source_blob.as_ref() else {
        return "raw bytes unavailable because no source blob is retained".to_owned();
    };
    if source_blob.byte_len > MAX_EXEC_WORKSPACE_FILE_BYTES as u64 {
        return format!(
            "raw bytes not materialized because the file exceeds the \
             {MAX_EXEC_WORKSPACE_FILE_BYTES}-byte exec workspace limit"
        );
    }
    let path = format!(
        "documents/{}",
        exec_attachment_file_name(document.title.as_deref(), document.document_id)
    );
    let hint = attachment_script_hint(&document.media_type).map_or_else(String::new, |script| {
        format!("; helper: python3 .tidebreak/exec-scripts/{script} {path}")
    });
    format!("raw bytes at {path} in the exec workspace{hint}")
}

fn attachment_script_hint(media_type: &str) -> Option<&'static str> {
    let media_type = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "application/pdf" => Some("render_pdf.py"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        | "application/msword"
        | "application/vnd.ms-powerpoint"
        | "application/vnd.oasis.opendocument.text"
        | "application/vnd.oasis.opendocument.presentation" => Some("render_office.py"),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel" => Some("analyze_xlsx.py"),
        _ => None,
    }
}

/// Where an accepted tool invocation must execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallExecution {
    /// Execute inside the server-owned agent runtime.
    Server,
    /// Execute through a separately leased trusted client surface.
    Client,
    /// Foreground orchestration committed atomically by the turn state machine.
    ///
    /// These records rebuild structured model history, but are never eligible
    /// for either generic server or client execution.
    Orchestration,
}

impl ToolCallExecution {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Client => "client",
            Self::Orchestration => "orchestration",
        }
    }
}

/// Durable lifecycle of one tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// Canonical arguments are committed and execution has not resolved.
    Pending,
    /// Execution returned a successful model-facing result.
    Completed,
    /// Execution returned a stable failure and model-facing result.
    Failed,
    /// Execution was intentionally cancelled with a model-facing result.
    Cancelled,
}

impl ToolCallStatus {
    /// Stable database and wire representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether execution has a durable final result.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// Exact terminal payload used for idempotent tool-call resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallResolution {
    /// Successful execution.
    Completed { result: String },
    /// Failed execution with stable machine and optional diagnostic detail.
    Failed {
        result: String,
        error_code: String,
        error_detail: Option<String>,
    },
    /// Intentional cancellation, including a user declining a native prompt.
    Cancelled { result: String },
}

impl ToolCallResolution {
    /// Terminal state represented by this payload.
    #[must_use]
    pub const fn status(&self) -> ToolCallStatus {
        match self {
            Self::Completed { .. } => ToolCallStatus::Completed,
            Self::Failed { .. } => ToolCallStatus::Failed,
            Self::Cancelled { .. } => ToolCallStatus::Cancelled,
        }
    }

    /// Model-facing result for this terminal outcome.
    #[must_use]
    pub fn result(&self) -> &str {
        match self {
            Self::Completed { result }
            | Self::Failed { result, .. }
            | Self::Cancelled { result } => result,
        }
    }
}

/// A persisted tool invocation — canonical identity, arguments, and lifecycle.
///
/// Distinct from [`Message`]: the model transcript rebuilds `ToolUse` /
/// `ToolResult` blocks from these rows so cross-turn context keeps structured
/// tool activity, not just free text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Stable id (same as the live [`crate::id::CallId`] on the event stream).
    pub id: crate::id::CallId,
    /// Chat this call belongs to.
    pub chat_id: ChatId,
    /// Turn that produced the call.
    pub turn_id: TurnId,
    /// Provider-facing tool-use id (Anthropic `tool_use.id`, OpenAI `tool_call_id`).
    pub provider_id: String,
    /// Tool name.
    pub name: String,
    /// Parsed JSON arguments.
    pub arguments: serde_json::Value,
    /// The exact argument bytes the provider streamed, kept only when they
    /// would not parse as JSON — `arguments` then holds the coerced empty
    /// object and this is the sole copy of what the model actually sent.
    /// Bounded, untrusted text for post-hoc debugging of a garbled stream;
    /// never re-parsed. Absent for well-formed calls and historical rows.
    #[serde(default)]
    pub raw_arguments: Option<String>,
    /// Which trusted surface owns execution.
    pub execution: ToolCallExecution,
    /// Durable execution state.
    pub status: ToolCallStatus,
    /// Result text fed back to the model once terminal.
    pub result: Option<String>,
    /// Closed projection persisted with the call, including exec preview image
    /// references. Absent for historical rows and tools without a rich result.
    #[serde(default)]
    pub result_preview: Option<crate::ToolResultPreview>,
    /// Provider-native blocks for same-route replay of a provider-executed
    /// call. Absent for host tools and when the adapter captured nothing
    /// opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_replay: Option<crate::provider::ProviderToolReplay>,
    /// Stable machine-readable failure code, only for `failed`.
    pub error_code: Option<String>,
    /// Bounded diagnostic failure detail, only for `failed`.
    pub error_detail: Option<String>,
    /// Exact client executor that owns the pending lease or resolved the call.
    pub client_executor_id: Option<Uuid>,
    /// Expiry of the exact client executor lease.
    pub client_lease_expires_at: Option<DateTime<Utc>>,
    /// When the call was recorded (args known).
    pub created_at: DateTime<Utc>,
    /// When the terminal outcome was written.
    pub resolved_at: Option<DateTime<Utc>>,
}

impl ToolCallRecord {
    /// Maximum UTF-8 bytes accepted for provider identity or tool name.
    pub const MAX_LABEL_LEN: usize = 256;
    /// Maximum serialized canonical argument bytes.
    pub const MAX_ARGUMENT_BYTES: usize = 128 * 1024;
    /// Maximum model-facing terminal result bytes.
    pub const MAX_RESULT_BYTES: usize = 512 * 1024;
    /// Maximum stable failure code bytes.
    pub const MAX_ERROR_CODE_LEN: usize = 128;
    /// Maximum diagnostic failure detail bytes.
    pub const MAX_ERROR_DETAIL_LEN: usize = 4 * 1024;
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    use crate::id::DocumentId;

    #[test]
    fn exec_attachment_names_are_pathless_and_collision_safe() {
        let first_id = DocumentId(Uuid::from_u128(1));
        let second_id = DocumentId(Uuid::from_u128(2));
        let first = exec_attachment_file_name(Some("../reports/quarter 1.pdf"), first_id);
        let second = exec_attachment_file_name(Some("../reports/quarter 1.pdf"), second_id);

        assert_eq!(first, "quarter-1--00000000-0000-0000-0000-000000000001.pdf");
        assert_ne!(first, second);
        assert!(!first.contains(['/', '\\']));
    }
}
