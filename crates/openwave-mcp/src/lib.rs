//! OpenWave MCP — the Model Context Protocol server face.
//!
//! Exposes OpenWave's read-only tools from the same `ToolRegistry` the agent uses
//! to an external MCP client, so any MCP-speaking host can drive capabilities such
//! as `search` over a document index. Workspace-mutating and sensitive tools stay
//! hidden until an approval-aware MCP execution bridge exists.
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
//! # fn demo() -> std::io::Result<()> {
//! let tools = Arc::new(ToolRegistry::new());
//! let ctx = ToolCtx::try_new(ChatId::new(), None, ".".into())?;
//! let server = McpServer::new(tools, ctx);
//! // then: `openwave_mcp::serve_stdio(server).await` inside an async runtime.
//! # let _ = server;
//! # Ok(())
//! # }
//! ```
//!
//! The server enforces the MCP session lifecycle: tool requests remain gated until
//! a valid `initialize` exchange is acknowledged by `notifications/initialized`.
//! The client side (mounting *external* MCP tool servers into the agent) is a
//! later slice.

pub mod protocol;
mod server;
mod stdio;

pub use protocol::PROTOCOL_VERSION;
pub use server::McpServer;
pub use stdio::{serve, serve_stdio};
