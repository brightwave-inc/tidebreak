//! Loopback MCP bridge that serves Tidebreak's connected apps to external
//! engines.
//!
//! The in-process engine reads every mounted MCP server — the gateway
//! endpoints an organization entitles plus locally configured servers —
//! straight from the [`crate::mcp_config::McpRuntime`] snapshot. Claude
//! Code, Codex, OpenCode, and Grok run as children and cannot, so without
//! this bridge a chat or code session on one of them has no connected apps
//! at all. The bridge closes that gap: an external engine mounts
//! `POST /code/mcp/connected-apps` as one HTTP MCP server named
//! [`AppsChannelSpec::MCP_SERVER`], authenticated with a session-scoped
//! bearer (not the install token), and sees the same tools the in-process
//! engine sees.
//!
//! Tool names drop the runtime's `mcp__` prefix, so a tool the in-process
//! engine calls `mcp__primary__github__proxy_api` is advertised here as
//! `primary__github__proxy_api`; the engine's own namespacing puts the
//! server name back in front. Gateway bearers, attestation contexts, and
//! reconnects stay inside Tidebreak: the child only ever holds the loopback
//! token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use tidebreak_core::{OwnerId, SessionId, SessionLifecycle, ToolCtx, ToolRegistry};
use tidebreak_harness::AppsChannelSpec;

use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

/// The runtime prefix every mounted MCP tool carries.
const MCP_PREFIX: &str = "mcp__";

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppsTokenSubject {
    owner: OwnerId,
    session_id: SessionId,
    spawn_epoch: i64,
}

/// Session-scoped bearers for the connected-apps bridge.
#[derive(Default)]
pub struct AppsBridge {
    tokens: Mutex<HashMap<String, AppsTokenSubject>>,
    by_session: Mutex<HashMap<SessionId, String>>,
    /// The fingerprint of the tool list each bearer last received, so the
    /// event stream asks an engine to list again only when that list is out
    /// of date.
    listed: Mutex<HashMap<String, u64>>,
    /// Bumped each time a bearer's list is recorded, so an event stream also
    /// checks again then. A tool change that lands between building a list
    /// and recording it would otherwise go unnoticed until the next change.
    lists: tokio::sync::watch::Sender<u64>,
}

impl AppsBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mint a worker-scoped token. Every older token for the session dies.
    pub fn issue_token(&self, owner: &OwnerId, session_id: SessionId, spawn_epoch: i64) -> String {
        let token = format!("cap_apps_{}", Uuid::new_v4());
        let mut by_session = self.by_session.lock().expect("apps tokens");
        let mut tokens = self.tokens.lock().expect("apps tokens");
        Self::revoke_session_locked(&mut by_session, &mut tokens, session_id);
        by_session.insert(session_id, token.clone());
        tokens.insert(
            token.clone(),
            AppsTokenSubject {
                owner: owner.clone(),
                session_id,
                spawn_epoch,
            },
        );
        self.forget_revoked_lists(&tokens);
        token
    }

    fn subject_for_token(&self, token: &str) -> Option<AppsTokenSubject> {
        self.tokens.lock().expect("apps tokens").get(token).cloned()
    }

    /// Revoke one worker's bearer.
    pub fn revoke_session(&self, session_id: SessionId) {
        let mut by_session = self.by_session.lock().expect("apps tokens");
        let mut tokens = self.tokens.lock().expect("apps tokens");
        Self::revoke_session_locked(&mut by_session, &mut tokens, session_id);
        self.forget_revoked_lists(&tokens);
    }

    fn revoke_session_locked(
        by_session: &mut HashMap<SessionId, String>,
        tokens: &mut HashMap<String, AppsTokenSubject>,
        session_id: SessionId,
    ) {
        by_session.remove(&session_id);
        tokens.retain(|_, subject| subject.session_id != session_id);
    }

    /// Drop the list fingerprints of bearers that no longer exist.
    fn forget_revoked_lists(&self, tokens: &HashMap<String, AppsTokenSubject>) {
        self.listed
            .lock()
            .expect("apps lists")
            .retain(|token, _| tokens.contains_key(token));
    }

    /// Note the fingerprint of the tool list `token` just received.
    fn record_listed(&self, token: &str, fingerprint: u64) {
        if self.subject_for_token(token).is_some() {
            self.listed
                .lock()
                .expect("apps lists")
                .insert(token.to_owned(), fingerprint);
            self.lists
                .send_modify(|lists| *lists = lists.wrapping_add(1));
        }
    }

    /// Note that `token`'s engine was told about `fingerprint`, so the
    /// stream does not tell it again before it lists or the tools change.
    /// Wakes no stream.
    fn record_listed_quietly(&self, token: &str, fingerprint: u64) {
        if self.subject_for_token(token).is_some() {
            self.listed
                .lock()
                .expect("apps lists")
                .insert(token.to_owned(), fingerprint);
        }
    }

    fn listed_fingerprint(&self, token: &str) -> Option<u64> {
        self.listed.lock().expect("apps lists").get(token).copied()
    }
}

impl crate::code::CodeRuntime {
    /// The connected-apps channel for one external-engine worker: a fresh
    /// session-scoped bearer over the loopback bridge. `None` when the
    /// server has no loopback listener yet, which is the only state where
    /// there is nothing to mount.
    pub(super) fn apps_channel(
        &self,
        owner: &OwnerId,
        session_id: SessionId,
        spawn_epoch: i64,
    ) -> Option<AppsChannelSpec> {
        self.apps.revoke_session(session_id);
        let base = self.loopback_base.lock().expect("loopback base").clone()?;
        let token = self.apps.issue_token(owner, session_id, spawn_epoch);
        Some(AppsChannelSpec {
            mcp_endpoint_url: format!("{base}/code/mcp/connected-apps"),
            token,
        })
    }
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

/// The tool the bridge adds to its list while saved servers are still
/// making their first connection, so an engine's model knows why their tools
/// are missing. No mounted tool can take this name: every mounted name
/// carries its server's namespace and a `__`.
const STATUS_TOOL: &str = "connected_apps_status";

/// What the event stream sends when an engine's tool list is out of date.
const LIST_CHANGED: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

/// How often the event stream sends a comment, so an idle connection is not
/// taken for a dead one.
const EVENT_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(15);

/// Check a request's bearer against the live worker it names.
///
/// Every method rides a token that names one live worker. Refuse the whole
/// surface, not just calls, once that worker is gone: a replaced or ended
/// session must not keep listing an owner's apps.
async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
) -> std::result::Result<(Arc<crate::code::CodeRuntime>, AppsTokenSubject, String), Response> {
    let Some(runtime) = state.code.clone() else {
        return Err(
            ServerError::internal("code mode is not configured on this server").into_response(),
        );
    };
    let Some(token) = bearer_token(headers) else {
        return Err(ServerError::unauthorized("missing connected-apps token").into_response());
    };
    let Some(subject) = runtime.apps.subject_for_token(token) else {
        return Err(ServerError::unauthorized("unknown connected-apps token").into_response());
    };
    let session =
        match tidebreak_core::db::code::get_session_all_owners(&runtime.db, subject.session_id)
            .await
        {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(ServerError::not_found(format!(
                    "session {} not found",
                    subject.session_id
                ))
                .into_response())
            }
            Err(err) => return Err(ServerError::from(err).into_response()),
        };
    if session.owner != subject.owner || session.spawn_epoch != subject.spawn_epoch {
        return Err(ServerError::conflict_kind(
            "apps_worker_replaced",
            "the worker that issued this connected-apps token is no longer attached",
        )
        .into_response());
    }
    if session.lifecycle == SessionLifecycle::Ended {
        return Err(
            ServerError::conflict_kind("session_ended", "session has ended").into_response(),
        );
    }
    let token = token.to_owned();
    Ok((runtime, subject, token))
}

/// `POST /code/mcp/connected-apps` — an external engine's HTTP MCP client.
pub async fn connected_apps(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, ServerError>,
) -> Response {
    let Json(value) = match body {
        Ok(json) => json,
        Err(err) => return err.into_response(),
    };
    let (runtime, subject, token) = match authorize(&state, &headers).await {
        Ok(authorized) => authorized,
        Err(refused) => return refused,
    };
    let request: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(err) => {
            return json_rpc_error(None, -32700, format!("parse error: {err}")).into_response();
        }
    };
    if request.jsonrpc.as_deref().is_some_and(|v| v != "2.0") {
        return json_rpc_error(request.id, -32600, "jsonrpc must be 2.0".into()).into_response();
    }
    // Only a tool list waits for saved servers still connecting after boot,
    // and only until one deadline measured from boot: an engine started then
    // lists its tools right away, and its first list should hold what it
    // can. Every other method answers at once.
    let view = match request.method.as_deref() {
        Some("tools/list") => {
            let tools = state.mcp.tools_after_boot().await;
            runtime.apps.record_listed(&token, tools.fingerprint);
            BridgeView {
                registry: tools.registry,
                connecting: tools.connecting,
                connected: Vec::new(),
            }
        }
        Some("tools/call") if called_tool(&request.params) == Some(STATUS_TOOL) => {
            let tools = state.mcp.tools_view().await;
            BridgeView {
                registry: tools.registry,
                connecting: tools.connecting,
                connected: state.mcp.connected_tool_counts().await,
            }
        }
        _ => BridgeView {
            registry: state.mcp.snapshot(),
            connecting: Vec::new(),
            connected: Vec::new(),
        },
    };
    // Connected apps run under the session as the chat context: gateway
    // call bearers are minted against it, exactly as for the in-process
    // engine.
    let ctx = ToolCtx::without_private_scratch(subject.session_id, None);
    dispatch(&view, &ctx, request).await.into_response()
}

/// `GET /code/mcp/connected-apps` — the event stream an engine's MCP client
/// opens beside its requests (Streamable HTTP).
///
/// It carries `notifications/tools/list_changed` whenever the tools the
/// engine last listed are out of date: a saved server finished connecting
/// after boot, a server reconnected with other tools, or a server went away.
/// An engine that follows the notification lists its tools again, so a list
/// taken while servers were still connecting heals on its own.
pub async fn connected_apps_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (runtime, _subject, token) = match authorize(&state, &headers).await {
        Ok(authorized) => authorized,
        Err(refused) => return refused,
    };
    let stream = list_changed_events(runtime.apps.clone(), token, state.mcp.tool_changes());
    axum::response::sse::Sse::new(stream)
        .keep_alive(axum::response::sse::KeepAlive::new().interval(EVENT_KEEP_ALIVE))
        .into_response()
}

/// One `list_changed` event each time the advertised tools stop matching
/// what `token` last listed, checked when the stream opens, on every tool
/// change, and whenever a list is recorded. A bearer that has not listed yet
/// has nothing out of date, and a revoked bearer ends the stream.
fn list_changed_events(
    apps: Arc<AppsBridge>,
    token: String,
    changes: tokio::sync::watch::Receiver<u64>,
) -> impl futures::Stream<
    Item = std::result::Result<axum::response::sse::Event, std::convert::Infallible>,
> + Send
       + 'static {
    let lists = apps.lists.subscribe();
    futures::stream::unfold(
        (apps, token, changes, lists, false),
        |(apps, token, mut changes, mut lists, mut started)| async move {
            loop {
                if started {
                    tokio::select! {
                        changed = changes.changed() => changed.ok()?,
                        // The bridge owns the sender, so this never closes.
                        _ = lists.changed() => {}
                    }
                }
                started = true;
                apps.subject_for_token(&token)?;
                lists.borrow_and_update();
                let current = *changes.borrow_and_update();
                if apps
                    .listed_fingerprint(&token)
                    .is_some_and(|listed| listed != current)
                {
                    // Sent once per list: the next one waits for the
                    // engine to list again or for another change.
                    apps.record_listed_quietly(&token, current);
                    let event = axum::response::sse::Event::default().data(LIST_CHANGED);
                    return Some((Ok(event), (apps, token, changes, lists, started)));
                }
            }
        },
    )
}

/// The name a `tools/call` request asks for.
fn called_tool(params: &Value) -> Option<&str> {
    params.get("name").and_then(Value::as_str)
}

/// What one request is answered from: the tools, the servers still making
/// their first connection, and each connected server's tool count.
struct BridgeView {
    registry: Arc<ToolRegistry>,
    connecting: Vec<String>,
    connected: Vec<(String, usize)>,
}

/// Answer one JSON-RPC request against a view of the MCP runtime. Pure over
/// its inputs so the projection and call paths are testable without a
/// listener.
async fn dispatch(view: &BridgeView, ctx: &ToolCtx, request: JsonRpcRequest) -> Response {
    match request.method.as_deref() {
        Some("initialize") => json_rpc_ok(
            request.id,
            json!({
                "protocolVersion": request.params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25"),
                // The list changes after boot and on every reconnect; the
                // event stream says when.
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": AppsChannelSpec::MCP_SERVER, "version": "0.0.1" },
            }),
        )
        .into_response(),
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("ping") => json_rpc_ok(request.id, json!({})).into_response(),
        Some("tools/list") => json_rpc_ok(
            request.id,
            json!({ "tools": advertised_tools(&view.registry, &view.connecting) }),
        )
        .into_response(),
        Some("tools/call") if called_tool(&request.params) == Some(STATUS_TOOL) => {
            tool_result(request.id, status_report(view), None, false).into_response()
        }
        Some("tools/call") => call_tool(&view.registry, ctx, request.id, request.params)
            .await
            .into_response(),
        Some(other) => {
            json_rpc_error(request.id, -32601, format!("Method not found: {other}")).into_response()
        }
        None => json_rpc_error(request.id, -32600, "missing method".into()).into_response(),
    }
}

/// The status tool's entry, naming the servers still connecting.
fn status_tool(connecting: &[String]) -> Value {
    json!({
        "name": STATUS_TOOL,
        "description": format!(
            "Tidebreak connected apps that were still connecting when this list was built: {}. \
             Their tools are missing from this list until they connect, and Tidebreak asks your \
             client to list its tools again when they do. Call this tool to see which are \
             connected now.",
            connecting.join(", ")
        ),
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "title": "Connected apps status", "readOnlyHint": true },
    })
}

/// What the status tool reports: which apps are still connecting, and which
/// are connected with how many tools.
fn status_report(view: &BridgeView) -> String {
    let still = if view.connecting.is_empty() {
        "Every connected app has finished connecting.".to_owned()
    } else {
        format!("Still connecting: {}.", view.connecting.join(", "))
    };
    let connected = if view.connected.is_empty() {
        "No connected app is up yet.".to_owned()
    } else {
        let apps: Vec<String> = view
            .connected
            .iter()
            .map(|(name, tools)| {
                format!(
                    "{name} ({tools} tool{})",
                    if *tools == 1 { "" } else { "s" }
                )
            })
            .collect();
        format!("Connected: {}.", apps.join(", "))
    };
    format!(
        "{still} {connected} Tools from an app that connected after your tool list was built \
         appear once your client lists its tools again."
    )
}

/// Every mounted MCP tool, advertised without the runtime's `mcp__` prefix.
/// Built-in and client tools are not connected apps and stay out. While
/// servers are still making their first connection, the status tool joins
/// the list and names them.
fn advertised_tools(registry: &ToolRegistry, connecting: &[String]) -> Vec<Value> {
    let mut tools: Vec<Value> = registry
        .specs()
        .into_iter()
        .filter_map(|spec| {
            let name = spec.name.strip_prefix(MCP_PREFIX)?;
            Some(json!({
                "name": name,
                "description": spec.description,
                "inputSchema": spec.input_schema,
            }))
        })
        .collect();
    if !connecting.is_empty() {
        tools.push(status_tool(connecting));
    }
    tools
}

async fn call_tool(
    registry: &ToolRegistry,
    ctx: &ToolCtx,
    id: Option<Value>,
    params: Value,
) -> Json<JsonRpcResponse> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let mounted = format!("{MCP_PREFIX}{name}");
    let Some(tool) = registry.server_tool(&mounted) else {
        return tool_result(id, format!("unknown tool {name}"), None, true);
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match tool.execute(ctx, arguments).await {
        Ok(output) => tool_result(id, output.content, output.data, output.is_error),
        Err(error) => tool_result(id, error.to_string(), None, true),
    }
}

fn tool_result(
    id: Option<Value>,
    text: String,
    data: Option<Value>,
    is_error: bool,
) -> Json<JsonRpcResponse> {
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if let Some(data) = data {
        result["structuredContent"] = data;
    }
    json_rpc_ok(id, result)
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    value.strip_prefix("Bearer ").map(str::trim)
}

fn json_rpc_ok(id: Option<Value>, result: Value) -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    })
}

fn json_rpc_error(id: Option<Value>, code: i64, message: String) -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({ "code": code, "message": message })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use tidebreak_core::{AgentError, ApprovalClass, Tool, ToolOutput, ToolSpec};

    /// A mounted MCP tool that echoes its arguments and records the chat it
    /// ran under.
    struct Echo {
        name: &'static str,
        fail: bool,
    }

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "echo".into(),
                input_schema: json!({ "type": "object" }),
            }
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }

        async fn execute(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput, AgentError> {
            if self.fail {
                return Err(AgentError::msg("upstream refused"));
            }
            Ok(
                ToolOutput::text(format!("chat={} args={args}", ctx.chat_id))
                    .with_data(json!({ "echoed": args })),
            )
        }
    }

    fn registry() -> ToolRegistry {
        ToolRegistry::new()
            .with(Box::new(Echo {
                name: "mcp__primary__github__proxy_api",
                fail: false,
            }))
            .with(Box::new(Echo {
                name: "mcp__docs__lookup",
                fail: true,
            }))
            .with(Box::new(Echo {
                name: "web_search",
                fail: false,
            }))
    }

    async fn body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// A view with every server up.
    fn view(registry: ToolRegistry) -> BridgeView {
        BridgeView {
            registry: Arc::new(registry),
            connecting: Vec::new(),
            connected: Vec::new(),
        }
    }

    fn request(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: Some(method.into()),
            params,
        }
    }

    /// The names a `tools/list` answer advertises.
    async fn listed_names(view: &BridgeView) -> Vec<String> {
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        body(dispatch(view, &ctx, request("tools/list", json!({}))).await).await["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The engine's client learns at `initialize` that the list changes, so
    /// it opens the event stream and lists again when told to.
    #[tokio::test]
    async fn initialize_says_the_tool_list_changes() {
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let answer = body(
            dispatch(
                &view(registry()),
                &ctx,
                request("initialize", json!({ "protocolVersion": "2025-06-18" })),
            )
            .await,
        )
        .await;
        assert_eq!(
            answer["result"]["capabilities"]["tools"]["listChanged"],
            json!(true)
        );
    }

    /// While a server is still making its first connection, the list says
    /// so through the status tool, and the status tool reports what is up.
    /// Once nothing is connecting, the status tool leaves the list.
    #[tokio::test]
    async fn a_list_built_while_servers_connect_names_them() {
        let mut connecting = view(registry());
        connecting.connecting = vec!["linear".to_owned()];
        connecting.connected = vec![("primary".to_owned(), 1)];
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let listed =
            body(dispatch(&connecting, &ctx, request("tools/list", json!({}))).await).await;
        let status = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == STATUS_TOOL)
            .expect("the status tool is listed while a server connects")
            .clone();
        assert!(
            status["description"]
                .as_str()
                .unwrap()
                .contains("still connecting when this list was built: linear."),
            "{status}"
        );

        let report = body(
            dispatch(
                &connecting,
                &ctx,
                request(
                    "tools/call",
                    json!({ "name": STATUS_TOOL, "arguments": {} }),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(report["result"]["isError"], json!(false));
        let text = report["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("Still connecting: linear. Connected: primary (1 tool)."),
            "{text}"
        );

        assert!(!listed_names(&view(registry()))
            .await
            .contains(&STATUS_TOOL.to_owned()));
    }

    /// The next event, `None` when none arrives in time, and `Some(None)`
    /// when the stream ended.
    async fn next_event<S: futures::Stream + Unpin>(events: &mut S) -> Option<Option<S::Item>> {
        use futures::StreamExt as _;
        tokio::time::timeout(std::time::Duration::from_millis(200), events.next())
            .await
            .ok()
    }

    /// The event stream asks an engine to list again only when the list it
    /// last received is out of date, once per change, including a change
    /// that landed while its list was being built. A revoked bearer's stream
    /// ends.
    #[tokio::test]
    async fn the_event_stream_asks_for_a_new_list_only_when_the_last_is_out_of_date() {
        let bridge = AppsBridge::new();
        let owner = OwnerId::local();
        let session = SessionId::new();
        let token = bridge.issue_token(&owner, session, 1);
        let other = bridge.issue_token(&owner, SessionId::new(), 1);
        let (tools, changes) = tokio::sync::watch::channel(1_u64);
        let mut events = Box::pin(list_changed_events(bridge.clone(), token.clone(), changes));

        // Nothing listed yet, so nothing is out of date.
        tools.send(2).unwrap();
        assert!(next_event(&mut events).await.is_none());

        bridge.record_listed(&token, 2);
        assert!(next_event(&mut events).await.is_none());
        tools.send(3).unwrap();
        assert!(matches!(next_event(&mut events).await, Some(Some(Ok(_)))));
        // Told once: another engine listing does not repeat it.
        bridge.record_listed(&other, 3);
        assert!(next_event(&mut events).await.is_none());

        // The engine lists again, but its list was built just before the
        // tools changed, so it is already out of date.
        tools.send(4).unwrap();
        assert!(matches!(next_event(&mut events).await, Some(Some(Ok(_)))));
        tools.send(5).unwrap();
        assert!(matches!(next_event(&mut events).await, Some(Some(Ok(_)))));
        bridge.record_listed(&token, 4);
        assert!(matches!(next_event(&mut events).await, Some(Some(Ok(_)))));
        bridge.record_listed(&token, 5);
        assert!(next_event(&mut events).await.is_none());

        bridge.revoke_session(session);
        tools.send(6).unwrap();
        assert!(matches!(next_event(&mut events).await, Some(None)));
    }

    #[tokio::test]
    async fn tools_list_advertises_only_mounted_mcp_tools_without_the_prefix() {
        let view = view(registry());
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let response = dispatch(&view, &ctx, request("tools/list", json!({}))).await;
        let names: Vec<String> = body(response).await["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["docs__lookup", "primary__github__proxy_api"]);
    }

    #[tokio::test]
    async fn tools_call_runs_the_mounted_tool_under_the_session_context() {
        let view = view(registry());
        let session = SessionId::new();
        let ctx = ToolCtx::without_private_scratch(session, None);
        let response = dispatch(
            &view,
            &ctx,
            request(
                "tools/call",
                json!({ "name": "primary__github__proxy_api", "arguments": { "operation_id": "getIssue" } }),
            ),
        )
        .await;
        let result = body(response).await["result"].clone();
        assert_eq!(result["isError"], json!(false));
        assert_eq!(
            result["content"][0]["text"],
            json!(format!(
                "chat={session} args={{\"operation_id\":\"getIssue\"}}"
            ))
        );
        assert_eq!(
            result["structuredContent"]["echoed"]["operation_id"],
            json!("getIssue")
        );
    }

    #[tokio::test]
    async fn tool_failures_and_unknown_names_are_tool_errors_not_transport_errors() {
        let view = view(registry());
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let failed = body(
            dispatch(
                &view,
                &ctx,
                request(
                    "tools/call",
                    json!({ "name": "docs__lookup", "arguments": {} }),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(failed["result"]["isError"], json!(true));
        assert_eq!(
            failed["result"]["content"][0]["text"],
            json!("upstream refused")
        );

        // A built-in tool is not a connected app even though it is registered.
        let hidden = body(
            dispatch(
                &view,
                &ctx,
                request(
                    "tools/call",
                    json!({ "name": "web_search", "arguments": {} }),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(hidden["result"]["isError"], json!(true));
        assert_eq!(
            hidden["result"]["content"][0]["text"],
            json!("unknown tool web_search")
        );
    }

    #[test]
    fn a_new_token_replaces_the_session_s_old_one() {
        let bridge = AppsBridge::new();
        let owner = OwnerId::local();
        let session = SessionId::new();
        let first = bridge.issue_token(&owner, session, 1);
        let second = bridge.issue_token(&owner, session, 2);
        assert!(bridge.subject_for_token(&first).is_none());
        assert_eq!(
            bridge.subject_for_token(&second).map(|s| s.spawn_epoch),
            Some(2)
        );
        bridge.revoke_session(session);
        assert!(bridge.subject_for_token(&second).is_none());
    }
}
