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

pub mod agent;
#[cfg(feature = "sqlite")]
pub mod db;
pub mod error;
pub mod event;
pub mod id;
#[cfg(feature = "keychain")]
pub mod keychain;
pub mod model;
pub mod provider;
pub mod storage;
pub mod tool;
#[cfg(feature = "tools")]
pub mod tools;

pub use agent::{Agent, AgentConfig, ToolRegistry};
#[cfg(feature = "sqlite")]
pub use db::DbStore;
pub use error::{AgentError, AgentErrorInfo, Result};
pub use event::AgentEvent;
pub use id::{CallId, MessageId, SessionId, StepId, TurnId};
#[cfg(feature = "keychain")]
pub use keychain::KeychainSecretProvider;
pub use model::{Message, Role, Session};
pub use provider::{
    ChatMessage, ChatRequest, ContentBlock, ModelProvider, ProviderEvent, ProviderId, StopReason,
    Usage,
};
pub use storage::{BlobStore, SecretProvider, Store};
pub use tool::{ApprovalClass, Tool, ToolCtx, ToolOutput, ToolSpec};
#[cfg(feature = "tools")]
pub use tools::{ListDir, ReadFile, WriteFile};
