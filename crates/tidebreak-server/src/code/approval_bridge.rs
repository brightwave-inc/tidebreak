//! Loopback MCP permission-prompt endpoint for Claude Code 2.1.233.
//!
//! The CLI calls `POST /code/mcp/approval-prompt` as an HTTP MCP server,
//! authenticated with a session-scoped bearer (not the install token). A
//! `tools/call` of `permission_prompt` parks until
//! [`ApprovalBridge::complete`] runs from the decision route.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use uuid::Uuid;

use tidebreak_core::{CodeSessionId, CodeSessionLifecycle};
use tidebreak_harness::claude::approvals::{
    event_from_prompt_request, PermissionPromptRequest, PermissionPromptResponse, APPROVAL_MCP_TOOL,
};
use tidebreak_harness::{ApprovalCompleter, ApprovalDecision, HarnessError};

use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

/// In-process park for one session's permission-prompt MCP calls.
#[derive(Default)]
pub(crate) struct ApprovalBridge {
    tokens: Mutex<HashMap<String, CodeSessionId>>,
    by_session: Mutex<HashMap<CodeSessionId, String>>,
    parked: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
}

impl ApprovalBridge {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mint a session-scoped token. A previous token for the same session dies.
    pub(crate) fn issue_token(&self, session_id: CodeSessionId) -> String {
        let token = format!("cma_{}", Uuid::new_v4());
        let mut by_session = self.by_session.lock().expect("approval tokens");
        let mut tokens = self.tokens.lock().expect("approval tokens");
        if let Some(old) = by_session.insert(session_id, token.clone()) {
            tokens.remove(&old);
        }
        tokens.insert(token.clone(), session_id);
        token
    }

    pub(crate) fn session_for_token(&self, token: &str) -> Option<CodeSessionId> {
        self.tokens
            .lock()
            .expect("approval tokens")
            .get(token)
            .copied()
    }

    fn park(&self, call_id: String) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.parked
            .lock()
            .expect("approval park")
            .insert(call_id, tx);
        rx
    }
}

#[async_trait]
impl ApprovalCompleter for ApprovalBridge {
    async fn complete(
        &self,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let sender = self.parked.lock().expect("approval park").remove(call_id);
        match sender {
            Some(tx) => {
                let _ = tx.send(decision);
            }
            None => {
                // Restart / reconnect: the original MCP call is gone; the
                // decision still records on the approval row.
                let _ = call_id;
            }
        }
        Ok(())
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

/// `POST /code/mcp/approval-prompt` — Claude Code's HTTP MCP client.
pub async fn approval_prompt(
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
    let token = bearer_token(&headers);
    let Some(token) = token else {
        return ServerError::unauthorized("missing approval token").into_response();
    };
    let Some(session_id) = runtime.approvals.session_for_token(token) else {
        return ServerError::unauthorized("unknown approval token").into_response();
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
    match request.method.as_deref() {
        Some("initialize") => json_rpc_ok(
            request.id,
            json!({
                "protocolVersion": request.params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25"),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tb-approvals", "version": "0.0.1" },
            }),
        )
        .into_response(),
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        Some("ping") => json_rpc_ok(request.id, json!({})).into_response(),
        Some("tools/list") => json_rpc_ok(
            request.id,
            json!({
                "tools": [{
                    "name": APPROVAL_MCP_TOOL,
                    "description": "Decide whether a tool call is allowed.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "tool_name": { "type": "string" },
                            "input": { "type": "object" },
                            "tool_use_id": { "type": "string" }
                        },
                        "required": ["tool_name", "tool_use_id"]
                    }
                }]
            }),
        )
        .into_response(),
        Some("tools/call") => {
            match handle_tools_call(runtime, session_id, request.id, request.params).await {
                Ok(response) => response.into_response(),
                Err(err) => err.into_response(),
            }
        }
        Some(other) => {
            json_rpc_error(request.id, -32601, format!("Method not found: {other}")).into_response()
        }
        None => json_rpc_error(request.id, -32600, "missing method".into()).into_response(),
    }
}

async fn handle_tools_call(
    runtime: &crate::code::CodeRuntime,
    session_id: CodeSessionId,
    id: Option<Value>,
    params: Value,
) -> Result<Json<JsonRpcResponse>, ServerError> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != APPROVAL_MCP_TOOL {
        return Ok(json_rpc_ok(
            id,
            json!({
                "content": [{ "type": "text", "text": format!("unknown tool {name}") }],
                "isError": true
            }),
        ));
    }
    // Reached by a single-use capability token minted for this session, not
    // by a bearer credential, so there is no principal to scope against here.
    // The token is the authorization, and it names one session.
    let session = tidebreak_core::db::code::get_session_all_owners(&runtime.db, session_id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("session {session_id} not found")))?;
    if session.lifecycle == CodeSessionLifecycle::Ended {
        return Err(ServerError::conflict_kind(
            "session_ended",
            "session has ended",
        ));
    }
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let request: PermissionPromptRequest = serde_json::from_value(arguments)
        .map_err(|err| ServerError::bad_request(format!("permission prompt arguments: {err}")))?;
    if request.tool_use_id.is_empty() {
        return Err(ServerError::bad_request("tool_use_id is required"));
    }
    let rx = runtime.approvals.park(request.tool_use_id.clone());
    runtime
        .ingest_harness_event(session_id, event_from_prompt_request(&request))
        .await?;
    let decision = rx
        .await
        .map_err(|_| ServerError::internal("permission prompt was dropped before a decision"))?;
    let text = PermissionPromptResponse::from_decision(&decision).as_text_block();
    Ok(json_rpc_ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
    ))
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
