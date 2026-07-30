//! MCP client support for mounting an external tool server over stdio or
//! Streamable HTTP.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_core::{
    AgentError, ApprovalClass, Result, Tool, ToolCtx, ToolOutput, ToolRegistry, ToolSpec,
    ToolUiView,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::http::HttpWire;
use crate::protocol::{Response, RpcError, PROTOCOL_VERSION};

const CLIENT_NAME: &str = "openwave";
/// Provider-safe mounted function-name limit shared by OpenAI and Anthropic.
pub const MAX_MOUNTED_TOOL_NAME_BYTES: usize = 64;
/// Leaves useful room for the remote name inside `mcp__{server}__{tool}`.
pub const MAX_SERVER_NAME_BYTES: usize = 32;
const MAX_SERVER_INFO_BYTES: usize = 256;
const MAX_TOOLS_PER_SERVER: usize = 128;
const MAX_TOOL_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_TOOL_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_TOOL_METADATA_BYTES: usize = 512 * 1024;
const MAX_TOOL_LIST_PAGES: usize = 64;
const MAX_CURSOR_BYTES: usize = 4 * 1024;
const MAX_UI_RESOURCE_URI_BYTES: usize = 2 * 1024;
const MAX_RESOURCE_CONTENT_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_JSON_RPC_FRAME_BYTES: usize = 2 * 1024 * 1024;

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
#[derive(Clone)]
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
            Session::stream(
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
    /// directory on `command`; this method owns stdin/stdout and discards stderr
    /// so an untrusted child cannot copy a selected credential into host logs.
    /// The child is terminated if the client session is dropped.
    pub async fn spawn(server_name: impl Into<String>, command: Command) -> Result<Self> {
        Self::spawn_with_timeout(server_name, command, DEFAULT_REQUEST_TIMEOUT).await
    }

    /// Spawn a server with an explicit per-request timeout.
    pub async fn spawn_with_timeout(
        server_name: impl Into<String>,
        command: Command,
        request_timeout: Duration,
    ) -> Result<Self> {
        Self::spawn_with_timeouts(server_name, command, request_timeout, request_timeout).await
    }

    /// Spawn with a separately bounded initialization timeout. The established
    /// session switches to `request_timeout` only after initialize and initial
    /// tool discovery succeed.
    pub async fn spawn_with_timeouts(
        server_name: impl Into<String>,
        mut command: Command,
        initialization_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self> {
        let server_name = server_name.into();
        validate_server_name(&server_name)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
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

        let client = Self::connect_session(
            server_name,
            Session::stream(
                Box::new(BufReader::new(stdout)),
                Box::new(stdin),
                Some(child),
                initialization_timeout,
            ),
        )
        .await?;
        client.session.lock().await.request_timeout = request_timeout;
        Ok(client)
    }

    /// Connect to an external Streamable HTTP MCP server.
    ///
    /// `bearer_token` is attached as an `Authorization: Bearer` header on every
    /// request and never appears in errors, logs, or the mounted tool surface.
    pub async fn connect_http(
        server_name: impl Into<String>,
        url: &str,
        bearer_token: Option<&str>,
    ) -> Result<Self> {
        Self::connect_http_with_timeouts(
            server_name,
            url,
            bearer_token,
            DEFAULT_REQUEST_TIMEOUT,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    }

    /// Connect over HTTP with a separately bounded initialization timeout, in
    /// the same shape as [`Self::spawn_with_timeouts`].
    pub async fn connect_http_with_timeouts(
        server_name: impl Into<String>,
        url: &str,
        bearer_token: Option<&str>,
        initialization_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self> {
        let server_name = server_name.into();
        validate_server_name(&server_name)?;
        let wire = HttpWire::new(url, bearer_token)?;
        let client =
            Self::connect_session(server_name, Session::http(wire, initialization_timeout)).await?;
        client.session.lock().await.request_timeout = request_timeout;
        Ok(client)
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
        if initialized.server_info.name.len() > MAX_SERVER_INFO_BYTES
            || initialized.server_info.version.len() > MAX_SERVER_INFO_BYTES
        {
            return Err(mcp_message(
                "external server identity exceeds the metadata limit",
            ));
        }
        session
            .notify("notifications/initialized", json!({}))
            .await?;

        let descriptors = discover_tools(&server_name, &mut session).await?;
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

    /// The validated `ui://` view declared for one mounted tool, if any.
    pub fn ui_resource_uri(&self, mounted_tool_name: &str) -> Option<&str> {
        self.tools
            .iter()
            .find(|tool| tool.spec.name == mounted_tool_name)
            .and_then(|tool| tool.ui_resource_uri.as_deref())
    }

    /// Read one resource the server declared through a mounted tool's
    /// `_meta` UI declaration. Undeclared URIs are refused before any
    /// request is sent, so a caller cannot be steered into fetching
    /// arbitrary server-chosen content.
    pub async fn read_resource(&self, uri: &str) -> Result<ResourceContent> {
        if !self
            .tools
            .iter()
            .any(|tool| tool.ui_resource_uri.as_deref() == Some(uri))
        {
            return Err(mcp_message(
                "resource URI was not declared by any mounted tool",
            ));
        }
        let result = self
            .session
            .lock()
            .await
            .request("resources/read", json!({"uri": uri}))
            .await?;
        let response: ReadResourceResponse = decode_result("resources/read", result)?;
        let content = response
            .contents
            .into_iter()
            .find(|content| content.uri == uri)
            .ok_or_else(|| {
                mcp_message("external server resources/read result did not contain the URI")
            })?;
        let (text_bytes, blob_bytes) = (
            content.text.as_ref().map_or(0, String::len),
            content.blob.as_ref().map_or(0, String::len),
        );
        if content.text.is_some() == content.blob.is_some() {
            return Err(mcp_message(
                "external server resource must contain exactly one of text or blob",
            ));
        }
        if text_bytes.max(blob_bytes) > MAX_RESOURCE_CONTENT_BYTES {
            return Err(mcp_message("external server resource exceeds the limit"));
        }
        Ok(ResourceContent {
            uri: content.uri,
            mime_type: content.mime_type,
            text: content.text,
            blob: content.blob,
        })
    }

    /// Check that an idle session still responds and report whether the server
    /// asked the client to refresh its tool list. A session owned by a live tool
    /// call returns [`McpProbe::Busy`] immediately.
    ///
    /// MCP stdio is serialized per server, so the ping also drains notifications
    /// that arrived while no tool call was active. Callers should establish a
    /// fresh connection before publishing a changed tool surface; existing
    /// registry snapshots may continue using this session until their turn ends.
    pub async fn probe(&self) -> Result<McpProbe> {
        let Ok(mut session) = self.session.try_lock() else {
            return Ok(McpProbe::Busy);
        };
        session.request("ping", json!({})).await?;
        Ok(McpProbe::Ready {
            tools_list_changed: session.take_tools_list_changed(),
        })
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
                ui_resource_uri: tool.ui_resource_uri.clone(),
                session: Arc::clone(&self.session),
            }));
        }
    }
}

struct Session {
    wire: Wire,
    next_id: u64,
    request_timeout: Duration,
    tools_list_changed: bool,
}

enum Wire {
    Stream(StreamWire),
    Http(HttpWire),
}

impl Session {
    fn stream(
        reader: BoxReader,
        writer: BoxWriter,
        child: Option<Child>,
        request_timeout: Duration,
    ) -> Self {
        Self {
            wire: Wire::Stream(StreamWire {
                reader,
                writer,
                _child: child,
            }),
            next_id: 1,
            request_timeout,
            tools_list_changed: false,
        }
    }

    fn http(wire: HttpWire, request_timeout: Duration) -> Self {
        Self {
            wire: Wire::Http(wire),
            next_id: 1,
            request_timeout,
            tools_list_changed: false,
        }
    }

    fn take_tools_list_changed(&mut self) -> bool {
        std::mem::take(&mut self.tools_list_changed)
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
        let request_timeout = self.request_timeout;
        let Self {
            wire,
            tools_list_changed,
            ..
        } = self;
        let outcome = match wire {
            Wire::Stream(stream) => {
                timeout(request_timeout, stream.write_value(&request))
                    .await
                    .map_err(|_| mcp_message(format!("timed out writing {method} request")))??;
                timeout(
                    request_timeout,
                    stream.read_response(id, tools_list_changed),
                )
                .await
            }
            Wire::Http(http) => {
                timeout(
                    request_timeout,
                    http.request(id, &request, tools_list_changed),
                )
                .await
            }
        };
        match outcome {
            Ok(result) => result,
            Err(_) => {
                // Best-effort courtesy: tell the server the request is dead so
                // it can stop working on it. Failure to deliver changes nothing.
                let cancelled = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": "OpenWave MCP request timed out"}
                });
                let _ = match &mut self.wire {
                    Wire::Stream(stream) => {
                        timeout(request_timeout, stream.write_value(&cancelled)).await
                    }
                    Wire::Http(http) => timeout(request_timeout, http.notify(&cancelled)).await,
                };
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
        match &mut self.wire {
            Wire::Stream(stream) => {
                timeout(self.request_timeout, stream.write_value(&notification))
                    .await
                    .map_err(|_| mcp_message(format!("timed out writing {method} notification")))?
            }
            Wire::Http(http) => timeout(self.request_timeout, http.notify(&notification))
                .await
                .map_err(|_| mcp_message(format!("timed out writing {method} notification")))?,
        }
    }
}

struct StreamWire {
    reader: BoxReader,
    writer: BoxWriter,
    // Keeping the child owns its lifecycle; `kill_on_drop(true)` handles both a
    // normal registry teardown and a failed initialization.
    _child: Option<Child>,
}

impl StreamWire {
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

    async fn read_response(
        &mut self,
        expected_id: u64,
        tools_list_changed: &mut bool,
    ) -> Result<Value> {
        loop {
            let Some(line) = read_bounded_line(&mut self.reader).await? else {
                return Err(mcp_message("external server closed stdout before replying"));
            };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let value: Value = serde_json::from_slice(&line)
                .map_err(|error| mcp_error("external server returned malformed JSON-RPC", error))?;
            match classify_incoming(value, expected_id, tools_list_changed)? {
                Incoming::FinalResult(result) => return Ok(result),
                Incoming::ServerRequest { id, method } => {
                    let response = server_request_response(id, &method)?;
                    self.write_value(&response).await?;
                }
                Incoming::Ignored => {}
            }
        }
    }
}

/// One classified incoming JSON-RPC message, transport-independent.
pub(crate) enum Incoming {
    /// The response matching the in-flight request id.
    FinalResult(Value),
    /// A server-initiated request that needs an answer on the write path.
    ServerRequest { id: Value, method: String },
    /// A notification or a stale response to an already-abandoned request.
    Ignored,
}

/// Classify one message the way the stdio loop always has: notifications latch
/// `tools/list_changed`, server requests are surfaced for a fail-closed answer,
/// stale numeric ids are skipped, and anything else must be the response.
pub(crate) fn classify_incoming(
    value: Value,
    expected_id: u64,
    tools_list_changed: &mut bool,
) -> Result<Incoming> {
    // Server notifications are asynchronous and need no response. Ping is part
    // of the MCP base protocol in either direction. The client advertises no
    // other request-producing capabilities, so fail closed for any other
    // server request.
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        if method == "notifications/tools/list_changed" && value.get("id").is_none() {
            *tools_list_changed = true;
        }
        if let Some(id) = value.get("id") {
            return Ok(Incoming::ServerRequest {
                id: id.clone(),
                method: method.to_string(),
            });
        }
        return Ok(Incoming::Ignored);
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
        // A server may complete an already-cancelled timed-out request after
        // the client has moved on. Calls are serialized, so only an older
        // numeric id can safely be recognized as stale.
        return Ok(Incoming::Ignored);
    }
    if response.id != json!(expected_id) {
        return Err(mcp_message(format!(
            "external server replied with id {} while waiting for {expected_id}",
            response.id
        )));
    }
    match (response.result, response.error) {
        (Some(result), None) => Ok(Incoming::FinalResult(result)),
        (None, Some(error)) => Err(mcp_message(format!(
            "external server returned JSON-RPC error {}: {}",
            error.code, error.message
        ))),
        _ => Err(mcp_message(
            "external server response must contain exactly one of result or error",
        )),
    }
}

/// The fail-closed answer to a server-initiated request.
pub(crate) fn server_request_response(id: Value, method: &str) -> Result<Value> {
    let response = if method == "ping" {
        Response::result(id, json!({}))
    } else {
        Response::error(id, RpcError::method_not_found(method))
    };
    Ok(serde_json::to_value(response)?)
}

async fn read_bounded_line(reader: &mut BoxReader) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|error| mcp_error("could not read from external server", error))?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(consumed) > MAX_JSON_RPC_FRAME_BYTES {
            return Err(mcp_message(
                "external server JSON-RPC frame exceeds the limit",
            ));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}

/// One resource document returned by `resources/read`.
///
/// Exactly one of `text` or `blob` is present; `blob` is the base64 encoding
/// the MCP resource contract uses for binary data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceContent {
    /// The URI the document was read from.
    pub uri: String,
    /// The server-reported MIME type, when given.
    pub mime_type: Option<String>,
    /// UTF-8 document body.
    pub text: Option<String>,
    /// Base64 document body.
    pub blob: Option<String>,
}

#[derive(Deserialize)]
struct ReadResourceResponse {
    contents: Vec<RawResourceContent>,
}

#[derive(Deserialize)]
struct RawResourceContent {
    uri: String,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    blob: Option<String>,
}

/// Result of one non-mutating MCP health probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpProbe {
    /// A tool call already owns the serialized stdio session. Health supervision
    /// must skip this cycle rather than time out legitimate work.
    Busy,
    /// The ping completed on an otherwise idle session.
    Ready {
        /// The server emitted `notifications/tools/list_changed`.
        tools_list_changed: bool,
    },
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
    /// Open extension metadata. Only the MCP Apps UI declaration is read;
    /// everything else is bounded and dropped.
    #[serde(default, rename = "_meta")]
    meta: Option<Map<String, Value>>,
}

impl RemoteTool {
    /// The declared MCP Apps view for this tool, under either published
    /// spelling: nested `_meta.ui.resourceUri` wins over the legacy flat
    /// `_meta["ui/resourceUri"]` key some hosts still require.
    fn declared_ui_resource(&self) -> Option<&str> {
        let meta = self.meta.as_ref()?;
        meta.get("ui")
            .and_then(|ui| ui.get("resourceUri"))
            .or_else(|| meta.get("ui/resourceUri"))
            .and_then(Value::as_str)
    }
}

#[derive(Deserialize)]
struct ListToolsResponse {
    tools: Vec<RemoteTool>,
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

async fn discover_tools(server_name: &str, session: &mut Session) -> Result<Vec<RemoteTool>> {
    let mut tools = Vec::new();
    let mut metadata_bytes = 0_usize;
    let mut pages = 0_usize;
    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    loop {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
        let result = session.request("tools/list", params).await?;
        let page: ListToolsResponse = decode_result("tools/list", result)?;
        pages += 1;
        if pages > MAX_TOOL_LIST_PAGES {
            return Err(mcp_message("external server tool list has too many pages"));
        }
        if tools.len().saturating_add(page.tools.len()) > MAX_TOOLS_PER_SERVER {
            return Err(mcp_message("external server advertises too many tools"));
        }
        for tool in page.tools {
            metadata_bytes = metadata_bytes
                .checked_add(validate_remote_tool(server_name, &tool)?)
                .ok_or_else(|| mcp_message("external server tool metadata exceeds the limit"))?;
            if metadata_bytes > MAX_TOOL_METADATA_BYTES {
                return Err(mcp_message(
                    "external server tool metadata exceeds the limit",
                ));
            }
            tools.push(tool);
        }
        match page.next_cursor.filter(|cursor| !cursor.is_empty()) {
            Some(next) if next.len() <= MAX_CURSOR_BYTES && seen_cursors.insert(next.clone()) => {
                cursor = Some(next)
            }
            Some(next) if next.len() > MAX_CURSOR_BYTES => {
                return Err(mcp_message(
                    "external server tool-list cursor exceeds the limit",
                ))
            }
            Some(_) => {
                return Err(mcp_message(
                    "external server repeated a tools/list pagination cursor",
                ))
            }
            None => return Ok(tools),
        }
    }
}

#[derive(Clone)]
struct MountedTool {
    spec: ToolSpec,
    remote_name: String,
    /// Validated `ui://` view declared through `_meta`, if any.
    ui_resource_uri: Option<String>,
}

fn build_mounted_tools(server_name: &str, tools: Vec<RemoteTool>) -> Result<Vec<MountedTool>> {
    if tools.len() > MAX_TOOLS_PER_SERVER {
        return Err(mcp_message("external server advertises too many tools"));
    }
    let mut remote_names = HashSet::new();
    let mut metadata_bytes = 0_usize;
    tools
        .into_iter()
        .map(|tool| {
            metadata_bytes = metadata_bytes
                .checked_add(validate_remote_tool(server_name, &tool)?)
                .ok_or_else(|| mcp_message("external server tool metadata exceeds the limit"))?;
            if metadata_bytes > MAX_TOOL_METADATA_BYTES {
                return Err(mcp_message(
                    "external server tool metadata exceeds the limit",
                ));
            }
            if !remote_names.insert(tool.name.clone()) {
                return Err(mcp_message(
                    "external server advertised duplicate tool names",
                ));
            }
            let ui_resource_uri = tool.declared_ui_resource().map(str::to_string);
            Ok(MountedTool {
                spec: ToolSpec {
                    name: format!("mcp__{server_name}__{}", tool.name),
                    description: tool.description,
                    input_schema: Value::Object(tool.input_schema),
                },
                remote_name: tool.name,
                ui_resource_uri,
            })
        })
        .collect()
}

fn validate_remote_tool(server_name: &str, tool: &RemoteTool) -> Result<usize> {
    let mounted_name = format!("mcp__{server_name}__{}", tool.name);
    if tool.name.is_empty()
        || mounted_name.len() > MAX_MOUNTED_TOOL_NAME_BYTES
        || !mounted_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(mcp_message(
            "external server advertised a provider-incompatible tool name",
        ));
    }
    if tool.description.len() > MAX_TOOL_DESCRIPTION_BYTES {
        return Err(mcp_message(
            "external server tool description exceeds the limit",
        ));
    }
    let schema_bytes = serde_json::to_vec(&tool.input_schema)?.len();
    if schema_bytes > MAX_TOOL_SCHEMA_BYTES {
        return Err(mcp_message("external server tool schema exceeds the limit"));
    }
    let meta_bytes = match &tool.meta {
        Some(meta) => serde_json::to_vec(meta)?.len(),
        None => 0,
    };
    // A declared view is a contract, so a malformed declaration fails the
    // connection instead of silently dropping the view. Unrelated `_meta`
    // stays ignored.
    if let Some(meta) = &tool.meta {
        // Every present spelling is held to the contract, not only the one
        // that wins precedence: a declaration is either wholly valid or the
        // connection fails.
        for declared in [
            meta.get("ui").and_then(|ui| ui.get("resourceUri")),
            meta.get("ui/resourceUri"),
        ]
        .into_iter()
        .flatten()
        {
            let Some(uri) = declared.as_str() else {
                return Err(mcp_message(
                    "external server declared a non-string UI resource",
                ));
            };
            if uri.len() > MAX_UI_RESOURCE_URI_BYTES
                || !uri.starts_with("ui://")
                || uri.len() == "ui://".len()
                || uri.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(mcp_message(
                    "external server declared an invalid ui:// resource URI",
                ));
            }
        }
    }
    mounted_name
        .len()
        .checked_add(tool.description.len())
        .and_then(|size| size.checked_add(schema_bytes))
        .and_then(|size| size.checked_add(meta_bytes))
        .ok_or_else(|| mcp_message("external server tool metadata exceeds the limit"))
}

fn validate_server_name(server_name: &str) -> Result<()> {
    if server_name.is_empty()
        || server_name.len() > MAX_SERVER_NAME_BYTES
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
    /// Validated `ui://` view from discovery, stamped onto each output so the
    /// host can surface the declared MCP Apps view for this tool's results.
    ui_resource_uri: Option<String>,
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
            // The external server reports that it failed, not why in terms this
            // side can classify, so it is the tool's own failure.
            error_category: result
                .is_error
                .then_some(openwave_core::ToolErrorCategory::ToolFailed),
            ui_view: self.ui_resource_uri.as_ref().map(|uri| {
                Box::new(ToolUiView {
                    server: self.server_name.clone(),
                    resource_uri: uri.clone(),
                })
            }),
            images: Vec::new(),
            image_data: openwave_core::ImageAttachments::new(),
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

pub(crate) fn mcp_message(message: impl Into<String>) -> AgentError {
    AgentError::msg(format!("MCP client error: {}", message.into()))
}

pub(crate) fn mcp_error(context: impl AsRef<str>, error: impl std::fmt::Display) -> AgentError {
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
        // The declared view rides the output so the host can surface it.
        assert_eq!(
            output.ui_view,
            Some(Box::new(ToolUiView {
                server: "private_docs".into(),
                resource_uri: "ui://fixture/app.html".into(),
            }))
        );

        assert_eq!(
            client.ui_resource_uri("mcp__private_docs__search"),
            Some("ui://fixture/app.html")
        );
        assert_eq!(client.ui_resource_uri("mcp__private_docs__metadata"), None);
        let resource = client.read_resource("ui://fixture/app.html").await.unwrap();
        assert_eq!(resource.uri, "ui://fixture/app.html");
        assert_eq!(
            resource.mime_type.as_deref(),
            Some("text/html;profile=mcp-app")
        );
        assert_eq!(resource.text.as_deref(), Some("<html>view</html>"));
        assert!(resource.blob.is_none());
        // An undeclared URI is refused before any request reaches the server;
        // the fixture would panic on an unexpected resources/read.
        let undeclared = client
            .read_resource("ui://somewhere/else.html")
            .await
            .expect_err("undeclared URI must fail");
        assert!(undeclared.to_string().contains("not declared"));

        let probe = client.probe().await.unwrap();
        assert_eq!(
            probe,
            McpProbe::Ready {
                tools_list_changed: true
            }
        );

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

    fn remote_tool(name: impl Into<String>) -> RemoteTool {
        RemoteTool {
            name: name.into(),
            description: "fixture".to_string(),
            input_schema: json!({"type": "object"}).as_object().unwrap().clone(),
            meta: None,
        }
    }

    fn remote_tool_with_meta(name: impl Into<String>, meta: Value) -> RemoteTool {
        let mut tool = remote_tool(name);
        tool.meta = Some(meta.as_object().unwrap().clone());
        tool
    }

    #[test]
    fn ui_declarations_are_validated_and_bounded() {
        // Nested spelling, flat spelling, and nested-wins precedence.
        let nested = build_mounted_tools(
            "docs",
            vec![remote_tool_with_meta(
                "viewer",
                json!({"ui": {"resourceUri": "ui://docs/app.html"}}),
            )],
        )
        .unwrap();
        assert_eq!(
            nested[0].ui_resource_uri.as_deref(),
            Some("ui://docs/app.html")
        );

        let flat = build_mounted_tools(
            "docs",
            vec![remote_tool_with_meta(
                "viewer",
                json!({"ui/resourceUri": "ui://docs/flat.html"}),
            )],
        )
        .unwrap();
        assert_eq!(
            flat[0].ui_resource_uri.as_deref(),
            Some("ui://docs/flat.html")
        );

        let both = build_mounted_tools(
            "docs",
            vec![remote_tool_with_meta(
                "viewer",
                json!({
                    "ui": {"resourceUri": "ui://docs/nested.html"},
                    "ui/resourceUri": "ui://docs/flat.html"
                }),
            )],
        )
        .unwrap();
        assert_eq!(
            both[0].ui_resource_uri.as_deref(),
            Some("ui://docs/nested.html")
        );

        // Unrelated extension metadata stays ignored.
        let unrelated = build_mounted_tools(
            "docs",
            vec![remote_tool_with_meta("plain", json!({"vendor": {"x": 1}}))],
        )
        .unwrap();
        assert!(unrelated[0].ui_resource_uri.is_none());

        // A declared view is a contract: wrong scheme, wrong type, and
        // oversized URIs fail the connection.
        for (meta, fragment) in [
            (
                json!({"ui": {"resourceUri": "https://evil.example/app"}}),
                "invalid ui://",
            ),
            (json!({"ui/resourceUri": 7}), "non-string UI resource"),
            // A malformed losing spelling still fails the connection.
            (
                json!({"ui": {"resourceUri": "ui://docs/ok.html"}, "ui/resourceUri": 7}),
                "non-string UI resource",
            ),
            (json!({"ui": {"resourceUri": "ui://"}}), "invalid ui://"),
            (
                json!({"ui": {"resourceUri": format!("ui://{}", "x".repeat(MAX_UI_RESOURCE_URI_BYTES))}}),
                "invalid ui://",
            ),
        ] {
            let error = build_mounted_tools("docs", vec![remote_tool_with_meta("viewer", meta)])
                .err()
                .expect("invalid declaration must fail");
            assert!(error.to_string().contains(fragment), "{error}");
        }
    }

    #[test]
    fn meta_counts_against_the_aggregate_metadata_budget() {
        let error = build_mounted_tools(
            "docs",
            vec![remote_tool_with_meta(
                "viewer",
                json!({"vendor": "x".repeat(MAX_TOOL_METADATA_BYTES)}),
            )],
        )
        .err()
        .expect("oversized meta must fail");
        assert!(error.to_string().contains("metadata exceeds"));
    }

    #[test]
    fn mounted_tool_names_use_the_shared_provider_safe_contract() {
        for name in ["", "bad tool", "bad/tool", "bad.tool"] {
            let error = build_mounted_tools("docs", vec![remote_tool(name)])
                .err()
                .expect("unsafe remote tool name must fail");
            assert!(error.to_string().contains("provider-incompatible"));
            if !name.is_empty() {
                assert!(!error.to_string().contains(name));
            }
        }

        let error = build_mounted_tools("docs", vec![remote_tool("x".repeat(64))])
            .err()
            .expect("overlong mounted tool name must fail");
        assert!(error.to_string().contains("provider-incompatible"));
    }

    #[test]
    fn tool_discovery_metadata_is_bounded_per_tool_and_in_aggregate() {
        let too_many = (0..=MAX_TOOLS_PER_SERVER)
            .map(|index| remote_tool(format!("tool_{index}")))
            .collect();
        assert!(build_mounted_tools("docs", too_many)
            .err()
            .unwrap()
            .to_string()
            .contains("too many tools"));

        let mut description = remote_tool("description");
        description.description = "x".repeat(MAX_TOOL_DESCRIPTION_BYTES + 1);
        assert!(build_mounted_tools("docs", vec![description])
            .err()
            .unwrap()
            .to_string()
            .contains("description exceeds"));

        let mut schema = remote_tool("schema");
        schema.input_schema = json!({
            "type": "object",
            "description": "x".repeat(MAX_TOOL_SCHEMA_BYTES)
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(build_mounted_tools("docs", vec![schema])
            .err()
            .unwrap()
            .to_string()
            .contains("schema exceeds"));

        let aggregate = (0..MAX_TOOLS_PER_SERVER)
            .map(|index| {
                let mut tool = remote_tool(format!("tool_{index}"));
                tool.description = "x".repeat(MAX_TOOL_METADATA_BYTES / MAX_TOOLS_PER_SERVER);
                tool
            })
            .collect();
        assert!(build_mounted_tools("docs", aggregate)
            .err()
            .unwrap()
            .to_string()
            .contains("metadata exceeds"));
    }

    #[tokio::test]
    async fn rejects_an_oversized_json_rpc_frame_before_decoding_it() {
        let (client_stream, server_stream) = duplex(MAX_JSON_RPC_FRAME_BYTES + 1024);
        let (client_reader, client_writer) = split(client_stream);
        let server = tokio::spawn(async move {
            let (reader, mut writer) = split(server_stream);
            let mut lines = BufReader::new(reader).lines();
            let _request = lines.next_line().await.unwrap().unwrap();
            writer
                .write_all(&vec![b'x'; MAX_JSON_RPC_FRAME_BYTES + 1])
                .await
                .unwrap();
            writer.write_all(b"\n").await.unwrap();
        });
        let error = McpClient::connect_with_timeout(
            "docs",
            client_reader,
            client_writer,
            Duration::from_secs(5),
        )
        .await
        .err()
        .expect("oversized frame must fail");
        assert!(error.to_string().contains("frame exceeds"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn health_probe_skips_a_session_owned_by_a_long_running_tool_call() {
        let (client_stream, server_stream) = duplex(16 * 1024);
        let (client_reader, client_writer) = split(client_stream);
        let call_started = Arc::new(tokio::sync::Notify::new());
        let release_call = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(blocking_tool_server(
            server_stream,
            call_started.clone(),
            release_call.clone(),
        ));
        let client = McpClient::connect("busy", client_reader, client_writer)
            .await
            .unwrap();
        let mut registry = ToolRegistry::new();
        client.mount(&mut registry);
        let call = tokio::spawn(async move {
            registry
                .get("mcp__busy__wait")
                .unwrap()
                .execute(
                    &ToolCtx::new_legacy_workspace(
                        ChatId::new(),
                        None,
                        PathBuf::from("unused-by-mcp"),
                    ),
                    json!({}),
                )
                .await
        });
        call_started.notified().await;

        assert_eq!(
            tokio::time::timeout(Duration::from_millis(50), client.probe())
                .await
                .expect("busy detection must not wait for the tool request")
                .unwrap(),
            McpProbe::Busy
        );

        release_call.notify_one();
        assert_eq!(call.await.unwrap().unwrap().content, "finished");
        assert_eq!(
            client.probe().await.unwrap(),
            McpProbe::Ready {
                tools_list_changed: false
            }
        );
        drop(client);
        server.await.unwrap();
    }

    async fn blocking_tool_server(
        stream: tokio::io::DuplexStream,
        call_started: Arc<tokio::sync::Notify>,
        release_call: Arc<tokio::sync::Notify>,
    ) {
        let (reader, mut writer) = split(stream);
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = request.get("id").cloned() else {
                assert_eq!(request["method"], "notifications/initialized");
                continue;
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "blocking-fixture", "version": "1"}
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "wait",
                        "description": "Wait for the deterministic test",
                        "inputSchema": {"type": "object"}
                    }]
                }),
                "tools/call" => {
                    call_started.notify_one();
                    release_call.notified().await;
                    json!({
                        "content": [{"type": "text", "text": "finished"}],
                        "isError": false
                    })
                }
                "ping" => json!({}),
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
                            "inputSchema": {"type": "object"},
                            "_meta": {
                                "ui": {"resourceUri": "ui://fixture/app.html"},
                                "ui/resourceUri": "ui://fixture/app.html"
                            }
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
                "resources/read" => {
                    assert_eq!(request["params"], json!({"uri": "ui://fixture/app.html"}));
                    json!({
                        "contents": [{
                            "uri": "ui://fixture/app.html",
                            "mimeType": "text/html;profile=mcp-app",
                            "text": "<html>view</html>"
                        }]
                    })
                }
                "ping" => {
                    let mut notification = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed"
                    }))
                    .unwrap();
                    notification.push(b'\n');
                    writer.write_all(&notification).await.unwrap();
                    writer.flush().await.unwrap();
                    json!({})
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

    #[tokio::test]
    async fn resource_reads_are_bounded_and_shape_checked() {
        const URI: &str = "ui://fixture/app.html";
        let cases = [
            (
                json!([{"uri": URI, "text": "x", "blob": "eA=="}]),
                "exactly one of text or blob",
            ),
            (
                json!([{"uri": URI, "text": "x".repeat(MAX_RESOURCE_CONTENT_BYTES + 1)}]),
                "resource exceeds the limit",
            ),
            (
                json!([{"uri": "ui://fixture/other.html", "text": "x"}]),
                "did not contain the URI",
            ),
            (json!([]), "did not contain the URI"),
        ];
        for (contents, fragment) in cases {
            let (client_stream, server_stream) = duplex(4 * 1024 * 1024);
            let (reader, writer) = split(client_stream);
            let server = tokio::spawn(resource_server(server_stream, contents));
            let client = McpClient::connect("docs", reader, writer).await.unwrap();
            let error = client
                .read_resource(URI)
                .await
                .expect_err("malformed resource must fail");
            assert!(error.to_string().contains(fragment), "{error}");
            drop(client);
            server.await.unwrap();
        }
    }

    async fn resource_server(stream: tokio::io::DuplexStream, contents: Value) {
        let (reader, mut writer) = split(stream);
        let mut lines = BufReader::new(reader).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            let Some(id) = request.get("id").cloned() else {
                continue;
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "resource-fixture", "version": "1"}
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "viewer",
                        "description": "Tool with a declared view",
                        "inputSchema": {"type": "object"},
                        "_meta": {"ui": {"resourceUri": "ui://fixture/app.html"}}
                    }]
                }),
                "resources/read" => json!({"contents": contents}),
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

    mod http_transport {
        use std::net::SocketAddr;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::{IntoResponse, Response as AxumResponse};
        use axum::routing::post;
        use axum::Router;

        use super::*;

        const TEST_TOKEN: &str = "test-token-abc123";
        const TEST_SESSION_ID: &str = "session-xyz";

        #[derive(Default)]
        struct ServerState {
            requests: AtomicUsize,
            sse: bool,
        }

        async fn handler(
            State(state): State<Arc<ServerState>>,
            headers: HeaderMap,
            body: String,
        ) -> AxumResponse {
            let authorization = headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            if authorization != format!("Bearer {TEST_TOKEN}") {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            assert!(headers
                .get("accept")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|accept| accept.contains("application/json")));
            assert_eq!(
                headers
                    .get("mcp-protocol-version")
                    .and_then(|value| value.to_str().ok()),
                Some(PROTOCOL_VERSION)
            );
            let request: Value = serde_json::from_str(&body).unwrap();
            let sequence = state.requests.fetch_add(1, Ordering::SeqCst);
            if sequence > 0 {
                // Every request after initialize must echo the issued session.
                assert_eq!(
                    headers
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok()),
                    Some(TEST_SESSION_ID)
                );
            }
            let Some(id) = request.get("id").cloned() else {
                return StatusCode::ACCEPTED.into_response();
            };
            let result = match request["method"].as_str().unwrap() {
                "initialize" => json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "http-fixture", "version": "9"}
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "lookup",
                        "description": "Look something up",
                        "inputSchema": {"type": "object"}
                    }]
                }),
                "tools/call" => json!({
                    "content": [{"type": "text", "text": "http result"}],
                    "structuredContent": {"via": "http"},
                    "isError": false
                }),
                "ping" => json!({}),
                method => panic!("unexpected method: {method}"),
            };
            let response = json!({"jsonrpc": "2.0", "id": id, "result": result});
            if state.sse {
                let notification =
                    json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
                let stream_body =
                    format!("event: message\ndata: {notification}\n\ndata: {response}\n\n");
                (
                    [
                        ("content-type", "text/event-stream"),
                        ("mcp-session-id", TEST_SESSION_ID),
                    ],
                    stream_body,
                )
                    .into_response()
            } else {
                (
                    [
                        ("content-type", "application/json"),
                        ("mcp-session-id", TEST_SESSION_ID),
                    ],
                    response.to_string(),
                )
                    .into_response()
            }
        }

        async fn serve(state: Arc<ServerState>) -> SocketAddr {
            let app = Router::new().route("/mcp", post(handler)).with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            address
        }

        #[tokio::test]
        async fn connects_and_calls_tools_over_json_responses() {
            let address = serve(Arc::new(ServerState::default())).await;
            let client = McpClient::connect_http(
                "gateway",
                &format!("http://{address}/mcp"),
                Some(TEST_TOKEN),
            )
            .await
            .unwrap();
            assert_eq!(client.server_info().name, "http-fixture");

            let mut registry = ToolRegistry::new();
            client.mount(&mut registry);
            let output = registry
                .get("mcp__gateway__lookup")
                .unwrap()
                .execute(
                    &ToolCtx::new_legacy_workspace(
                        ChatId::new(),
                        None,
                        PathBuf::from("unused-by-mcp"),
                    ),
                    json!({}),
                )
                .await
                .unwrap();
            assert_eq!(output.content, "http result");
            assert_eq!(output.data, Some(json!({"via": "http"})));
            assert_eq!(
                client.probe().await.unwrap(),
                McpProbe::Ready {
                    tools_list_changed: false
                }
            );
        }

        #[tokio::test]
        async fn reads_event_stream_responses_and_latches_notifications() {
            let address = serve(Arc::new(ServerState {
                requests: AtomicUsize::new(0),
                sse: true,
            }))
            .await;
            let client = McpClient::connect_http(
                "gateway",
                &format!("http://{address}/mcp"),
                Some(TEST_TOKEN),
            )
            .await
            .unwrap();
            // Every SSE response carried a tools/list_changed notification
            // ahead of the result; the probe must surface the latch.
            assert_eq!(
                client.probe().await.unwrap(),
                McpProbe::Ready {
                    tools_list_changed: true
                }
            );
        }

        #[tokio::test]
        async fn rejected_credentials_fail_without_echoing_the_token() {
            let address = serve(Arc::new(ServerState::default())).await;
            let error = McpClient::connect_http(
                "gateway",
                &format!("http://{address}/mcp"),
                Some("wrong-token"),
            )
            .await
            .err()
            .expect("wrong token must fail");
            let message = error.to_string();
            assert!(message.contains("rejected the configured credentials"));
            assert!(!message.contains("wrong-token"));
        }

        #[tokio::test]
        async fn redirects_are_not_followed_with_a_credential_in_hand() {
            // A redirecting endpoint must surface as an error, not as a
            // silently relocated (and re-credentialed) request.
            let app = Router::new().route(
                "/mcp",
                post(|| async {
                    (
                        StatusCode::FOUND,
                        [("location", "http://127.0.0.1:1/elsewhere")],
                    )
                        .into_response()
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let error = McpClient::connect_http(
                "gateway",
                &format!("http://{address}/mcp"),
                Some(TEST_TOKEN),
            )
            .await
            .err()
            .expect("redirect must fail");
            assert!(error.to_string().contains("HTTP status 302"), "{error}");
        }

        #[tokio::test]
        async fn an_unreachable_server_fails_with_a_bounded_error() {
            let error = McpClient::connect_http_with_timeouts(
                "gateway",
                "http://127.0.0.1:1/mcp",
                None,
                Duration::from_secs(2),
                Duration::from_secs(2),
            )
            .await
            .err()
            .expect("unreachable server must fail");
            assert!(error
                .to_string()
                .contains("could not reach external server"));
        }

        #[tokio::test]
        async fn http_connect_validates_the_namespace_and_url_first() {
            let error = McpClient::connect_http("bad server", "http://127.0.0.1/mcp", None)
                .await
                .err()
                .expect("invalid name must fail");
            assert!(error.to_string().contains("ASCII letters"));

            let error = McpClient::connect_http("gateway", "ftp://host/mcp", None)
                .await
                .err()
                .expect("invalid scheme must fail");
            assert!(error.to_string().contains("http or https"));
        }
    }
}
