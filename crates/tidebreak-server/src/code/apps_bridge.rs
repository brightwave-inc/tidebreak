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
    }

    fn revoke_session_locked(
        by_session: &mut HashMap<SessionId, String>,
        tokens: &mut HashMap<String, AppsTokenSubject>,
        session_id: SessionId,
    ) {
        by_session.remove(&session_id);
        tokens.retain(|_, subject| subject.session_id != session_id);
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
    let Some(runtime) = state.code.as_deref() else {
        return ServerError::internal("code mode is not configured on this server").into_response();
    };
    let Some(token) = bearer_token(&headers) else {
        return ServerError::unauthorized("missing connected-apps token").into_response();
    };
    let Some(subject) = runtime.apps.subject_for_token(token) else {
        return ServerError::unauthorized("unknown connected-apps token").into_response();
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
    // Every method rides a token that names one live worker. Refuse the
    // whole surface, not just calls, once that worker is gone: a replaced
    // or ended session must not keep listing an owner's apps.
    let session =
        match tidebreak_core::db::code::get_session_all_owners(&runtime.db, subject.session_id)
            .await
        {
            Ok(Some(session)) => session,
            Ok(None) => {
                return ServerError::not_found(format!("session {} not found", subject.session_id))
                    .into_response()
            }
            Err(err) => return ServerError::from(err).into_response(),
        };
    if session.owner != subject.owner || session.spawn_epoch != subject.spawn_epoch {
        return ServerError::conflict_kind(
            "apps_worker_replaced",
            "the worker that issued this connected-apps token is no longer attached",
        )
        .into_response();
    }
    if session.lifecycle == SessionLifecycle::Ended {
        return ServerError::conflict_kind("session_ended", "session has ended").into_response();
    }
    let registry = state.mcp.snapshot();
    // Connected apps run under the session as the chat context: gateway
    // call bearers are minted against it, exactly as for the in-process
    // engine.
    let ctx = ToolCtx::without_private_scratch(subject.session_id, None);
    dispatch(&registry, &ctx, request).await.into_response()
}

/// Answer one JSON-RPC request against the current MCP snapshot. Pure over
/// its inputs so the projection and call paths are testable without a
/// listener.
async fn dispatch(registry: &ToolRegistry, ctx: &ToolCtx, request: JsonRpcRequest) -> Response {
    match request.method.as_deref() {
        Some("initialize") => json_rpc_ok(
            request.id,
            json!({
                "protocolVersion": request.params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": AppsChannelSpec::MCP_SERVER, "version": "0.0.1" },
            }),
        )
        .into_response(),
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("ping") => json_rpc_ok(request.id, json!({})).into_response(),
        Some("tools/list") => {
            json_rpc_ok(request.id, json!({ "tools": advertised_tools(registry) })).into_response()
        }
        Some("tools/call") => call_tool(registry, ctx, request.id, request.params)
            .await
            .into_response(),
        Some(other) => {
            json_rpc_error(request.id, -32601, format!("Method not found: {other}")).into_response()
        }
        None => json_rpc_error(request.id, -32600, "missing method".into()).into_response(),
    }
}

/// Every mounted MCP tool, advertised without the runtime's `mcp__` prefix.
/// Built-in and client tools are not connected apps and stay out.
fn advertised_tools(registry: &ToolRegistry) -> Vec<Value> {
    registry
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
        .collect()
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

    fn request(method: &str, params: Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(1)),
            method: Some(method.into()),
            params,
        }
    }

    #[tokio::test]
    async fn tools_list_advertises_only_mounted_mcp_tools_without_the_prefix() {
        let registry = registry();
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let response = dispatch(&registry, &ctx, request("tools/list", json!({}))).await;
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
        let registry = registry();
        let session = SessionId::new();
        let ctx = ToolCtx::without_private_scratch(session, None);
        let response = dispatch(
            &registry,
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
        let registry = registry();
        let ctx = ToolCtx::without_private_scratch(SessionId::new(), None);
        let failed = body(
            dispatch(
                &registry,
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
                &registry,
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
