//! The wire types: JSON-RPC 2.0 framing and the MCP messages layered on it.
//!
//! MCP is JSON-RPC 2.0. A message with an `id` is a request that expects a
//! response; one without an `id` is a notification (no reply). We keep the types
//! minimal — just what the server face needs to `initialize`, list tools, and call
//! them — rather than pulling a full MCP SDK.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP protocol revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC standard error codes.
pub mod error_code {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON is not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal server error.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// An incoming JSON-RPC message. An **absent** `id` ⇒ a notification (no
/// response); a present `id` (including the literal `null`) ⇒ a request that must
/// be answered.
#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    /// The JSON-RPC version; must be `"2.0"` (validated by the server).
    #[serde(default)]
    pub jsonrpc: String,
    /// Request id. The outer option distinguishes an *absent* id (`None`, a
    /// notification) from a present `null` id (`Some(Null)`, still a request), via
    /// [`present_or_absent`] — serde's plain `Option` collapses both to `None`.
    #[serde(default, deserialize_with = "present_or_absent")]
    pub id: Option<Value>,
    /// The method name (e.g. `"tools/list"`).
    pub method: String,
    /// Method parameters; `Null` when omitted.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// Whether this is a notification (no `id` at all, so no reply is sent).
    #[must_use]
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// The id to echo in the response (the present value, or `null` if `id` was
    /// the literal `null`). Only meaningful when this is not a notification.
    #[must_use]
    pub fn reply_id(&self) -> Value {
        self.id.clone().unwrap_or(Value::Null)
    }
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`; the
/// `#[serde(default)]` on the field supplies `None` when it is absent. This lets a
/// present-but-`null` `id` be told apart from a missing one.
fn present_or_absent<'de, D>(deserializer: D) -> std::result::Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// A JSON-RPC response — exactly one of `result` or `error` is set.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    /// Echoes the request id (JSON `null` when the id was absent/unknown).
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A success response carrying `result`.
    #[must_use]
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response.
    #[must_use]
    pub fn error(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Build an error with a code and message.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// `-32601 Method not found`.
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            error_code::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
        )
    }

    /// `-32602 Invalid params`.
    #[must_use]
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(error_code::INVALID_PARAMS, message)
    }
}

// --- MCP message payloads ---

/// Result of `initialize`.
#[derive(Debug, Clone, Serialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// Advertised server capabilities. Only tools for now.
#[derive(Debug, Clone, Serialize)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
}

/// The tools capability (empty object today; may carry flags like `listChanged`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ToolsCapability {}

/// Server identity reported at `initialize`.
#[derive(Debug, Clone, Serialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Result of `tools/list`.
#[derive(Debug, Clone, Serialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDescriptor>,
}

/// One tool as advertised to an MCP client (mirrors the agent's `ToolSpec`).
#[derive(Debug, Clone, Serialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Params of `tools/call`.
#[derive(Debug, Clone, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// Result of `tools/call`.
#[derive(Debug, Clone, Serialize)]
pub struct CallToolResult {
    pub content: Vec<Content>,
    /// A tool's optional structured payload (the agent tool's `data`), surfaced as
    /// MCP `structuredContent` when present.
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

/// A content block in a tool result. Only text today.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_has_no_id() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(req.is_notification());

        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert!(!req.is_notification());
    }

    #[test]
    fn response_serializes_result_xor_error() {
        let ok = Response::result(serde_json::json!(1), serde_json::json!({"k": 1}));
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"result\""));
        assert!(!s.contains("\"error\""));

        let err = Response::error(serde_json::json!(1), RpcError::method_not_found("x"));
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"error\""));
        assert!(!s.contains("\"result\""));
        assert!(s.contains("-32601"));
    }
}
