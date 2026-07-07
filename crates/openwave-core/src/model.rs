//! The persisted conversation model.
//!
//! Mirrors the `session` and `message` tables of `Store` schema v1. A
//! [`Session`] is a durable conversation that owns a workspace directory; a
//! [`Message`] is one user input or assistant answer within it.
//!
//! Turns and steps are runtime concepts of the agent loop (schema v1 has no
//! table for them — they are referenced by `turn_id`), so they live with the
//! loop, not here.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MessageId, SessionId, TurnId};

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
pub struct Session {
    /// Stable identifier.
    pub id: SessionId,
    /// Human-facing title; `None` until one is set or derived.
    pub title: Option<String>,
    /// Absolute path to this session's workspace directory.
    pub workspace_dir: PathBuf,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
}

/// One message in a session: user input or assistant text.
///
/// Tool calls are not messages; they persist separately (the `tool_call` table)
/// and are correlated by `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Stable identifier.
    pub id: MessageId,
    /// The session this message belongs to.
    pub session_id: SessionId,
    /// The turn this message was produced in.
    pub turn_id: TurnId,
    /// Who authored it.
    pub role: Role,
    /// The text body.
    pub content: String,
    /// When it was created.
    pub created_at: DateTime<Utc>,
}
