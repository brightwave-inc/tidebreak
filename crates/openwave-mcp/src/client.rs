//! MCP client support for mounting an external stdio tool server.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_core::{
    AgentError, ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolRegistry, ToolSpec,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::protocol::{Response, RpcError, PROTOCOL_VERSION};

const CLIENT_NAME: &str = "openwave";

/// Default maximum time to wait for one external MCP request.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

type BoxReader = Box<dyn AsyncBufRead + Send + Unpin>;
type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Identity reported by the external server during MCP initialization.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpServerInfo {
    /// The server's self-reported name.
    pub name: String,
    /// The server's self-reported version.
    pub version: String,
}

/// A connected external MCP server whose tools can be mounted into OpenWave.
///
/// The configured `server_name` is a stable local namespace, independent of the
/// server's self-reported identity. A remote tool named `search` on a configured
/// server named `docs` is advertised as `mcp__docs__search`.
pub struct McpClient {
    server_name: String,
    server_info: McpServerInfo,
    tools: Vec<MountedTool>,
    session: Arc<Mutex<Session>>,
}

impl McpClient {
    /// Connect over an already-open pair of streams.
    ///
    /// This performs the full MCP initialize lifecycle and paginated tool
    /// discovery before returning. The streams and session remain alive as long
    /// as this client or any tool mounted from it remains alive.
    pub async fn connect<R, W>(server_name: impl Into<String>, reader: R, writer: W) -> Result<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::connect_with_timeout(server_name, reader, writer, DEFAULT_REQUEST_TIMEOUT).await
    }

    /// Connect over streams with an explicit per-request timeout.
    pub async fn connect_with_timeout<R, W>(
        server_name: impl Into<String>,
        reader: R,
        writer: W,
        request_timeout: Duration,
    ) -> Result<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::connect_session(
            server_name.into(),
            Session::new(
                Box::new(BufReader::new(reader)),
                Box::new(writer),
                None,
                request_timeout,
            ),
        )
        .await
    }

    /// Spawn and connect to an external MCP stdio server.
    ///
    /// Callers configure the executable, arguments, environment, and working
    /// directory on `command`; this method owns stdin/stdout and inherits stderr.
    /// The child is terminated if the client session is dropped.
    pub async fn spawn(server_name: impl Into<String>, command: Command) -> Result<Self> {
        Self::spawn_with_timeout(server_name, command, DEFAULT_REQUEST_TIMEOUT).await
    }

    /// Spawn a server with an explicit per-request timeout.
    pub async fn spawn_with_timeout(
        server_name: impl Into<String>,
        mut command: Command,
        request_timeout: Duration,
    ) -> Result<Self> {
        let server_name = server_name.into();
        validate_server_name(&server_name)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| mcp_error("could not spawn external server", error))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| mcp_message("external server did not provide stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| mcp_message("external server did not provide stdout"))?;

        Self::connect_session(
            server_name,
            Session::new(
                Box::new(BufReader::new(stdout)),
                Box::new(stdin),
                Some(child),
                request_timeout,
            ),
        )
        .await
    }

    async fn connect_session(server_name: String, mut session: Session) -> Result<Self> {
        validate_server_name(&server_name)?;

        let initialize = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": CLIENT_NAME,
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        let initialized: InitializeResponse = decode_result("initialize", initialize)?;
        if initialized.protocol_version != PROTOCOL_VERSION {
            return Err(mcp_message(format!(
                "external server selected unsupported protocol version {}",
                initialized.protocol_version
            )));
        }
        if initialized.capabilities.tools.is_none() {
            return Err(mcp_message(
                "external server did not advertise the tools capability",
            ));
        }
        session
            .notify("notifications/initialized", json!({}))
            .await?;

        let descriptors = discover_tools(&mut session).await?;
        let tools = build_mounted_tools(&server_name, descriptors)?;
        Ok(Self {
            server_name,
            server_info: initialized.server_info,
            tools,
            session: Arc::new(Mutex::new(session)),
        })
    }

    /// The stable local name used to namespace this server's mounted tools.
    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// The identity reported by the server during initialization.
    #[must_use]
    pub fn server_info(&self) -> &McpServerInfo {
        &self.server_info
    }

    /// The model-visible specs that this connection can mount.
    pub fn tools(&self) -> impl Iterator<Item = &ToolSpec> {
        self.tools.iter().map(|tool| &tool.spec)
    }

    /// Register namespaced proxies for all discovered tools.
    ///
    /// Registry registration follows [`ToolRegistry::register`] semantics, so a
    /// tool with the same fully-qualified name replaces an older registration.
    pub fn mount(&self, registry: &mut ToolRegistry) {
        for tool in &self.tools {
            registry.register(Box::new(McpTool {
                spec: tool.spec.clone(),
                remote_name: tool.remote_name.clone(),
                server_name: self.server_name.clone(),
                session: Arc::clone(&self.session),
            }));
        }
    }
}

struct Session {
    reader: BoxReader,
    writer: BoxWriter,
    next_id: u64,
    request_timeout: Duration,
    // Keeping the child owns its lifecycle; `kill_on_drop(true)` handles both a
    // normal registry teardown and a failed initialization.
    _child: Option<Child>,
}

impl Session {
    fn new(
        reader: BoxReader,
        writer: BoxWriter,
        child: Option<Child>,
        request_timeout: Duration,
    ) -> Self {
        Self {
            reader,
            writer,
            next_id: 1,
            request_timeout,
            _child: child,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| mcp_message("JSON-RPC request id exhausted"))?;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        timeout(self.request_timeout, self.write_value(&request))
            .await
            .map_err(|_| mcp_message(format!("timed out writing {method} request")))??;
        match timeout(self.request_timeout, self.read_response(id)).await {
            Ok(result) => result,
            Err(_) => {
                let _ = self
                    .notify(
                        "notifications/cancelled",
                        json!({"requestId": id, "reason": "OpenWave MCP request timed out"}),
                    )
                    .await;
                Err(mcp_message(format!(
                    "external server timed out handling {method}"
                )))
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        timeout(self.request_timeout, self.write_value(&notification))
            .await
            .map_err(|_| mcp_message(format!("timed out writing {method} notification")))?
    }

    async fn write_value(&mut self, value: &Value) -> Result<()> {
        let mut encoded = serde_json::to_vec(value)?;
        encoded.push(b'\n');
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|error| mcp_error("could not write to external server", error))?;
        self.writer
            .flush()
            .await
            .map_err(|error| mcp_error("could not flush external server request", error))
    }

    async fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        loop {
            let mut line = String::new();
            let read = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|error| mcp_error("could not read from external server", error))?;
            if read == 0 {
                return Err(mcp_message("external server closed stdout before replying"));
            }
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)
                .map_err(|error| mcp_error("external server returned malformed JSON-RPC", error))?;

            // Server notifications are asynchronous and need no response. Ping
            // is part of the MCP base protocol in either direction. The client
            // advertises no other request-producing capabilities, so fail closed
            // for any other server request.
            if let Some(method) = value.get("method").and_then(Value::as_str) {
                if let Some(id) = value.get("id") {
                    self.write_server_request_response(id.clone(), method)
                        .await?;
                }
                continue;
            }

            let response: IncomingResponse = serde_json::from_value(value).map_err(|error| {
                mcp_error(
                    "external server returned an invalid JSON-RPC response",
                    error,
                )
            })?;
            if response.jsonrpc != "2.0" {
                return Err(mcp_message(
                    "external server response did not use JSON-RPC 2.0",
                ));
            }
            if response.id.as_u64().is_some_and(|id| id < expected_id) {
                // A server may complete an already-cancelled timed-out request
                // after the client has moved on. Calls are serialized, so only
                // an older numeric id can safely be recognized as stale.
                continue;
            }
            if response.id != json!(expected_id) {
                return Err(mcp_message(format!(
                    "external server replied with id {} while waiting for {expected_id}",
                    response.id
                )));
            }
            return match (response.result, response.error) {
                (Some(result), None) => Ok(result),
                (None, Some(error)) => Err(mcp_message(format!(
                    "external server returned JSON-RPC error {}: {}",
                    error.code, error.message
                ))),
                _ => Err(mcp_message(
                    "external server response must contain exactly one of result or error",
                )),
            };
        }
    }

    async fn write_server_request_response(&mut self, id: Value, method: &str) -> Result<()> {
        let response = if method == "ping" {
            Response::result(id, json!({}))
        } else {
            Response::error(id, RpcError::method_not_found(method))
        };
        let value = serde_json::to_value(response)?;
        self.write_value(&value).await
    }
}

#[derive(Deserialize)]
struct IncomingResponse {
    jsonrpc: String,
    id: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<IncomingError>,
}

#[derive(Deserialize)]
struct IncomingError {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct InitializeResponse {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: ClientServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: McpServerInfo,
}

#[derive(Deserialize)]
struct ClientServerCapabilities {
    tools: Option<Map<String, Value>>,
}

#[derive(Clone, Deserialize)]
struct RemoteTool {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Map<String, Value>,
}

#[derive(Deserialize)]
struct ListToolsResponse {
    tools: Vec<RemoteTool>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

async fn discover_tools(session: &mut Session) -> Result<Vec<RemoteTool>> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let result = session.request("tools/list", params).await?;
        let page: ListToolsResponse = decode_result("tools/list", result)?;
        tools.extend(page.tools);
        match page.next_cursor.filter(|cursor| !cursor.is_empty()) {
            Some(next) if seen_cursors.insert(next.clone()) => cursor = Some(next),
            Some(_) => {
                return Err(mcp_message(
                    "external server repeated a tools/list pagination cursor",
                ))
            }
            None => return Ok(tools),
        }
    }
}

struct MountedTool {
    spec: ToolSpec,
    remote_name: String,
}

fn build_mounted_tools(server_name: &str, tools: Vec<RemoteTool>) -> Result<Vec<MountedTool>> {
    let mut remote_names = HashSet::new();
    tools
        .into_iter()
        .map(|tool| {
            if tool.name.is_empty() {
                return Err(mcp_message("external server advertised an empty tool name"));
            }
            if !remote_names.insert(tool.name.clone()) {
                return Err(mcp_message(format!(
                    "external server advertised duplicate tool name {}",
                    tool.name
                )));
            }
            Ok(MountedTool {
                spec: ToolSpec {
                    name: format!("mcp__{server_name}__{}", tool.name),
                    description: tool.description,
                    input_schema: Value::Object(tool.input_schema),
                },
                remote_name: tool.name,
            })
        })
        .collect()
}

fn validate_server_name(server_name: &str) -> Result<()> {
    if server_name.is_empty()
        || !server_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(mcp_message(
            "MCP server name must contain only ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct McpTool {
    spec: ToolSpec,
    remote_name: String,
    server_name: String,
    session: Arc<Mutex<Session>>,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn approval_class(&self) -> ApprovalClass {
        ApprovalClass::Sensitive
    }

    async fn execute(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let result = self
            .session
            .lock()
            .await
            .request(
                "tools/call",
                json!({"name": self.remote_name, "arguments": args}),
            )
            .await
            .map_err(|error| {
                mcp_message(format!(
                    "MCP server {} failed to call {}: {error}",
                    self.server_name, self.remote_name
                ))
            })?;
        let result: CallToolResponse = decode_result("tools/call", result)?;
        let content = result
            .content
            .iter()
            .map(content_for_model)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content,
            data: result.structured_content,
            is_error: result.is_error,
            private_evidence: Vec::new(),
        })
    }
}

#[derive(Deserialize)]
struct CallToolResponse {
    content: Vec<Value>,
    #[serde(rename = "structuredContent")]
    structured_content: Option<Value>,
    #[serde(default, rename = "isError")]
    is_error: bool,
}

fn content_for_model(content: &Value) -> String {
    if content.get("type").and_then(Value::as_str) == Some("text") {
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            return text.to_string();
        }
    }
    serde_json::to_string(content).unwrap_or_else(|_| "<invalid MCP content>".to_string())
}

fn decode_result<T: for<'de> Deserialize<'de>>(method: &str, value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|error| {
        mcp_error(
            format!("invalid {method} result from external server"),
            error,
        )
    })
}

fn mcp_message(message: impl Into<String>) -> AgentError {
    AgentError::msg(format!("MCP client error: {}", message.into()))
}

fn mcp_error(context: impl AsRef<str>, error: impl std::fmt::Display) -> AgentError {
    mcp_message(format!("{}: {error}", context.as_ref()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use openwave_core::{ChatId, ToolCallExecution};
    use tokio::io::{duplex, split, AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    #[tokio::test]
    async fn connects_discovers_pages_and_mounts_sensitive_proxy_tools() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (client_reader, client_writer) = split(client_stream);
        let server = tokio::spawn(fake_server(server_stream));

        let client = McpClient::connect("private_docs", client_reader, client_writer)
            .await
            .unwrap();
        assert_eq!(client.server_name(), "private_docs");
        assert_eq!(
            client.server_info(),
            &McpServerInfo {
                name: "fixture".into(),
                version: "1.2.3".into()
            }
        );
        assert_eq!(
            client
                .tools()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["mcp__private_docs__search", "mcp__private_docs__metadata"]
        );

        let mut registry = ToolRegistry::new();
        client.mount(&mut registry);
        assert_eq!(
            registry.execution("mcp__private_docs__search"),
            Some(ToolCallExecution::Server)
        );
        let tool = registry.get("mcp__private_docs__search").unwrap();
        assert_eq!(tool.approval_class(), ApprovalClass::Sensitive);
        let output = tool
            .execute(
                &ToolCtx::new_legacy_workspace(ChatId::new(), None, PathBuf::from("unused-by-mcp")),
                json!({"query": "waves"}),
            )
            .await
            .unwrap();
        assert_eq!(output.content, "found waves");
        assert_eq!(output.data, Some(json!({"matches": 1})));
        assert!(!output.is_error);

        drop(registry);
        drop(client);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_unsafe_local_server_names_before_writing() {
        let (client_stream, _server_stream) = duplex(1024);
        let (reader, writer) = split(client_stream);
        let error = McpClient::connect("bad server", reader, writer)
            .await
            .err()
            .expect("invalid name must fail");
        assert!(error.to_string().contains("ASCII letters"));
    }

    #[tokio::test]
    async fn initialization_is_bounded_by_the_configured_timeout() {
        let (client_stream, _server_stream) = duplex(1024);
        let (reader, writer) = split(client_stream);
        let error =
            McpClient::connect_with_timeout("hung", reader, writer, Duration::from_millis(10))
                .await
                .err()
                .expect("silent server must time out");
        assert!(error.to_string().contains("timed out handling initialize"));
    }

    #[tokio::test]
    async fn spawn_validates_the_namespace_before_starting_a_process() {
        let command = Command::new("openwave-command-that-does-not-exist");
        let error = McpClient::spawn("bad server", command)
            .await
            .err()
            .expect("invalid name must fail before spawn");
        assert!(error.to_string().contains("ASCII letters"));
        assert!(!error.to_string().contains("could not spawn"));
    }

    async fn fake_server(stream: tokio::io::DuplexStream) {
        let (reader, mut writer) = split(stream);
        let mut lines = BufReader::new(reader).lines();
        let mut page = 0;
        let mut initialized = false;
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = request.get("id").cloned() else {
                assert_eq!(request["method"], "notifications/initialized");
                initialized = true;
                continue;
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => {
                    let mut ping = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": "server-ping",
                        "method": "ping"
                    }))
                    .unwrap();
                    ping.push(b'\n');
                    writer.write_all(&ping).await.unwrap();
                    writer.flush().await.unwrap();
                    let ping_response: Value = serde_json::from_str(
                        &lines.next_line().await.unwrap().expect("ping response"),
                    )
                    .unwrap();
                    assert_eq!(ping_response["id"], "server-ping");
                    assert_eq!(ping_response["result"], json!({}));
                    json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fixture", "version": "1.2.3"}
                    })
                }
                "tools/list" if page == 0 => {
                    assert!(initialized, "tool discovery must follow initialization");
                    assert_eq!(request["params"], json!({}));
                    page += 1;
                    json!({
                        "tools": [{
                            "name": "search",
                            "description": "Search private docs",
                            "inputSchema": {"type": "object"}
                        }],
                        "nextCursor": "page-2"
                    })
                }
                "tools/list" => {
                    assert_eq!(request["params"], json!({"cursor": "page-2"}));
                    json!({
                        "tools": [{
                            "name": "metadata",
                            "description": "Get metadata",
                            "inputSchema": {"type": "object"}
                        }]
                    })
                }
                "tools/call" => {
                    assert_eq!(request["params"]["name"], "search");
                    assert_eq!(request["params"]["arguments"], json!({"query": "waves"}));
                    json!({
                        "content": [{"type": "text", "text": "found waves"}],
                        "structuredContent": {"matches": 1},
                        "isError": false
                    })
                }
                method => panic!("unexpected method: {method}"),
            };
            let mut response = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
            .unwrap();
            response.push(b'\n');
            writer.write_all(&response).await.unwrap();
            writer.flush().await.unwrap();
        }
    }

    #[test]
    fn non_text_content_is_preserved_for_the_model() {
        let block = json!({"type": "resource_link", "uri": "file:///report.pdf"});
        assert_eq!(content_for_model(&block), block.to_string());
    }
}
