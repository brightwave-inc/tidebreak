//! The MCP server face: exposes OpenWave's tools to an external MCP client.
//!
//! [`McpServer`] answers `initialize`, `tools/list`, and `tools/call` against a
//! [`ToolRegistry`]. It's transport-agnostic — [`McpServer::handle`] takes a parsed
//! request and returns a response (or `None` for a notification); a transport (see
//! [`crate::serve_stdio`]) just moves bytes.
//!
//! Every exposed tool runs within one configured [`ToolCtx`] (a chat id and a
//! workspace directory), so the whole MCP session operates in a single workspace.
//! Tool-reported failures come back as a `tools/call` result with `isError = true`
//! (the MCP convention), not a JSON-RPC error; JSON-RPC errors are reserved for
//! protocol-level problems (unknown method, bad params, an infrastructure fault).

use std::sync::Arc;

use openwave_core::{ChatId, ToolCtx, ToolRegistry};
use serde_json::Value;

use crate::protocol::{
    error_code, CallToolParams, CallToolResult, Content, InitializeResult, ListToolsResult,
    Request, Response, RpcError, ServerCapabilities, ServerInfo, ToolDescriptor, ToolsCapability,
    PROTOCOL_VERSION,
};

/// An MCP server exposing a tool registry over the Model Context Protocol.
pub struct McpServer {
    tools: Arc<ToolRegistry>,
    ctx: ToolCtx,
    server_name: String,
    server_version: String,
}

impl McpServer {
    /// Build a server exposing `tools`, whose calls run within `ctx`.
    #[must_use]
    pub fn new(tools: Arc<ToolRegistry>, ctx: ToolCtx) -> Self {
        Self {
            tools,
            ctx,
            server_name: "openwave".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// The [`ChatId`] the exposed tools run under (from the configured [`ToolCtx`]).
    #[must_use]
    pub fn chat_id(&self) -> ChatId {
        self.ctx.chat_id
    }

    /// Handle one request. Returns the response to send, or `None` for a
    /// notification (which by JSON-RPC gets no reply).
    pub async fn handle(&self, req: Request) -> Option<Response> {
        if req.is_notification() {
            // Accept lifecycle notifications (e.g. `notifications/initialized`)
            // silently; nothing to reply.
            return None;
        }
        let id = req.reply_id();

        // The version must be exactly "2.0"; anything else (including an absent
        // field) is a malformed request per JSON-RPC 2.0.
        if req.jsonrpc != "2.0" {
            return Some(Response::error(
                id,
                RpcError::new(
                    error_code::INVALID_REQUEST,
                    "jsonrpc must be \"2.0\"".to_string(),
                ),
            ));
        }

        let outcome = match req.method.as_str() {
            "initialize" => Ok(self.initialize_result()),
            "tools/list" => Ok(self.list_tools_result()),
            "tools/call" => self.call_tool(req.params).await,
            other => Err(RpcError::method_not_found(other)),
        };
        Some(match outcome {
            Ok(result) => Response::result(id, result),
            Err(error) => Response::error(id, error),
        })
    }

    fn initialize_result(&self) -> Value {
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION,
            capabilities: ServerCapabilities {
                tools: ToolsCapability::default(),
            },
            server_info: ServerInfo {
                name: self.server_name.clone(),
                version: self.server_version.clone(),
            },
        };
        serde_json::to_value(result).expect("InitializeResult is always serializable")
    }

    fn list_tools_result(&self) -> Value {
        let mut tools: Vec<ToolDescriptor> = self
            .tools
            .specs()
            .into_iter()
            .map(|spec| ToolDescriptor {
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
            })
            .collect();
        // The registry is a HashMap; sort by name so the advertised list is stable
        // across launches (friendlier for clients and snapshot tests).
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        serde_json::to_value(ListToolsResult { tools })
            .expect("ListToolsResult is always serializable")
    }

    async fn call_tool(&self, params: Value) -> Result<Value, RpcError> {
        let params: CallToolParams = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("invalid tools/call params: {e}")))?;

        let tool = self
            .tools
            .get(&params.name)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown tool: {}", params.name)))?;

        // A tool's own failure is a result with isError = true, not a protocol
        // error; only an infrastructure fault (Err) becomes a JSON-RPC error.
        let output = tool
            .execute(&self.ctx, params.arguments)
            .await
            .map_err(|e| {
                RpcError::new(
                    error_code::INTERNAL_ERROR,
                    format!("tool execution failed: {e}"),
                )
            })?;

        let result = CallToolResult {
            content: vec![Content::Text {
                text: output.content,
            }],
            structured_content: output.data,
            is_error: output.is_error,
        };
        serde_json::to_value(result).map_err(|e| {
            RpcError::new(
                error_code::INTERNAL_ERROR,
                format!("failed to encode tool result: {e}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use async_trait::async_trait;
    use openwave_core::{ApprovalClass, Tool, ToolOutput, ToolSpec};
    use serde_json::json;

    /// A tool that echoes its `text` argument, or errors when asked.
    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "echo".into(),
                description: "Echo the text argument back.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }
        async fn execute(&self, _ctx: &ToolCtx, args: Value) -> openwave_core::Result<ToolOutput> {
            let text = args.get("text").and_then(Value::as_str).unwrap_or("");
            match text {
                "boom" => Ok(ToolOutput::error("asked to fail")),
                "withdata" => Ok(ToolOutput::text("ok").with_data(json!({"n": 1}))),
                _ => Ok(ToolOutput::text(format!("echo: {text}"))),
            }
        }
    }

    fn server() -> McpServer {
        let tools = Arc::new(ToolRegistry::new().with(Box::new(EchoTool)));
        let ctx = ToolCtx {
            chat_id: ChatId::new(),
            workspace_dir: PathBuf::from("/tmp/ws"),
        };
        McpServer::new(tools, ctx)
    }

    fn request(id: i64, method: &str, params: Value) -> Request {
        serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn initialize_reports_protocol_and_server_info() {
        let resp = server()
            .handle(request(1, "initialize", Value::Null))
            .await
            .unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "openwave");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn tools_list_advertises_the_registry() {
        let resp = server()
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap();
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");
        assert!(tools[0]["inputSchema"]["properties"]["text"].is_object());
    }

    #[tokio::test]
    async fn tools_call_runs_the_tool() {
        let resp = server()
            .handle(request(
                3,
                "tools/call",
                json!({"name": "echo", "arguments": {"text": "hi"}}),
            ))
            .await
            .unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "echo: hi");
    }

    #[tokio::test]
    async fn tool_failure_is_a_result_with_is_error_not_a_protocol_error() {
        let resp = server()
            .handle(request(
                4,
                "tools/call",
                json!({"name": "echo", "arguments": {"text": "boom"}}),
            ))
            .await
            .unwrap();
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["isError"], true);
    }

    #[tokio::test]
    async fn unknown_tool_and_unknown_method_are_protocol_errors() {
        let unknown_tool = server()
            .handle(request(
                5,
                "tools/call",
                json!({"name": "nope", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(unknown_tool.error.unwrap().code, error_code::INVALID_PARAMS);

        let unknown_method = server()
            .handle(request(6, "does/not/exist", Value::Null))
            .await
            .unwrap();
        assert_eq!(
            unknown_method.error.unwrap().code,
            error_code::METHOD_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn structured_output_is_surfaced_as_structured_content() {
        let resp = server()
            .handle(request(
                7,
                "tools/call",
                json!({"name": "echo", "arguments": {"text": "withdata"}}),
            ))
            .await
            .unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["structuredContent"], json!({"n": 1}));
        // Tools without structured data omit the field entirely.
        let plain = server()
            .handle(request(
                8,
                "tools/call",
                json!({"name": "echo", "arguments": {"text": "hi"}}),
            ))
            .await
            .unwrap();
        assert!(plain.result.unwrap().get("structuredContent").is_none());
    }

    #[tokio::test]
    async fn a_request_with_explicit_null_id_still_gets_a_reply() {
        // Present-but-null id is a request (must be answered), not a notification.
        let req: Request =
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": null, "method": "tools/list"}))
                .unwrap();
        assert!(!req.is_notification());
        let resp = server().handle(req).await.unwrap();
        assert_eq!(resp.id, Value::Null);
        assert!(resp.result.is_some());
    }

    #[tokio::test]
    async fn a_wrong_jsonrpc_version_is_an_invalid_request() {
        let req: Request =
            serde_json::from_value(json!({"jsonrpc": "1.0", "id": 9, "method": "tools/list"}))
                .unwrap();
        let resp = server().handle(req).await.unwrap();
        assert_eq!(resp.error.unwrap().code, error_code::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn a_notification_gets_no_response() {
        let note: Request = serde_json::from_value(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .unwrap();
        assert!(server().handle(note).await.is_none());
    }
}
