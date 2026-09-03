//! Tidebreak MCP — Model Context Protocol server and client faces.
//!
//! Exposes Tidebreak's tools from the same `ToolRegistry` the agent uses to an
//! external MCP client, so any MCP-speaking host can drive capabilities such as
//! `search` over a document index. Read-only tools are always exposed; workspace-
//! mutating and sensitive tools stay hidden unless an [`ApprovalGate`] is wired
//! in with [`McpServer::with_approval_gate`], which routes each mutating
//! `tools/call` through the same approval gate and standing grants the in-app
//! agent consults.
//!
//! [`ApprovalGate`]: tidebreak_core::ApprovalGate
//!
//! [`McpServer`] answers `initialize`, `tools/list`, and `tools/call` and is
//! transport-agnostic; [`serve_stdio`] runs it over the standard newline-delimited
//! JSON-RPC stdio transport that MCP clients launch servers with.
//! [`McpClient`] connects in the other direction: it initializes an external
//! MCP server — a spawned stdio child or a remote Streamable HTTP endpoint —
//! discovers its tools, and mounts namespaced proxies into an Tidebreak
//! [`ToolRegistry`](tidebreak_core::ToolRegistry).
//!
//! The protocol layer is hand-rolled ([`protocol`]) rather than pulling a full MCP
//! SDK — the surface a tool server needs is small, and this keeps the crate light
//! and fully unit-testable in-process.
//!
//! ```
//! use std::sync::Arc;
//! use tidebreak_core::{ChatId, ToolCtx, ToolRegistry};
//! use tidebreak_mcp::McpServer;
//!
//! # fn demo() -> std::io::Result<()> {
//! let tools = Arc::new(ToolRegistry::new());
//! let ctx = ToolCtx::try_new_legacy_workspace(ChatId::new(), None, ".".into())?;
//! let server = McpServer::new(tools, ctx);
//! // then: `tidebreak_mcp::serve_stdio(server).await` inside an async runtime.
//! # let _ = server;
//! # Ok(())
//! # }
//! ```
//!
//! The server enforces the MCP session lifecycle: tool requests remain gated until
//! a valid `initialize` exchange is acknowledged by `notifications/initialized`.
//! The client performs the matching lifecycle before it advertises any mounted
//! tools. External tools are conservatively classified as sensitive because an
//! MCP tool can reach outside Tidebreak's workspace and process boundary.

mod client;
mod http;
pub mod protocol;
mod server;
mod stdio;

pub use client::{
    CallBearerSource, McpClient, McpProbe, McpServerInfo, ResourceContent, DEFAULT_REQUEST_TIMEOUT,
    MAX_SERVER_NAME_BYTES,
};
pub use http::{validate_http_url, validate_http_url_with_credentials};
pub use protocol::PROTOCOL_VERSION;
pub use server::McpServer;
pub use stdio::{serve, serve_stdio};

#[cfg(test)]
mod doc_example {
    //! Mirror of the crate-level documentation example. Doctests are disabled
    //! workspace-wide (`doctest = false`), so this keeps the snippet compiling.

    #[test]
    fn crate_example_constructs_a_server() -> std::io::Result<()> {
        use std::sync::Arc;
        use tidebreak_core::{ChatId, ToolCtx, ToolRegistry};

        use crate::McpServer;

        let tools = Arc::new(ToolRegistry::new());
        let ctx = ToolCtx::try_new_legacy_workspace(ChatId::new(), None, ".".into())?;
        let server = McpServer::new(tools, ctx);
        // then: `tidebreak_mcp::serve_stdio(server).await` inside an async runtime.
        let _ = server;
        Ok(())
    }
}
