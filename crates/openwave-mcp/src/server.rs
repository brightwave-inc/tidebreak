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

use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

use openwave_core::{
    ApprovalClass, ApprovalDecision, ApprovalGate, ApprovalRequest, CallId, ChatId, StandingGrants,
    ToolActionPreview, ToolApprovalKind, ToolCtx, ToolOutput, ToolRegistry, TurnId, VERSION,
};
use serde_json::Value;

use crate::protocol::{
    error_code, CallToolParams, CallToolResult, Content, InitializeParams, InitializeResult,
    ListToolsResult, Request, Response, RpcError, ServerCapabilities, ServerInfo, ToolDescriptor,
    ToolsCapability, PROTOCOL_VERSION,
};

const SESSION_UNINITIALIZED: u8 = 0;
const SESSION_AWAITING_INITIALIZED: u8 = 1;
const SESSION_READY: u8 = 2;

/// An MCP server exposing a tool registry over the Model Context Protocol.
///
/// [`ApprovalClass::ReadOnly`] tools always cross this boundary. Workspace and
/// Sensitive (mutating) tools stay hidden unless an [`ApprovalGate`] is wired in
/// via [`McpServer::with_approval_gate`]; with a gate configured, each mutating
/// `tools/call` is routed through the same gate — and any standing grants — the
/// in-app agent consults, so a headless client runs mutating tools only with
/// consent.
pub struct McpServer {
    tools: Arc<ToolRegistry>,
    ctx: ToolCtx,
    server_name: String,
    server_version: String,
    session_state: AtomicU8,
    approval: Option<ApprovalBridge>,
}

/// The consent surface for mutating `tools/call`s: a gate consulted after any
/// standing grant, mirroring the agent's park-and-resume policy.
struct ApprovalBridge {
    gate: Arc<dyn ApprovalGate>,
    grants: Arc<StandingGrants>,
}

impl ApprovalBridge {
    /// Decide a Workspace/Sensitive call. Returns `None` when it may run, or
    /// `Some(reason)` describing why it was denied (or could not be presented to
    /// the client). A standing grant the user already gave for this chat runs
    /// without re-consulting the gate.
    async fn decide(
        &self,
        chat_id: ChatId,
        tool_name: &str,
        class: ApprovalClass,
        arguments: &Value,
    ) -> Option<String> {
        let kind = ToolApprovalKind::for_tool_name(tool_name);
        let action = ToolActionPreview::build(tool_name, arguments);
        // The canonical arguments decide authority; `action` beside it only
        // describes them for the card.
        // The MCP face carries no project membership, so a grant reaches it
        // only at chat level.
        if self
            .grants
            .covers(chat_id, None, tool_name, kind, arguments)
        {
            return None;
        }
        let request = ApprovalRequest {
            call_id: CallId::new(),
            chat_id,
            turn_id: TurnId::new(),
            tool_name: tool_name.to_string(),
            class,
            kind,
            preview: action,
            // The MCP face has no chat permission mode; every gated call is a
            // human's to decide.
            auto_judge: false,
        };
        // MCP has no durable steer journal, so register without a journal identity.
        let registration = self.gate.register(request, None).await;
        match registration.decision.await {
            ApprovalDecision::Approve => None,
            ApprovalDecision::Reject { reason } => Some(reason),
        }
    }
}

impl McpServer {
    /// Build a server exposing the read-only entries in `tools`, whose calls run
    /// within `ctx`. Mutating tools stay hidden until a gate is wired in with
    /// [`McpServer::with_approval_gate`].
    #[must_use]
    pub fn new(tools: Arc<ToolRegistry>, ctx: ToolCtx) -> Self {
        Self {
            tools,
            ctx,
            server_name: "openwave".to_string(),
            server_version: VERSION.to_string(),
            session_state: AtomicU8::new(SESSION_UNINITIALIZED),
            approval: None,
        }
    }

    /// Expose Workspace and Sensitive tools, routing each such `tools/call`
    /// through `gate` for a decision. An approved call runs; a denied or
    /// unpresentable one comes back as a `tools/call` result with `isError`,
    /// never a silent drop. Without a gate the server keeps advertising and
    /// executing only ReadOnly tools.
    #[must_use]
    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approval = Some(ApprovalBridge {
            gate,
            grants: Arc::new(StandingGrants::new()),
        });
        self
    }

    /// Consult `grants` before the gate, so a repeated in-scope action the user
    /// already approved for this chat runs without re-prompting. Has no effect
    /// unless [`McpServer::with_approval_gate`] was called first.
    #[must_use]
    pub fn with_standing_grants(mut self, grants: Arc<StandingGrants>) -> Self {
        if let Some(bridge) = self.approval.as_mut() {
            bridge.grants = grants;
        }
        self
    }

    /// Whether a tool of `class` crosses this server's boundary. ReadOnly always
    /// does; mutating classes only once an approval gate is configured.
    fn exposes(&self, class: ApprovalClass) -> bool {
        class == ApprovalClass::ReadOnly || self.approval.is_some()
    }

    /// The [`ChatId`] the exposed tools run under (from the configured [`ToolCtx`]).
    #[must_use]
    pub fn chat_id(&self) -> ChatId {
        self.ctx.chat_id
    }

    /// Handle one request. Returns the response to send, or `None` for a
    /// notification (which by JSON-RPC gets no reply).
    pub async fn handle(&self, req: Request) -> Option<Response> {
        // The version must be exactly "2.0"; anything else (including an absent
        // field) is a malformed request per JSON-RPC 2.0.
        if req.jsonrpc != "2.0" {
            if req.is_notification() {
                return None;
            }
            let id = req.reply_id();
            return Some(Response::error(
                if request_id_is_valid(&id) {
                    id
                } else {
                    Value::Null
                },
                RpcError::new(
                    error_code::INVALID_REQUEST,
                    "jsonrpc must be \"2.0\"".to_string(),
                ),
            ));
        }

        if req.is_notification() {
            self.handle_notification(&req);
            return None;
        }
        let id = req.reply_id();
        if !request_id_is_valid(&id) {
            return Some(Response::error(
                Value::Null,
                RpcError::new(
                    error_code::INVALID_REQUEST,
                    "request id must be a string or number",
                ),
            ));
        }

        let outcome = match req.method.as_str() {
            "initialize" => self.initialize(req.params),
            "ping" => validate_optional_object_params(req.params).map(|()| serde_json::json!({})),
            "tools/list" => self.require_ready().map(|()| self.list_tools_result()),
            "tools/call" => match self.require_ready() {
                Ok(()) => self.call_tool(req.params).await,
                Err(error) => Err(error),
            },
            other => Err(RpcError::method_not_found(other)),
        };
        Some(match outcome {
            Ok(result) => Response::result(id, result),
            Err(error) => Response::error(id, error),
        })
    }

    fn handle_notification(&self, req: &Request) {
        if req.method != "notifications/initialized"
            || !(req.params.is_null() || req.params.is_object())
        {
            return;
        }

        let _ = self.session_state.compare_exchange(
            SESSION_AWAITING_INITIALIZED,
            SESSION_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn initialize(&self, params: Value) -> Result<Value, RpcError> {
        let params: InitializeParams = serde_json::from_value(params)
            .map_err(|e| RpcError::invalid_params(format!("invalid initialize params: {e}")))?;

        // The protocol says the two sides agree on a version or the handshake
        // fails. This face implements exactly one version, so a request for any
        // other is refused with the supported set rather than answered with a
        // version the client never asked for — the same strictness this crate's
        // client applies to servers.
        if params.protocol_version != PROTOCOL_VERSION {
            let mut error = RpcError::invalid_params(format!(
                "unsupported protocol version {}",
                params.protocol_version
            ));
            error.data = Some(serde_json::json!({
                "supported": [PROTOCOL_VERSION],
                "requested": params.protocol_version,
            }));
            return Err(error);
        }

        self.session_state
            .compare_exchange(
                SESSION_UNINITIALIZED,
                SESSION_AWAITING_INITIALIZED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| {
                RpcError::new(error_code::INVALID_REQUEST, "session already initialized")
            })?;

        Ok(self.initialize_result())
    }

    fn require_ready(&self) -> Result<(), RpcError> {
        if self.session_state.load(Ordering::Acquire) == SESSION_READY {
            Ok(())
        } else {
            Err(RpcError::new(
                error_code::INVALID_REQUEST,
                "session is not initialized",
            ))
        }
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
            .filter(|spec| {
                self.tools
                    .get(&spec.name)
                    .is_some_and(|tool| self.exposes(tool.approval_class()))
            })
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
            .filter(|tool| self.exposes(tool.approval_class()))
            .ok_or_else(|| RpcError::invalid_params(format!("unknown tool: {}", params.name)))?;

        // Hold the call to the contract `tools/list` advertised, before consent
        // is asked for it and before the tool's own deserializer decides what
        // enforcement means. The in-app agent validates at the same point
        // (`openwave_core::tool::schema_mismatch`); an MCP client reaching the
        // same registry through this face gets the same answer instead of
        // whichever fields the tool happened to read.
        //
        // `arguments` is an object or it is nothing: a scalar or an array is
        // not an argument map, and reporting that as `invalid_params` tells the
        // client what to fix rather than surfacing a schema error about types.
        if !params.arguments.is_object() {
            return Err(RpcError::invalid_params(format!(
                "arguments for {} must be a JSON object",
                params.name
            )));
        }
        if let Some(mismatch) = self.tools.schema_mismatch(&params.name, &params.arguments) {
            return Err(RpcError::invalid_params(format!(
                "arguments for {} do not satisfy its schema: {mismatch}",
                params.name
            )));
        }

        // A mutating call crosses the boundary only through the approval bridge.
        // `exposes` guarantees `self.approval` is `Some` for a non-ReadOnly tool.
        let class = tool.approval_class();
        if class != ApprovalClass::ReadOnly {
            if let Some(bridge) = self.approval.as_ref() {
                if let Some(reason) = bridge
                    .decide(self.ctx.chat_id, &params.name, class, &params.arguments)
                    .await
                {
                    // Denied or unpresentable: a clear error result, not a
                    // silent drop and not a protocol error.
                    return tool_result(ToolOutput::error(reason));
                }
            }
        }

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

        tool_result(output)
    }
}

/// Encode a [`ToolOutput`] as a `tools/call` result value.
fn tool_result(output: ToolOutput) -> Result<Value, RpcError> {
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

fn request_id_is_valid(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

fn validate_optional_object_params(params: Value) -> Result<(), RpcError> {
    if params.is_null() || params.is_object() {
        Ok(())
    } else {
        Err(RpcError::invalid_params("params must be an object"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Barrier;

    use async_trait::async_trait;
    use chrono::Utc;
    use openwave_core::{
        ApprovalClass, AutoApproveGate, GrantLevel, RefuseGate, StandingGrant, Tool, ToolOutput,
        ToolSpec,
    };
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

    struct ClassifiedTool {
        name: &'static str,
        class: ApprovalClass,
        ran: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl Tool for ClassifiedTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.to_string(),
                description: format!("{} test tool", self.name),
                input_schema: json!({"type": "object"}),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            self.class
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: Value) -> openwave_core::Result<ToolOutput> {
            self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(ToolOutput::text(self.name))
        }
    }

    fn server() -> McpServer {
        server_with(ToolRegistry::new().with(Box::new(EchoTool)))
    }

    fn server_with(tools: ToolRegistry) -> McpServer {
        let ctx = ToolCtx::new_legacy_workspace(ChatId::new(), None, PathBuf::from("/tmp/ws"));
        McpServer::new(Arc::new(tools), ctx)
    }

    fn ctx_for(chat_id: ChatId) -> ToolCtx {
        ToolCtx::new_legacy_workspace(chat_id, None, PathBuf::from("/tmp/ws"))
    }

    fn request(id: i64, method: &str, params: Value) -> Request {
        serde_json::from_value(json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .unwrap()
    }

    fn initialize_params() -> Value {
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0.0"}
        })
    }

    fn initialized_notification(params: Value) -> Request {
        serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": params
        }))
        .unwrap()
    }

    async fn initialize_session(server: &McpServer) {
        let response = server
            .handle(request(1, "initialize", initialize_params()))
            .await
            .unwrap();
        assert!(response.error.is_none());
        assert!(server
            .handle(initialized_notification(Value::Null))
            .await
            .is_none());
    }

    async fn initialized_server() -> McpServer {
        let server = server();
        initialize_session(&server).await;
        server
    }

    #[tokio::test]
    async fn initialize_reports_protocol_and_server_info() {
        let resp = server()
            .handle(request(1, "initialize", initialize_params()))
            .await
            .unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(result["serverInfo"]["name"], "openwave");
        assert_eq!(result["serverInfo"]["version"], VERSION);
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn tools_remain_gated_until_initialized_notification() {
        let server = server();

        // An early acknowledgement cannot unlock a later session.
        assert!(server
            .handle(initialized_notification(Value::Null))
            .await
            .is_none());
        let initialized = server
            .handle(request(1, "initialize", initialize_params()))
            .await
            .unwrap();
        assert!(initialized.error.is_none());

        let early_list = server
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap();
        assert_eq!(early_list.error.unwrap().code, error_code::INVALID_REQUEST);

        // A malformed lifecycle notification is ignored and cannot unlock tools.
        assert!(server
            .handle(initialized_notification(json!([])))
            .await
            .is_none());
        let still_early = server
            .handle(request(3, "tools/list", Value::Null))
            .await
            .unwrap();
        assert_eq!(still_early.error.unwrap().code, error_code::INVALID_REQUEST);

        // Notification params are optional and, when present, are an open object
        // that may contain protocol metadata.
        assert!(server
            .handle(initialized_notification(
                json!({"_meta": {"trace": "test"}})
            ))
            .await
            .is_none());
        let ready = server
            .handle(request(4, "tools/list", Value::Null))
            .await
            .unwrap();
        assert!(ready.error.is_none());
    }

    #[tokio::test]
    async fn invalid_initialize_does_not_consume_the_session() {
        let server = server();
        let invalid = server
            .handle(request(1, "initialize", Value::Null))
            .await
            .unwrap();
        assert_eq!(invalid.error.unwrap().code, error_code::INVALID_PARAMS);

        let valid = server
            .handle(request(2, "initialize", initialize_params()))
            .await
            .unwrap();
        assert!(valid.error.is_none());
    }

    #[test]
    fn only_one_concurrent_initialize_request_can_claim_the_session() {
        let server = Arc::new(server());
        let barrier = Arc::new(Barrier::new(3));
        let results = std::thread::scope(|scope| {
            let first_server = Arc::clone(&server);
            let first_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                first_barrier.wait();
                first_server.initialize(initialize_params())
            });
            let second_server = Arc::clone(&server);
            let second_barrier = Arc::clone(&barrier);
            let second = scope.spawn(move || {
                second_barrier.wait();
                second_server.initialize(initialize_params())
            });
            barrier.wait();
            [first.join().unwrap(), second.join().unwrap()]
        });

        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
        assert_eq!(
            results.iter().find_map(|r| r.as_ref().err()).unwrap().code,
            error_code::INVALID_REQUEST
        );
    }

    #[tokio::test]
    async fn ping_is_available_throughout_the_session_lifecycle() {
        let server = server();
        for id in 1..=2 {
            let response = server
                .handle(request(id, "ping", Value::Null))
                .await
                .unwrap();
            assert_eq!(response.result.unwrap(), json!({}));
        }

        server
            .handle(request(3, "initialize", initialize_params()))
            .await
            .unwrap();
        let awaiting = server.handle(request(4, "ping", json!({}))).await.unwrap();
        assert_eq!(awaiting.result.unwrap(), json!({}));

        server.handle(initialized_notification(Value::Null)).await;
        let ready = server
            .handle(request(5, "ping", Value::Null))
            .await
            .unwrap();
        assert_eq!(ready.result.unwrap(), json!({}));

        let invalid = server.handle(request(6, "ping", json!([]))).await.unwrap();
        assert_eq!(invalid.error.unwrap().code, error_code::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn server_refuses_an_unsupported_protocol_version() {
        let server = server();
        let mut params = initialize_params();
        params["protocolVersion"] = json!("2099-01-01");
        let response = server
            .handle(request(1, "initialize", params))
            .await
            .unwrap();
        let error = response.error.unwrap();
        assert_eq!(error.code, error_code::INVALID_PARAMS);
        assert_eq!(error.data.unwrap()["supported"], json!([PROTOCOL_VERSION]));

        // A refused handshake leaves the session uninitialized, so the client can
        // retry with a version this face speaks.
        let retried = server
            .handle(request(2, "initialize", initialize_params()))
            .await
            .unwrap();
        assert_eq!(retried.result.unwrap()["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn tools_list_advertises_the_registry() {
        let resp = initialized_server()
            .await
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap();
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "echo");
        assert!(tools[0]["inputSchema"]["properties"]["text"].is_object());
    }

    #[tokio::test]
    async fn only_read_only_tools_are_advertised_or_executable() {
        let read_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let workspace_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sensitive_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tools = ToolRegistry::new()
            .with(Box::new(ClassifiedTool {
                name: "read",
                class: ApprovalClass::ReadOnly,
                ran: Arc::clone(&read_ran),
            }))
            .with(Box::new(ClassifiedTool {
                name: "write",
                class: ApprovalClass::Workspace,
                ran: Arc::clone(&workspace_ran),
            }))
            .with(Box::new(ClassifiedTool {
                name: "external",
                class: ApprovalClass::Sensitive,
                ran: Arc::clone(&sensitive_ran),
            }));
        let server = server_with(tools);
        initialize_session(&server).await;

        let listed = server
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap()
            .result
            .unwrap();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["read"]);

        for (id, name) in [(3, "write"), (4, "external")] {
            let response = server
                .handle(request(
                    id,
                    "tools/call",
                    json!({"name": name, "arguments": {}}),
                ))
                .await
                .unwrap();
            assert_eq!(response.error.unwrap().code, error_code::INVALID_PARAMS);
        }
        assert!(!workspace_ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!sensitive_ran.load(std::sync::atomic::Ordering::SeqCst));

        let response = server
            .handle(request(
                5,
                "tools/call",
                json!({"name": "read", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert!(response.error.is_none());
        assert!(read_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn approval_gate_advertises_and_runs_an_approved_mutating_tool() {
        let read_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sensitive_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tools = ToolRegistry::new()
            .with(Box::new(ClassifiedTool {
                name: "read",
                class: ApprovalClass::ReadOnly,
                ran: Arc::clone(&read_ran),
            }))
            .with(Box::new(ClassifiedTool {
                name: "external",
                class: ApprovalClass::Sensitive,
                ran: Arc::clone(&sensitive_ran),
            }));
        let server = McpServer::new(Arc::new(tools), ctx_for(ChatId::new()))
            .with_approval_gate(Arc::new(AutoApproveGate));
        initialize_session(&server).await;

        // A configured gate advertises the mutating tool alongside the read-only one.
        let listed = server
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap()
            .result
            .unwrap();
        let names: Vec<&str> = listed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["external", "read"]);

        // The approved Sensitive call runs and returns a normal (non-error) result.
        let response = server
            .handle(request(
                3,
                "tools/call",
                json!({"name": "external", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["isError"], false);
        assert!(sensitive_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn denied_mutating_call_is_an_error_result_not_a_protocol_error() {
        let sensitive_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tools = ToolRegistry::new().with(Box::new(ClassifiedTool {
            name: "external",
            class: ApprovalClass::Sensitive,
            ran: Arc::clone(&sensitive_ran),
        }));
        let server = McpServer::new(Arc::new(tools), ctx_for(ChatId::new()))
            .with_approval_gate(Arc::new(RefuseGate));
        initialize_session(&server).await;

        let response = server
            .handle(request(
                2,
                "tools/call",
                json!({"name": "external", "arguments": {}}),
            ))
            .await
            .unwrap();
        // A refusal is a result with isError = true, never a JSON-RPC error, and
        // the tool never runs.
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("requires approval"));
        assert!(!sensitive_ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn arguments_that_miss_the_advertised_schema_never_reach_the_tool() {
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let tools = ToolRegistry::new()
            .with(Box::new(EchoTool))
            .with(Box::new(ClassifiedTool {
                name: "read",
                class: ApprovalClass::ReadOnly,
                ran: Arc::clone(&ran),
            }));
        let server = McpServer::new(Arc::new(tools), ctx_for(ChatId::new()));
        initialize_session(&server).await;

        // `echo` advertises a required `text`; a call without it is the client's
        // bug, answered as invalid params rather than run with a default.
        let response = server
            .handle(request(
                2,
                "tools/call",
                json!({"name": "echo", "arguments": {"other": 1}}),
            ))
            .await
            .unwrap();
        assert_eq!(response.error.unwrap().code, error_code::INVALID_PARAMS);

        // Non-object `arguments` are not an argument map at all, whatever the
        // tool's schema says, and the tool is never entered.
        for arguments in [json!("text"), json!([1, 2]), Value::Null] {
            let response = server
                .handle(request(
                    3,
                    "tools/call",
                    json!({"name": "read", "arguments": arguments}),
                ))
                .await
                .unwrap();
            assert_eq!(response.error.unwrap().code, error_code::INVALID_PARAMS);
        }
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));

        // Omitted entirely is "no arguments", which the permissive schema takes.
        let response = server
            .handle(request(4, "tools/call", json!({"name": "read"})))
            .await
            .unwrap();
        assert!(response.error.is_none());
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_standing_grant_runs_a_mutating_tool_without_consulting_the_gate() {
        let chat_id = ChatId::new();
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // "search" is the sole standing-grantable action today.
        let tools = ToolRegistry::new().with(Box::new(ClassifiedTool {
            name: "search",
            class: ApprovalClass::Sensitive,
            ran: Arc::clone(&ran),
        }));
        let grant = StandingGrant::new(
            GrantLevel::Chat { chat_id },
            "search",
            ToolApprovalKind::for_tool_name("search"),
            Utc::now(),
        )
        .expect("search is grantable");
        let server = McpServer::new(Arc::new(tools), ctx_for(chat_id))
            // A refusing gate proves the grant, not the gate, let the call through.
            .with_approval_gate(Arc::new(RefuseGate))
            .with_standing_grants(Arc::new(StandingGrants::from_grants(vec![grant])));
        initialize_session(&server).await;

        let response = server
            .handle(request(
                2,
                "tools/call",
                json!({"name": "search", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert!(response.error.is_none());
        assert_eq!(response.result.unwrap()["isError"], false);
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn tools_call_runs_the_tool() {
        let resp = initialized_server()
            .await
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
        let resp = initialized_server()
            .await
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
        let server = initialized_server().await;
        let unknown_tool = server
            .handle(request(
                5,
                "tools/call",
                json!({"name": "nope", "arguments": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(unknown_tool.error.unwrap().code, error_code::INVALID_PARAMS);

        let unknown_method = server
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
        let server = initialized_server().await;
        let resp = server
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
        let plain = server
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
    async fn invalid_mcp_request_ids_get_an_invalid_request_response() {
        let server = initialized_server().await;

        // Present null is distinguishable from a notification, but MCP forbids it
        // (along with non-string/non-number values).
        let req: Request =
            serde_json::from_value(json!({"jsonrpc": "2.0", "id": null, "method": "tools/list"}))
                .unwrap();
        assert!(!req.is_notification());
        let response = server.handle(req).await.unwrap();
        assert_eq!(response.id, Value::Null);
        assert_eq!(response.error.unwrap().code, error_code::INVALID_REQUEST);

        for invalid_id in [json!(true), json!([]), json!({})] {
            let req: Request = serde_json::from_value(json!({
                "jsonrpc": "2.0",
                "id": invalid_id,
                "method": "tools/list"
            }))
            .unwrap();
            let response = server.handle(req).await.unwrap();
            assert_eq!(response.id, Value::Null);
            assert_eq!(response.error.unwrap().code, error_code::INVALID_REQUEST);
        }

        let fractional: Request = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": 1.5,
            "method": "tools/list"
        }))
        .unwrap();
        let response = server.handle(fractional).await.unwrap();
        assert_eq!(response.id, json!(1.5));
        assert!(response.result.is_some());
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
    async fn malformed_notification_cannot_advance_the_session() {
        let server = server();
        let initialized = server
            .handle(request(1, "initialize", initialize_params()))
            .await
            .unwrap();
        assert!(initialized.error.is_none());

        let note: Request = serde_json::from_value(json!({
            "jsonrpc": "1.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        assert!(server.handle(note).await.is_none());

        let tools = server
            .handle(request(2, "tools/list", Value::Null))
            .await
            .unwrap();
        assert_eq!(tools.error.unwrap().code, error_code::INVALID_REQUEST);
    }

    #[tokio::test]
    async fn a_notification_gets_no_response() {
        let note = initialized_notification(Value::Null);
        assert!(server().handle(note).await.is_none());
    }
}
