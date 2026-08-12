//! Durable HTTP control plane for work that executes in a trusted client.
//!
//! Polling is authoritative; event streams are only a latency hint. A client
//! generates a fresh secret lease token before claiming, then retains it across
//! ambiguous HTTP responses and presents it for every heartbeat or resolution.
//! Raw polling, claim, heartbeat, and resolve require the server's native-only
//! executor credential. The renderer-facing pending route returns a closed,
//! presentation-only projection of folder-access requests.

use axum::extract::State;
use serde::{Deserialize, Serialize};

use chrono::{Duration, Utc};
use tidebreak_core::{
    CallId, ChatId, ClaimClientToolCallOutcome, HeartbeatClientToolCallOutcome, OutputWriteMode,
    PermissionMode, RequestFolderAccessArgs, RequestedFolderHint, ResolveToolCallOutcome,
    ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus, TurnId, TurnRunStatus,
    WriteOutputToConnectedFolderArgs, REQUEST_FOLDER_ACCESS_TOOL,
    WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::principal::ClientExecutor;
use crate::scoped_store::ScopedStore;
use crate::state::AppState;

const CLIENT_EXECUTION_LEASE: Duration = Duration::seconds(60);
/// Frozen consent prose. The renderer pins this byte-for-byte so no
/// server-authored text can reach a consent prompt.
pub(crate) const RENDERER_FOLDER_ACCESS_REASON: &str =
    "The assistant needs read access to files outside the folders connected to this conversation.";

/// Caller-owned claim identity. The lease token is a secret capability and
/// must be generated freshly for a new claim attempt.
#[derive(Deserialize)]
pub struct ClaimClientExecution {
    pub executor_id: uuid::Uuid,
    pub lease_token: uuid::Uuid,
}

/// Whether a claim was first installed or recovered after a lost response.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimDisposition {
    Claimed,
    Existing,
}

/// Canonical claimed work plus the secret receipt needed to operate it.
#[derive(Serialize)]
pub struct ClaimedClientExecution {
    pub disposition: ClaimDisposition,
    pub call: ToolCallRecord,
    pub lease_token: uuid::Uuid,
}

/// Secret receipt for extending a live claim.
#[derive(Deserialize)]
pub struct HeartbeatClientExecution {
    pub lease_token: uuid::Uuid,
}

/// Whether a heartbeat advanced the lease or proposed its current expiry.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatDisposition {
    Extended,
    Existing,
}

#[derive(Serialize)]
pub struct ClientExecutionHeartbeat {
    pub disposition: HeartbeatDisposition,
}

/// Terminal client outcome. The model-facing `result` is retained for every
/// variant; failures additionally carry a stable machine code and bounded local
/// diagnostic detail.
#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClientExecutionResolution {
    Completed {
        result: String,
        /// What the executor surfaced, as `{entries, failures}`.
        ///
        /// Unvalidated here on purpose: the store builds the projection from it
        /// against the call's own stored name, so the allowlist and the clamps
        /// are applied where the name is authoritative rather than trusted from
        /// a client that could name any tool. Absent from executors that
        /// predate this field, which simply report no rows.
        #[serde(default)]
        rows: Option<serde_json::Value>,
    },
    Failed {
        result: String,
        error_code: String,
        #[serde(default)]
        error_detail: Option<String>,
    },
    Cancelled {
        result: String,
    },
}

impl ClientExecutionResolution {
    /// The rows the executor reported, if it reported any. Only a completed
    /// call has any — a failure describes work that did not happen.
    fn rows(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Completed { rows, .. } => rows.as_ref(),
            Self::Failed { .. } | Self::Cancelled { .. } => None,
        }
    }

    fn into_core(self) -> ToolCallResolution {
        match self {
            Self::Completed { result, .. } => ToolCallResolution::Completed { result },
            Self::Failed {
                result,
                error_code,
                error_detail,
            } => ToolCallResolution::Failed {
                result,
                error_code,
                error_detail,
            },
            Self::Cancelled { result } => ToolCallResolution::Cancelled { result },
        }
    }
}

#[derive(Deserialize)]
pub struct ResolveClientExecution {
    pub lease_token: uuid::Uuid,
    pub resolution: ClientExecutionResolution,
}

/// Whether this request committed the terminal state or recovered it.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionDisposition {
    Resolved,
    Existing,
}

#[derive(Serialize)]
pub struct ResolvedClientExecution {
    pub disposition: ResolutionDisposition,
}

/// One folder-access request that is safe for an untrusted renderer to present.
///
/// This intentionally omits the canonical tool name and arguments, chat and
/// executor identities, provider metadata, lifecycle details, and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct PendingFolderAccessRequest {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub reason: String,
    pub folder_hint: Option<RequestedFolderHint>,
    pub claimed: bool,
}

/// Renderer-safe write-back approval. Canonical output, root, and destination
/// identities remain native-only; the card can approve or decline this exact
/// call. The mode is carried so the card can name what is being decided —
/// creating a new file reads very differently from destroying an existing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct PendingOutputWritebackRequest {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub mode: OutputWriteMode,
    pub claimed: bool,
}

/// `GET /chats/{id}/client-executions/pending` — renderer-safe consent prompts.
///
/// Unknown, malformed, or non-folder-access client calls are omitted rather
/// than exposing their canonical records across the renderer boundary.
pub async fn list_pending_folder_access_requests(
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<PendingFolderAccessRequest>>, ServerError> {
    store.require_chat(id).await?;
    let requests = store
        .list_pending_client_tool_calls(id)
        .await?
        .into_iter()
        .filter_map(renderer_folder_access_request)
        .collect();
    Ok(Json(requests))
}

/// `GET /chats/{id}/output-writebacks/pending` — write-backs awaiting a decision.
///
/// Replacement always asks. A create asks only where the chat's permission mode
/// says workspace mutations ask; under `Auto` and `Allow` the native executor
/// takes it automatically and it is intentionally omitted from the renderer.
pub async fn list_pending_output_writebacks(
    store: ScopedStore,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<PendingOutputWritebackRequest>>, ServerError> {
    let chat = store.require_chat(id).await?;
    let requests = store
        .list_pending_client_tool_calls(id)
        .await?
        .into_iter()
        .filter_map(|call| renderer_output_writeback_request(call, chat.permission_mode))
        .collect();
    Ok(Json(requests))
}

/// Native-only authoritative pending work used by the trusted executor.
pub async fn list_pending_client_executions_raw(
    store: ScopedStore,
    _executor: ClientExecutor,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<ToolCallRecord>>, ServerError> {
    store.require_chat(id).await?;
    Ok(Json(
        store
            .list_pending_client_tool_calls(id)
            .await?
            .into_iter()
            .filter(|call| call.name != tidebreak_core::ASK_USER_QUESTIONS_TOOL)
            .collect(),
    ))
}

fn renderer_folder_access_request(call: ToolCallRecord) -> Option<PendingFolderAccessRequest> {
    if call.name != REQUEST_FOLDER_ACCESS_TOOL
        || call.execution != ToolCallExecution::Client
        || call.status != ToolCallStatus::Pending
    {
        return None;
    }
    let arguments: RequestFolderAccessArgs = serde_json::from_value(call.arguments).ok()?;
    if !arguments.is_well_formed() {
        return None;
    }
    Some(PendingFolderAccessRequest {
        call_id: call.id,
        turn_id: call.turn_id,
        reason: RENDERER_FOLDER_ACCESS_REASON.to_owned(),
        folder_hint: arguments.folder_hint,
        claimed: call.client_executor_id.is_some(),
    })
}

fn renderer_output_writeback_request(
    call: ToolCallRecord,
    permission_mode: Option<PermissionMode>,
) -> Option<PendingOutputWritebackRequest> {
    if call.name != WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL
        || call.execution != ToolCallExecution::Client
        || call.status != ToolCallStatus::Pending
    {
        return None;
    }
    let arguments: WriteOutputToConnectedFolderArgs =
        serde_json::from_value(call.arguments).ok()?;
    if !arguments.is_well_formed() || !arguments.mode.requires_user_decision(permission_mode) {
        return None;
    }
    Some(PendingOutputWritebackRequest {
        call_id: call.id,
        turn_id: call.turn_id,
        mode: arguments.mode,
        claimed: call.client_executor_id.is_some(),
    })
}

/// `POST .../{call_id}/claim` — atomically acquire or recover one exact claim.
pub async fn claim_client_execution(
    store: ScopedStore,
    _executor: ClientExecutor,
    Path((id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ClaimClientExecution>,
) -> Result<Json<ClaimedClientExecution>, ServerError> {
    store.require_chat(id).await?;
    ensure_non_nil(body.executor_id, "executor_id")?;
    ensure_non_nil(body.lease_token, "lease_token")?;
    let now = Utc::now();
    let outcome = store
        .claim_client_tool_call(
            call_id,
            id,
            body.executor_id,
            body.lease_token,
            now,
            now + CLIENT_EXECUTION_LEASE,
        )
        .await?;
    let (disposition, claim) = match outcome {
        ClaimClientToolCallOutcome::Claimed(claim) => (ClaimDisposition::Claimed, claim),
        ClaimClientToolCallOutcome::Existing(claim) => (ClaimDisposition::Existing, claim),
        ClaimClientToolCallOutcome::Unavailable => {
            return Err(ServerError::conflict(format!(
                "client execution {call_id} is not claimable for chat {id}"
            )));
        }
    };
    Ok(Json(ClaimedClientExecution {
        disposition,
        call: claim.call,
        lease_token: claim.lease_token,
    }))
}

/// `POST .../{call_id}/heartbeat` — renew the exact live lease for 60 seconds.
/// Heartbeats are monotonic liveness updates rather than exact commands: a
/// repeated request can extend the lease again from its new server receive time.
pub async fn heartbeat_client_execution(
    store: ScopedStore,
    _executor: ClientExecutor,
    Path((id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<HeartbeatClientExecution>,
) -> Result<Json<ClientExecutionHeartbeat>, ServerError> {
    store.require_chat(id).await?;
    ensure_non_nil(body.lease_token, "lease_token")?;
    let now = Utc::now();
    let disposition = match store
        .heartbeat_client_tool_call(
            call_id,
            id,
            body.lease_token,
            now,
            now + CLIENT_EXECUTION_LEASE,
        )
        .await?
    {
        HeartbeatClientToolCallOutcome::Extended => HeartbeatDisposition::Extended,
        HeartbeatClientToolCallOutcome::Existing => HeartbeatDisposition::Existing,
        HeartbeatClientToolCallOutcome::LeaseLost => {
            return Err(ServerError::conflict(format!(
                "client execution {call_id} lease is not live"
            )));
        }
    };
    Ok(Json(ClientExecutionHeartbeat { disposition }))
}

/// `POST .../{call_id}/resolve` — terminalize a known native outcome once.
///
/// The exact token can also reconcile a known outcome after lease expiry. Work
/// is never transferred to another executor, and a different payload conflicts
/// with the first committed result.
pub async fn resolve_client_execution(
    State(state): State<AppState>,
    store: ScopedStore,
    _executor: ClientExecutor,
    Path((id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ResolveClientExecution>,
) -> Result<Json<ResolvedClientExecution>, ServerError> {
    store.require_chat(id).await?;
    ensure_non_nil(body.lease_token, "lease_token")?;
    let rows = body.resolution.rows().cloned();
    let resolution = body.resolution.into_core();
    validate_resolution(&resolution)?;
    let now = Utc::now();
    let mut resolution_receipt = store
        .resolve_client_tool_call_and_append_event_with_rows(
            call_id,
            id,
            body.lease_token,
            now,
            &resolution,
            now,
            rows.as_ref(),
        )
        .await?;
    if resolution_receipt.outcome == ResolveToolCallOutcome::LeaseLost {
        resolution_receipt = store
            .resolve_expired_client_tool_call_and_append_event_with_rows(
                call_id,
                id,
                body.lease_token,
                now,
                &resolution,
                now,
                rows.as_ref(),
            )
            .await?;
    }
    let disposition = match resolution_receipt.outcome {
        ResolveToolCallOutcome::Resolved => ResolutionDisposition::Resolved,
        ResolveToolCallOutcome::Existing => ResolutionDisposition::Existing,
        ResolveToolCallOutcome::AlreadyTerminal => {
            return Err(ServerError::conflict(format!(
                "client execution {call_id} already has a different terminal result"
            )));
        }
        ResolveToolCallOutcome::LeaseLost => {
            return Err(ServerError::conflict(format!(
                "client execution {call_id} is not owned by this lease"
            )));
        }
        ResolveToolCallOutcome::NotFound => {
            return Err(ServerError::not_found(format!(
                "client execution {call_id} not found"
            )));
        }
    };
    if let Some(event) = resolution_receipt.terminal_event {
        // Exact retries may publish the same sequence again; WebSocket cursors
        // deduplicate it while this closes the commit-to-live delivery gap.
        let _ = state.events.sender(id).send(event);
    }
    if resolution_receipt
        .turn
        .is_some_and(|turn| turn.status == TurnRunStatus::Resuming)
    {
        state.turn_job_wake.notify_one();
    }
    Ok(Json(ResolvedClientExecution { disposition }))
}

fn ensure_non_nil(value: uuid::Uuid, field: &str) -> Result<(), ServerError> {
    if value.is_nil() {
        return Err(ServerError::bad_request(format!("{field} must not be nil")));
    }
    Ok(())
}

fn validate_resolution(resolution: &ToolCallResolution) -> Result<(), ServerError> {
    let (error_code, error_detail) = match resolution {
        ToolCallResolution::Failed {
            error_code,
            error_detail,
            ..
        } => (Some(error_code.as_str()), error_detail.as_deref()),
        ToolCallResolution::Completed { .. } | ToolCallResolution::Cancelled { .. } => (None, None),
    };
    let invalid = resolution.result().len() > ToolCallRecord::MAX_RESULT_BYTES
        || resolution.result().contains('\0')
        || error_code.is_some_and(|code| {
            code.is_empty()
                || code.len() > ToolCallRecord::MAX_ERROR_CODE_LEN
                || code.contains('\0')
        })
        || error_detail.is_some_and(|detail| {
            detail.is_empty()
                || detail.len() > ToolCallRecord::MAX_ERROR_DETAIL_LEN
                || detail.contains('\0')
        });
    if invalid {
        return Err(ServerError::bad_request(
            "client execution resolution contains invalid or oversized fields",
        ));
    }
    Ok(())
}
