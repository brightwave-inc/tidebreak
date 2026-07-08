//! OpenWave core — the open-core seam every client sits on.
//!
//! Holds the agent loop, tool registry, the `AgentEvent` stream, and the trait
//! contracts (`Tool`, `ModelProvider`, `Store`, `BlobStore`, `SecretProvider`)
//! plus their default local impls. Concrete model-provider adapters live in
//! `openwave-router`, not here.
//!
//! This crate must never depend on a specific client, and is independently
//! publishable on crates.io.
//!
//! Built incrementally with the M0 walking skeleton. Present so far: [`id`]
//! (typed identifiers), [`error`], [`model`] (the persisted session/message
//! model), [`tool`] (the tool contract), and [`provider`] (the model-provider
//! contract).

pub mod error;
pub mod event;
pub mod id;
pub mod model;
pub mod provider;
pub mod tool;

pub use error::{AgentError, AgentErrorInfo, Result};
pub use event::AgentEvent;
pub use id::{CallId, MessageId, SessionId, StepId, TurnId};
pub use model::{Message, Role, Session};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason,
    Usage,
};
pub use tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};
