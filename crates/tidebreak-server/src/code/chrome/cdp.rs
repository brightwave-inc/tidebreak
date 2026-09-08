//! Minimal direct CDP transport and message multiplexer.
//!
//! This module owns only DevTools wire mechanics: WebSocket framing, request
//! ids, response correlation, and bounded protocol events. Policy (which
//! endpoint, tab, origin, or action is allowed) lives in
//! [`super::runtime`] and in the host-owned connection source — never here and
//! never in model arguments.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Maximum CDP event history retained per session. Used to seed execution
/// contexts and diagnostics without unbounded memory.
const MAX_EVENT_HISTORY: usize = 4_096;
/// Maximum individual WebSocket message accepted from Chrome (64 MiB).
const MAX_WS_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// CDP wire error, deliberately carried as text so it never grants access.
#[derive(Debug, Clone)]
pub struct CdpError(pub String);

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CdpError {}

impl From<tokio_tungstenite::tungstenite::Error> for CdpError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self(format!("chrome websocket: {error}"))
    }
}

impl From<serde_json::Error> for CdpError {
    fn from(error: serde_json::Error) -> Self {
        Self(format!("chrome protocol frame: {error}"))
    }
}

impl From<std::io::Error> for CdpError {
    fn from(error: std::io::Error) -> Self {
        Self(format!("chrome transport io: {error}"))
    }
}

/// A raw CDP frame received from Chrome.
#[derive(Debug, Clone, PartialEq)]
pub enum CdpFrame {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

/// Transport abstraction so protocol tests can script Chrome without a
/// browser binary. The real implementation is a bounded tokio-tungstenite
/// socket; the mock is a channel pair.
#[async_trait]
pub trait CdpTransport: Send {
    async fn send_text(&mut self, text: &str) -> Result<(), CdpError>;
    async fn next(&mut self) -> Result<Option<CdpFrame>, CdpError>;
}

/// Real WebSocket transport to a host-derived DevTools endpoint.
pub struct WebSocketTransport {
    stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl WebSocketTransport {
    pub async fn connect(endpoint: &str) -> Result<Self, CdpError> {
        let request = endpoint
            .to_owned()
            .into_client_request()
            .map_err(|error| CdpError(format!("invalid chrome endpoint: {error}")))?;
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(MAX_WS_MESSAGE_BYTES));
        let (stream, _) =
            tokio_tungstenite::connect_async_with_config(request, Some(config), false).await?;
        Ok(Self { stream })
    }
}

#[async_trait]
impl CdpTransport for WebSocketTransport {
    async fn send_text(&mut self, text: &str) -> Result<(), CdpError> {
        self.stream
            .send(WsMessage::Text(text.to_owned().into()))
            .await?;
        Ok(())
    }

    async fn next(&mut self) -> Result<Option<CdpFrame>, CdpError> {
        loop {
            match self.stream.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let bytes = text.as_bytes().len();
                    if bytes > MAX_WS_MESSAGE_BYTES {
                        return Err(CdpError(format!(
                            "chrome message too large: {} bytes",
                            bytes
                        )));
                    }
                    return Ok(Some(CdpFrame::Text(text.to_string())));
                }
                Some(Ok(WsMessage::Binary(bytes))) => {
                    if bytes.len() > MAX_WS_MESSAGE_BYTES {
                        return Err(CdpError(format!(
                            "chrome message too large: {} bytes",
                            bytes.len()
                        )));
                    }
                    return Ok(Some(CdpFrame::Binary(bytes.to_vec())));
                }
                Some(Ok(WsMessage::Close(_))) => return Ok(Some(CdpFrame::Close)),
                Some(Ok(WsMessage::Ping(_)))
                | Some(Ok(WsMessage::Pong(_)))
                | Some(Ok(WsMessage::Frame(_))) => continue,
                Some(Err(error)) => return Err(error.into()),
                None => return Ok(None),
            }
        }
    }
}

/// One bounded Chrome protocol event the driver understands.
#[derive(Debug, Clone, PartialEq)]
pub enum CdpEvent {
    Console {
        session_id: Option<String>,
        level: String,
        text: String,
        url: String,
        line: u32,
        column: u32,
        timestamp_ms: u64,
    },
    Exception {
        session_id: Option<String>,
        text: String,
        url: String,
        line: u32,
        column: u32,
        timestamp_ms: u64,
    },
    RequestWillBeSent {
        session_id: Option<String>,
        request_id: String,
        method: String,
        url: String,
        resource_type: String,
        timestamp_ms: u64,
    },
    ResponseReceived {
        session_id: Option<String>,
        request_id: String,
        status: u16,
        mime_type: String,
        from_cache: bool,
        timestamp_ms: u64,
    },
    LoadingFailed {
        session_id: Option<String>,
        request_id: String,
        error_text: String,
        timestamp_ms: u64,
    },
    TargetCreated {
        target_id: String,
        url: String,
        title: String,
        r#type: String,
    },
    TargetDestroyed {
        target_id: String,
    },
    ContextCreated {
        session_id: Option<String>,
        context_id: u32,
        frame_id: String,
        url: String,
    },
    Lifecycle {
        session_id: Option<String>,
        frame_id: String,
        name: String,
        timestamp_ms: u64,
    },
}

fn truncated(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_owned()
    } else {
        let mut end = max;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

/// Parse one protocol event. Unknown methods are ignored by design; Chrome
/// may add events without invalidating the driver.
pub fn parse_event(frame: &Value, session_id: Option<String>) -> Option<CdpEvent> {
    let method = frame.get("method")?.as_str()?;
    let params = frame.get("params")?;
    let timestamp_ms = params
        .get("timestamp")
        .and_then(Value::as_f64)
        .map(|seconds| (seconds * 1000.0) as u64)
        .unwrap_or(0);
    match method {
        "Runtime.consoleAPICalled" => {
            let level = params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("log")
                .to_owned();
            let args = params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(|argument| argument.get("value").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let mut text = args;
            if text.is_empty() {
                text = params
                    .get("values")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
            }
            let stack = params
                .get("stackTrace")
                .and_then(|stack| stack.get("callFrames"))
                .and_then(Value::as_array)
                .and_then(|frames| frames.first());
            let (url, line, column) = match stack {
                Some(frame) => (
                    frame
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    frame.get("lineNumber").and_then(Value::as_u64).unwrap_or(0) as u32,
                    frame
                        .get("columnNumber")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                ),
                None => (
                    params
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    params
                        .get("lineNumber")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                    params
                        .get("columnNumber")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                ),
            };
            Some(CdpEvent::Console {
                session_id,
                level: truncated(&level, 32),
                text: truncated(&text, 4_096),
                url: truncated(&url, 4_096),
                line,
                column,
                timestamp_ms,
            })
        }
        "Runtime.exceptionThrown" => {
            let detail = params
                .get("exceptionDetails")
                .and_then(|details| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("exception");
            let stack = params
                .get("exceptionDetails")
                .and_then(|details| details.get("stackTrace"))
                .and_then(|stack| stack.get("callFrames"))
                .and_then(Value::as_array)
                .and_then(|frames| frames.first());
            let (url, line, column) = match stack {
                Some(frame) => (
                    frame
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    frame.get("lineNumber").and_then(Value::as_u64).unwrap_or(0) as u32,
                    frame
                        .get("columnNumber")
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32,
                ),
                None => (String::new(), 0, 0),
            };
            Some(CdpEvent::Exception {
                session_id,
                text: truncated(detail, 4_096),
                url: truncated(&url, 4_096),
                line,
                column,
                timestamp_ms,
            })
        }
        "Network.requestWillBeSent" => Some(CdpEvent::RequestWillBeSent {
            session_id,
            request_id: params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            method: params
                .get("request")
                .and_then(|request| request.get("method"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            url: params
                .get("request")
                .and_then(|request| request.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            resource_type: params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            timestamp_ms,
        }),
        "Network.responseReceived" => Some(CdpEvent::ResponseReceived {
            session_id,
            request_id: params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            status: params
                .get("response")
                .and_then(|response| response.get("status"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u16,
            mime_type: params
                .get("response")
                .and_then(|response| response.get("mimeType"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            from_cache: params
                .get("response")
                .and_then(|response| response.get("fromDiskCache"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            timestamp_ms,
        }),
        "Network.loadingFailed" => Some(CdpEvent::LoadingFailed {
            session_id,
            request_id: params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            error_text: params
                .get("errorText")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            timestamp_ms,
        }),
        "Target.targetCreated" => Some(CdpEvent::TargetCreated {
            target_id: params
                .get("targetInfo")
                .and_then(|info| info.get("targetId"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            url: params
                .get("targetInfo")
                .and_then(|info| info.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            title: params
                .get("targetInfo")
                .and_then(|info| info.get("title"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            r#type: params
                .get("targetInfo")
                .and_then(|info| info.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        "Target.targetDestroyed" => Some(CdpEvent::TargetDestroyed {
            target_id: params
                .get("targetId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        "Runtime.executionContextCreated" => {
            let context = params.get("context")?;
            let is_default = context
                .get("auxData")
                .and_then(|data| data.get("isDefault"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !is_default {
                return None;
            }
            Some(CdpEvent::ContextCreated {
                session_id,
                context_id: context.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
                frame_id: context
                    .get("auxData")
                    .and_then(|data| data.get("frameId"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                url: context
                    .get("origin")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        }
        "Page.lifecycleEvent" => Some(CdpEvent::Lifecycle {
            session_id,
            frame_id: params
                .get("frameId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            name: params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            timestamp_ms,
        }),
        _ => None,
    }
}

enum CdpCommandMsg {
    Run {
        id: u64,
        session_id: Option<String>,
        method: String,
        params: Value,
        reply: oneshot::Sender<Result<Value, CdpError>>,
        authorized: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    },
    Shutdown,
}

/// A session-correlated command futures holder.
struct PendingCommand {
    reply: oneshot::Sender<Result<Value, CdpError>>,
}

/// One multiplexed DevTools protocol session. Commands carry an optional
/// `sessionId` so attached targets share one transport while staying
/// isolated in Chrome's session namespace.
#[derive(Clone)]
pub struct CdpSession {
    commands: mpsc::UnboundedSender<CdpCommandMsg>,
    next_id: std::sync::Arc<std::sync::atomic::AtomicU64>,
    events: std::sync::Arc<Mutex<Vec<CdpEvent>>>,
}

impl CdpSession {
    /// Connect through a real websocket. The endpoint is always host-derived.
    pub async fn connect(endpoint: &str) -> Result<Self, CdpError> {
        Ok(Self::with_transport(
            WebSocketTransport::connect(endpoint).await?,
        ))
    }

    /// Create a session over any transport (used by protocol tests).
    pub fn with_transport(transport: impl CdpTransport + 'static) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let events = std::sync::Arc::new(Mutex::new(Vec::new()));
        let session = Self {
            commands: command_tx,
            next_id: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1)),
            events: events.clone(),
        };
        tokio::spawn(reader_loop(transport, command_rx, events));
        session
    }

    /// Send one CDP command and await its correlated reply.
    pub async fn command(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.command_guarded(None, method, params, std::sync::Arc::new(|| true))
            .await
    }

    /// Send one CDP command in an attached target session.
    pub async fn command_in_session(
        &self,
        session_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        self.command_guarded(
            Some(session_id),
            method,
            params,
            std::sync::Arc::new(|| true),
        )
        .await
    }

    pub(crate) async fn command_guarded(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
        authorized: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Value, CdpError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.commands
            .send(CdpCommandMsg::Run {
                id,
                session_id: session_id.map(str::to_owned),
                method: method.to_owned(),
                params,
                reply: reply_tx,
                authorized,
            })
            .map_err(|_| CdpError("chrome protocol task is closed".to_owned()))?;
        reply_rx
            .await
            .map_err(|_| CdpError("chrome protocol session closed during command".to_owned()))?
    }

    /// Bounded clone of recently observed protocol events (newest last).
    pub fn recent_events(&self) -> Vec<CdpEvent> {
        let events = self.events.lock().expect("cdp event history");
        events.clone()
    }

    /// Close the protocol task and transport.
    pub fn close(&self) {
        let _ = self.commands.send(CdpCommandMsg::Shutdown);
    }
}

async fn reader_loop(
    mut transport: impl CdpTransport,
    mut commands: mpsc::UnboundedReceiver<CdpCommandMsg>,
    history: std::sync::Arc<Mutex<Vec<CdpEvent>>>,
) {
    let mut pending: HashMap<u64, PendingCommand> = HashMap::new();
    loop {
        tokio::select! {
            command = commands.recv() => {
                match command {
                    Some(CdpCommandMsg::Shutdown) | None => {
                        for (_, pending_command) in pending.drain() {
                            let _ = pending_command.reply.send(Err(CdpError("chrome protocol session closed".to_owned())));
                        }
                        return;
                    }
                    Some(CdpCommandMsg::Run { id, session_id, method, params, reply, authorized }) => {
                        pending.retain(|_, value| !value.reply.is_closed());
                        if reply.is_closed() || !authorized() {
                            let _ = reply.send(Err(CdpError("Chrome authority ended before dispatch".into())));
                            continue;
                        }
                        let mut request = json!({
                            "id": id,
                            "method": method,
                            "params": params,
                        });
                        if let Some(session_id) = &session_id {
                            request["sessionId"] = json!(session_id);
                        }
                        if let Err(error) = transport.send_text(&request.to_string()).await {
                            let _ = reply.send(Err(error));
                            continue;
                        }
                        pending.insert(id, PendingCommand { reply });
                    }
                }
            }
            frame = transport.next() => {
                match frame {
                    Ok(Some(CdpFrame::Text(text))) => {
                        let parsed: Value = match serde_json::from_str(&text) {
                            Ok(value) => value,
                            Err(error) => {
                                tracing::warn!(error = %error, "non-JSON Chrome protocol frame dropped");
                                continue;
                            }
                        };
                        if let Some(id) = parsed.get("id").and_then(Value::as_u64) {
                            if let Some(pending_command) = pending.remove(&id) {
                                let result = if let Some(error) = parsed.get("error") {
                                    Err(CdpError(format!(
                                        "{}: {}",
                                        error.get("message").and_then(Value::as_str).unwrap_or("protocol error"),
                                        error.get("code").and_then(Value::as_u64).unwrap_or(0)
                                    )))
                                } else {
                                    parsed.get("result").cloned().ok_or_else(|| CdpError("CDP response has no result".to_owned()))
                                };
                                let _ = pending_command.reply.send(result);
                            }
                        } else if let Some(event) = parse_event(&parsed, parsed.get("sessionId").and_then(Value::as_str).map(str::to_owned)) {
                            let mut stored = history.lock().expect("cdp event history");
                            stored.push(event);
                            if stored.len() > MAX_EVENT_HISTORY {
                                let excess = stored.len() - MAX_EVENT_HISTORY;
                                stored.drain(..excess);
                            }
                        }
                    }
                    Ok(Some(CdpFrame::Binary(_))) => {}
                    Ok(Some(CdpFrame::Close)) | Ok(None) => {
                        for (_, pending_command) in pending.drain() {
                            let _ = pending_command.reply.send(Err(CdpError("chrome transport closed".to_owned())));
                        }
                        return;
                    }
                    Err(error) => {
                        for (_, pending_command) in pending.drain() {
                            let _ = pending_command.reply.send(Err(error.clone()));
                        }
                        return;
                    }
                }
            }
        }
    }
}
