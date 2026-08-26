//! Loopback MCP permission-prompt endpoint for Claude Code 2.1.233.
//!
//! The CLI calls `POST /code/mcp/approval-prompt` as an HTTP MCP server,
//! authenticated with a session-scoped bearer (not the install token). A
//! `tools/call` of `permission_prompt` parks until
//! [`ApprovalBridge::complete`] runs from the decision route.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::sync::oneshot;
use uuid::Uuid;

use tidebreak_core::{CodeApprovalId, CodeSessionId, CodeSessionLifecycle, CodeTurnId, OwnerId};
use tidebreak_harness::claude::approvals::{
    PermissionPromptRequest, PermissionPromptResponse, APPROVAL_MCP_TOOL,
};
use tidebreak_harness::{
    ApprovalCompleter, ApprovalDecision, HarnessApprovalCapability, HarnessApprovalRef,
    HarnessError,
};

use crate::error::ServerError;
use crate::extract::Json;
use crate::state::AppState;

const APPROVAL_WAIT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NativeCallKey {
    session_id: CodeSessionId,
    spawn_epoch: i64,
    call_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalTokenSubject {
    owner: OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
}

struct ParkedApproval {
    session_id: CodeSessionId,
    approval: HarnessApprovalRef,
    sender: oneshot::Sender<ApprovalResolution>,
}

struct ApprovalResolution {
    decision: ApprovalDecision,
    accepted: oneshot::Sender<()>,
}

#[derive(Default)]
struct ApprovalParks {
    by_token: HashMap<String, ParkedApproval>,
    by_call: HashMap<NativeCallKey, String>,
}

/// In-process park for session-scoped permission-prompt MCP calls.
#[derive(Default)]
pub(crate) struct ApprovalBridge {
    tokens: Mutex<HashMap<String, ApprovalTokenSubject>>,
    by_session: Mutex<HashMap<CodeSessionId, String>>,
    parked: Mutex<ApprovalParks>,
}

impl ApprovalBridge {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Mint a worker-scoped token. Every older token and parked call dies.
    pub(crate) fn issue_token(
        &self,
        owner: &OwnerId,
        session_id: CodeSessionId,
        spawn_epoch: i64,
    ) -> String {
        let token = format!("cma_{}", Uuid::new_v4());
        let mut by_session = self.by_session.lock().expect("approval tokens");
        let mut tokens = self.tokens.lock().expect("approval tokens");
        let mut parked = self.parked.lock().expect("approval park");
        Self::revoke_session_locked(&mut by_session, &mut tokens, &mut parked, session_id);
        by_session.insert(session_id, token.clone());
        tokens.insert(
            token.clone(),
            ApprovalTokenSubject {
                owner: owner.clone(),
                session_id,
                spawn_epoch,
            },
        );
        token
    }

    fn subject_for_token(&self, token: &str) -> Option<ApprovalTokenSubject> {
        self.tokens
            .lock()
            .expect("approval tokens")
            .get(token)
            .cloned()
    }

    /// Revoke one worker's bearer and close every native call it parked.
    pub(crate) fn revoke_session(&self, session_id: CodeSessionId) {
        let mut by_session = self.by_session.lock().expect("approval tokens");
        let mut tokens = self.tokens.lock().expect("approval tokens");
        let mut parked = self.parked.lock().expect("approval park");
        Self::revoke_session_locked(&mut by_session, &mut tokens, &mut parked, session_id);
    }

    fn revoke_session_locked(
        by_session: &mut HashMap<CodeSessionId, String>,
        tokens: &mut HashMap<String, ApprovalTokenSubject>,
        parked: &mut ApprovalParks,
        session_id: CodeSessionId,
    ) {
        by_session.remove(&session_id);
        tokens.retain(|_, subject| subject.session_id != session_id);
        let doomed: Vec<String> = parked
            .by_token
            .iter()
            .filter(|(_, entry)| entry.session_id == session_id)
            .map(|(token, _)| token.clone())
            .collect();
        for token in doomed {
            if let Some(entry) = parked.by_token.remove(&token) {
                let capability = entry
                    .approval
                    .capability
                    .as_ref()
                    .expect("parked approval capability");
                parked.by_call.remove(&NativeCallKey {
                    session_id,
                    spawn_epoch: capability.spawn_epoch,
                    call_id: entry.approval.call_id,
                });
            }
        }
    }

    fn park(
        &self,
        bearer: &str,
        subject: &ApprovalTokenSubject,
        approval: HarnessApprovalRef,
    ) -> Result<oneshot::Receiver<ApprovalResolution>, ServerError> {
        let capability = approval.capability.as_ref().ok_or_else(|| {
            ServerError::internal("Claude approval is missing its server capability")
        })?;
        if capability.owner_id != subject.owner.as_str()
            || capability.session_id != subject.session_id.to_string()
            || capability.spawn_epoch != subject.spawn_epoch
        {
            return Err(ServerError::internal(
                "Claude approval capability names a different worker",
            ));
        }
        let key = NativeCallKey {
            session_id: subject.session_id,
            spawn_epoch: subject.spawn_epoch,
            call_id: approval.call_id.clone(),
        };
        let (tx, rx) = oneshot::channel();
        let tokens = self.tokens.lock().expect("approval tokens");
        if tokens.get(bearer) != Some(subject) {
            return Err(ServerError::conflict_kind(
                "approval_worker_replaced",
                "the worker approval token was revoked before the request could park",
            ));
        }
        let mut parked = self.parked.lock().expect("approval park");
        if parked.by_call.contains_key(&key) {
            return Err(ServerError::conflict_kind(
                "duplicate_approval_call",
                format!(
                    "approval call {} is already waiting in session {session_id}",
                    approval.call_id,
                    session_id = subject.session_id,
                ),
            ));
        }
        if parked.by_token.contains_key(&capability.token) {
            return Err(ServerError::internal(
                "Claude approval capability token was reused",
            ));
        }
        parked.by_call.insert(key, capability.token.clone());
        parked.by_token.insert(
            capability.token.clone(),
            ParkedApproval {
                session_id: subject.session_id,
                approval,
                sender: tx,
            },
        );
        Ok(rx)
    }

    fn discard(&self, approval: &HarnessApprovalRef) {
        let Some(capability) = approval.capability.as_ref() else {
            return;
        };
        let mut parked = self.parked.lock().expect("approval park");
        let Some(entry) = parked.by_token.remove(&capability.token) else {
            return;
        };
        parked.by_call.remove(&NativeCallKey {
            session_id: entry.session_id,
            spawn_epoch: capability.spawn_epoch,
            call_id: entry.approval.call_id,
        });
    }
}

#[async_trait]
impl ApprovalCompleter for ApprovalBridge {
    async fn complete(
        &self,
        approval: &HarnessApprovalRef,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessError> {
        let capability = approval.capability.as_ref().ok_or_else(|| {
            HarnessError::ApprovalBindingMismatch(
                "the durable approval has no server capability".into(),
            )
        })?;
        let entry = {
            let mut parked = self.parked.lock().expect("approval park");
            let Some(entry) = parked.by_token.get(&capability.token) else {
                return Err(HarnessError::ApprovalWaiterMissing(
                    capability.approval_id.clone(),
                ));
            };
            if entry.approval != *approval {
                return Err(HarnessError::ApprovalBindingMismatch(
                    capability.approval_id.clone(),
                ));
            }
            let entry = parked
                .by_token
                .remove(&capability.token)
                .expect("checked approval park");
            parked.by_call.remove(&NativeCallKey {
                session_id: entry.session_id,
                spawn_epoch: capability.spawn_epoch,
                call_id: entry.approval.call_id.clone(),
            });
            entry
        };
        let (accepted, acknowledgement) = oneshot::channel();
        entry
            .sender
            .send(ApprovalResolution { decision, accepted })
            .map_err(|_| HarnessError::ApprovalWaiterMissing(capability.approval_id.clone()))?;
        acknowledgement
            .await
            .map_err(|_| HarnessError::ApprovalAcknowledgementLost(capability.approval_id.clone()))
    }
}

fn approval_ref(
    owner: &OwnerId,
    session_id: CodeSessionId,
    turn_id: CodeTurnId,
    spawn_epoch: i64,
    approval_id: CodeApprovalId,
    request: &PermissionPromptRequest,
) -> Result<HarnessApprovalRef, ServerError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|err| ServerError::internal(format!("serialize approval request: {err}")))?;
    let request_sha256 =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes));
    Ok(HarnessApprovalRef {
        call_id: request.tool_use_id.clone(),
        capability: Some(HarnessApprovalCapability {
            token: format!("cap_{}", Uuid::new_v4()),
            owner_id: owner.to_string(),
            approval_id: approval_id.to_string(),
            session_id: session_id.to_string(),
            turn_id: turn_id.to_string(),
            spawn_epoch,
            request_sha256,
        }),
    })
}

fn denied(message: impl Into<String>) -> ApprovalDecision {
    ApprovalDecision::Deny {
        feedback: Some(message.into()),
    }
}

fn prompt_result(id: Option<Value>, decision: &ApprovalDecision) -> Json<JsonRpcResponse> {
    let text = PermissionPromptResponse::from_decision(decision).as_text_block();
    json_rpc_ok(
        id,
        json!({
            "content": [{ "type": "text", "text": text }]
        }),
    )
}

fn deny_parked(
    runtime: &crate::code::CodeRuntime,
    approval: &HarnessApprovalRef,
    receiver: oneshot::Receiver<ApprovalResolution>,
    message: &str,
) -> ApprovalDecision {
    runtime.approvals.discard(approval);
    drop(receiver);
    denied(message)
}

async fn await_decision(
    runtime: &crate::code::CodeRuntime,
    session_id: CodeSessionId,
    approval_id: CodeApprovalId,
    approval: &HarnessApprovalRef,
    receiver: oneshot::Receiver<ApprovalResolution>,
) -> ApprovalDecision {
    match tokio::time::timeout(APPROVAL_WAIT_TIMEOUT, receiver).await {
        Ok(Ok(resolution)) => {
            let _ = resolution.accepted.send(());
            resolution.decision
        }
        Ok(Err(_)) => {
            runtime.approvals.discard(approval);
            let _ = runtime
                .abandon_external_approval(session_id, approval_id)
                .await;
            denied("the approval request closed before Tidebreak received a decision")
        }
        Err(_) => {
            runtime.approvals.discard(approval);
            let _ = runtime
                .abandon_external_approval(session_id, approval_id)
                .await;
            denied("the approval request expired before Tidebreak received a decision")
        }
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
    let Some(subject) = runtime.approvals.subject_for_token(token) else {
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
            match handle_tools_call(runtime, token, &subject, request.id, request.params).await {
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
    bearer: &str,
    subject: &ApprovalTokenSubject,
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
    let session = tidebreak_core::db::code::get_session_all_owners(&runtime.db, subject.session_id)
        .await?
        .ok_or_else(|| {
            ServerError::not_found(format!("session {} not found", subject.session_id))
        })?;
    if session.owner != subject.owner || session.spawn_epoch != subject.spawn_epoch {
        return Err(ServerError::conflict_kind(
            "approval_worker_replaced",
            "the worker that issued this approval token is no longer attached",
        ));
    }
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
    let turn =
        tidebreak_core::db::code::get_open_turn(&runtime.db, &session.owner, subject.session_id)
            .await?
            .ok_or_else(|| {
                ServerError::conflict_kind(
                    "approval_turn_missing",
                    "the session has no running turn for this approval",
                )
            })?;
    let approval_id = CodeApprovalId::new();
    let harness_ref = approval_ref(
        &session.owner,
        subject.session_id,
        turn.id,
        session.spawn_epoch,
        approval_id,
        &request,
    )?;
    let receiver = runtime
        .approvals
        .park(bearer, subject, harness_ref.clone())?;
    let raw = serde_json::to_value(&request)
        .map_err(|err| ServerError::internal(format!("serialize approval request: {err}")))?;
    if let Err(err) = runtime
        .record_external_approval(subject.session_id, approval_id, &harness_ref, &raw)
        .await
    {
        let decision = deny_parked(
            runtime,
            &harness_ref,
            receiver,
            "Tidebreak could not save the approval request",
        );
        tracing::warn!(session = %subject.session_id, approval = %approval_id, error = %err.message(), "code-mode: approval persistence failed");
        return Ok(prompt_result(id, &decision));
    }
    let decision = await_decision(
        runtime,
        subject.session_id,
        approval_id,
        &harness_ref,
        receiver,
    )
    .await;
    Ok(prompt_result(id, &decision))
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

    async fn accept_resolution(
        receiver: oneshot::Receiver<ApprovalResolution>,
    ) -> ApprovalDecision {
        let resolution = receiver.await.expect("parked approval resolution");
        resolution
            .accepted
            .send(())
            .expect("completion is still waiting for acknowledgement");
        resolution.decision
    }

    fn approval(
        owner: &OwnerId,
        session_id: CodeSessionId,
        turn_id: CodeTurnId,
        spawn_epoch: i64,
        approval_id: CodeApprovalId,
        call_id: &str,
        token: &str,
    ) -> HarnessApprovalRef {
        HarnessApprovalRef {
            call_id: call_id.into(),
            capability: Some(HarnessApprovalCapability {
                token: token.into(),
                owner_id: owner.to_string(),
                approval_id: approval_id.to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.to_string(),
                spawn_epoch,
                request_sha256: format!("sha-{approval_id}"),
            }),
        }
    }

    #[tokio::test]
    async fn the_same_native_call_id_is_isolated_between_sessions() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let first_session = CodeSessionId::new();
        let second_session = CodeSessionId::new();
        let first_bearer = bridge.issue_token(&owner, first_session, 1);
        let second_bearer = bridge.issue_token(&owner, second_session, 1);
        let first_subject = bridge.subject_for_token(&first_bearer).unwrap();
        let second_subject = bridge.subject_for_token(&second_bearer).unwrap();
        let first = approval(
            &owner,
            first_session,
            CodeTurnId::new(),
            1,
            CodeApprovalId::new(),
            "toolu_same",
            "cap-first",
        );
        let second = approval(
            &owner,
            second_session,
            CodeTurnId::new(),
            1,
            CodeApprovalId::new(),
            "toolu_same",
            "cap-second",
        );
        let first_rx = bridge
            .park(&first_bearer, &first_subject, first.clone())
            .unwrap();
        let mut second_rx = bridge
            .park(&second_bearer, &second_subject, second.clone())
            .unwrap();

        let (completed, received) = tokio::join!(
            bridge.complete(&first, ApprovalDecision::Approve),
            accept_resolution(first_rx),
        );
        completed.unwrap();
        assert_eq!(received, ApprovalDecision::Approve);
        assert!(second_rx.try_recv().is_err());

        let denied = ApprovalDecision::Deny {
            feedback: Some("not this one".into()),
        };
        let (completed, received) = tokio::join!(
            bridge.complete(&second, denied.clone()),
            accept_resolution(second_rx),
        );
        completed.unwrap();
        assert_eq!(received, denied);
    }

    #[test]
    fn a_duplicate_native_call_id_is_rejected_within_one_worker() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let session_id = CodeSessionId::new();
        let bearer = bridge.issue_token(&owner, session_id, 4);
        let subject = bridge.subject_for_token(&bearer).unwrap();
        let first = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            4,
            CodeApprovalId::new(),
            "toolu_duplicate",
            "cap-first",
        );
        let second = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            4,
            CodeApprovalId::new(),
            "toolu_duplicate",
            "cap-second",
        );
        let _receiver = bridge.park(&bearer, &subject, first).unwrap();

        let error = match bridge.park(&bearer, &subject, second) {
            Ok(_) => panic!("duplicate approval call unexpectedly parked"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), "duplicate_approval_call");
    }

    #[tokio::test]
    async fn completing_a_missing_waiter_fails() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let approval = approval(
            &owner,
            CodeSessionId::new(),
            CodeTurnId::new(),
            1,
            CodeApprovalId::new(),
            "toolu_missing",
            "cap-missing",
        );

        assert!(matches!(
            bridge.complete(&approval, ApprovalDecision::Approve).await,
            Err(HarnessError::ApprovalWaiterMissing(_))
        ));
    }

    #[tokio::test]
    async fn completing_a_closed_waiter_fails() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let session_id = CodeSessionId::new();
        let bearer = bridge.issue_token(&owner, session_id, 2);
        let subject = bridge.subject_for_token(&bearer).unwrap();
        let approval = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            2,
            CodeApprovalId::new(),
            "toolu_closed",
            "cap-closed",
        );
        let receiver = bridge.park(&bearer, &subject, approval.clone()).unwrap();
        drop(receiver);

        assert!(matches!(
            bridge.complete(&approval, ApprovalDecision::Approve).await,
            Err(HarnessError::ApprovalWaiterMissing(_))
        ));
    }

    #[tokio::test]
    async fn completion_waits_until_the_handler_accepts_the_decision() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let session_id = CodeSessionId::new();
        let bearer = bridge.issue_token(&owner, session_id, 2);
        let subject = bridge.subject_for_token(&bearer).unwrap();
        let approval = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            2,
            CodeApprovalId::new(),
            "toolu_ack",
            "cap-ack",
        );
        let receiver = bridge.park(&bearer, &subject, approval.clone()).unwrap();
        let completion = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.complete(&approval, ApprovalDecision::Approve).await }
        });

        tokio::task::yield_now().await;
        assert!(!completion.is_finished());
        let resolution = receiver.await.unwrap();
        assert_eq!(resolution.decision, ApprovalDecision::Approve);
        assert!(!completion.is_finished());
        resolution.accepted.send(()).unwrap();
        completion.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn losing_acknowledgement_after_delivery_is_ambiguous() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let session_id = CodeSessionId::new();
        let bearer = bridge.issue_token(&owner, session_id, 2);
        let subject = bridge.subject_for_token(&bearer).unwrap();
        let approval = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            2,
            CodeApprovalId::new(),
            "toolu_lost_ack",
            "cap-lost-ack",
        );
        let receiver = bridge.park(&bearer, &subject, approval.clone()).unwrap();
        let completion = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.complete(&approval, ApprovalDecision::Approve).await }
        });

        let resolution = receiver.await.unwrap();
        assert_eq!(resolution.decision, ApprovalDecision::Approve);
        drop(resolution.accepted);
        assert!(matches!(
            completion.await.unwrap(),
            Err(HarnessError::ApprovalAcknowledgementLost(_))
        ));
    }

    #[tokio::test]
    async fn token_rotation_closes_only_the_replaced_workers_calls() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let replaced_session = CodeSessionId::new();
        let other_session = CodeSessionId::new();
        let replaced_bearer = bridge.issue_token(&owner, replaced_session, 8);
        let other_bearer = bridge.issue_token(&owner, other_session, 3);
        let replaced_subject = bridge.subject_for_token(&replaced_bearer).unwrap();
        let other_subject = bridge.subject_for_token(&other_bearer).unwrap();
        let replaced = approval(
            &owner,
            replaced_session,
            CodeTurnId::new(),
            8,
            CodeApprovalId::new(),
            "toolu_replaced",
            "cap-replaced",
        );
        let other = approval(
            &owner,
            other_session,
            CodeTurnId::new(),
            3,
            CodeApprovalId::new(),
            "toolu_other",
            "cap-other",
        );
        let replaced_rx = bridge
            .park(&replaced_bearer, &replaced_subject, replaced)
            .unwrap();
        let other_rx = bridge
            .park(&other_bearer, &other_subject, other.clone())
            .unwrap();

        let next_bearer = bridge.issue_token(&owner, replaced_session, 9);
        assert!(bridge.subject_for_token(&replaced_bearer).is_none());
        assert_eq!(
            bridge
                .subject_for_token(&next_bearer)
                .expect("replacement token")
                .spawn_epoch,
            9
        );
        assert!(replaced_rx.await.is_err());

        let (completed, received) = tokio::join!(
            bridge.complete(&other, ApprovalDecision::Approve),
            accept_resolution(other_rx),
        );
        completed.unwrap();
        assert_eq!(received, ApprovalDecision::Approve);
    }

    #[test]
    fn a_token_revoked_after_lookup_cannot_park() {
        let bridge = ApprovalBridge::new();
        let owner = OwnerId::local();
        let session_id = CodeSessionId::new();
        let stale_bearer = bridge.issue_token(&owner, session_id, 1);
        let stale_subject = bridge.subject_for_token(&stale_bearer).unwrap();
        let stale_approval = approval(
            &owner,
            session_id,
            CodeTurnId::new(),
            1,
            CodeApprovalId::new(),
            "toolu_stale",
            "cap-stale",
        );
        let _replacement = bridge.issue_token(&owner, session_id, 2);

        let error = match bridge.park(&stale_bearer, &stale_subject, stale_approval) {
            Ok(_) => panic!("revoked approval bearer unexpectedly parked"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), "approval_worker_replaced");
    }
}
