//! OpenWave core — the agent loop, tool registry, provider adapters, and the
//! `Store` / `BlobStore` / `SecretProvider` / `Auth` traits every client sits
//! on.
//!
//! This crate is the open-core seam: it must never depend on a specific client.
//!
//! Landing incrementally with the M0 walking skeleton. This slice defines the
//! foundational types — identifiers, the error type, and the persisted
//! conversation model.

pub mod error;
pub mod id;
pub mod model;
pub mod provider;
pub mod tool;

pub use error::{AgentError, Result};
pub use id::{CallId, MessageId, SessionId, StepId, TurnId};
pub use model::{Message, Role, Session};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason,
    Usage,
};
pub use tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};
