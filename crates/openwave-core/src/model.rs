//! The persisted conversation model.
//!
//! Mirrors the `chat` and `message` tables of `Store` schema v1. A
//! [`Chat`] is a durable conversation that owns a workspace directory; a
//! [`Message`] is one user input or assistant answer within it.
//!
//! Turns and steps are runtime concepts of the agent loop (schema v1 has no
//! table for them — they are referenced by `turn_id`), so they live with the
//! loop, not here.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ChatId, MessageId, ProjectId, TurnId};

/// An optional grouping of chats that share a workspace and (later) a document
/// corpus. A chat may belong to a project or stand alone — unlike some designs
/// that make a project mandatory, OpenWave keeps loose, projectless chats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable identifier.
    pub id: ProjectId,
    /// Human-facing title.
    pub title: Option<String>,
    /// Absolute path to the project's workspace/corpus root.
    pub workspace_dir: PathBuf,
    /// When the project was created.
    pub created_at: DateTime<Utc>,
}

/// Who authored a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The system prompt / instructions.
    System,
    /// Input from the human user.
    User,
    /// Output from the model.
    Assistant,
    /// A tool result fed back into the model.
    Tool,
}

/// A persistent conversation. Owns a workspace directory the agent operates in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    /// Stable identifier.
    pub id: ChatId,
    /// The project this chat belongs to, or `None` for a loose (projectless) chat.
    pub project_id: Option<ProjectId>,
    /// Human-facing title; `None` until one is set or derived.
    pub title: Option<String>,
    /// The model this chat runs against, or `None` to use the configured default.
    pub model: Option<String>,
    /// Absolute path to this chat's workspace directory.
    pub workspace_dir: PathBuf,
    /// When the chat was created.
    pub created_at: DateTime<Utc>,
}

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
    /// When it was created.
    pub created_at: DateTime<Utc>,
}

/// A persisted tool invocation — name, arguments, and (once finished) result.
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
    /// Result text fed back to the model, once completed.
    pub result: Option<String>,
    /// Whether the tool reported a failure.
    #[serde(default)]
    pub is_error: bool,
    /// When the call was recorded (args known).
    pub created_at: DateTime<Utc>,
    /// When the result was written, if completed.
    pub completed_at: Option<DateTime<Utc>>,
}
