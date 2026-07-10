//! OpenWave MCP — the Model Context Protocol server face.
//!
//! Exposes OpenWave's tools (the same `ToolRegistry` the agent uses) to an external
//! MCP client, so any MCP-speaking host can drive OpenWave's capabilities — for
//! example the `search` tool over a document index.
//!
//! [`McpServer`] answers `initialize`, `tools/list`, and `tools/call` and is
//! transport-agnostic; [`serve_stdio`] runs it over the standard newline-delimited
//! JSON-RPC stdio transport that MCP clients launch servers with.
//!
//! The protocol layer is hand-rolled ([`protocol`]) rather than pulling a full MCP
//! SDK — the surface a tool server needs is small, and this keeps the crate light
//! and fully unit-testable in-process.
//!
//! ```
//! use std::sync::Arc;
//! use openwave_core::{ChatId, ToolCtx, ToolRegistry};
//! use openwave_mcp::McpServer;
//!
//! # fn demo() {
//! let tools = Arc::new(ToolRegistry::new());
//! let ctx = ToolCtx { chat_id: ChatId::new(), project_id: None, workspace_dir: "/work".into() };
//! let server = McpServer::new(tools, ctx);
//! // then: `openwave_mcp::serve_stdio(server).await` inside an async runtime.
//! # let _ = server;
//! # }
//! ```
//!
//! Not yet enforced: the MCP session lifecycle (gating `tools/*` on a completed
//! `initialize`) — the server answers each request statelessly for now. The client
//! side (mounting *external* MCP tool servers into the agent) is a later slice.

pub mod protocol;
mod server;
mod stdio;

pub use protocol::PROTOCOL_VERSION;
pub use server::McpServer;
pub use stdio::{serve, serve_stdio};
