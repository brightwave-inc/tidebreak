//! Native-only control plane for exact sandbox file delegations.
//!
//! Pending projections reveal only the durable call identity. A successful
//! claim revalidates the immutable child admission and current chat attachment
//! before returning the opaque broker root plus root-relative path. Renderer
//! credentials never reach this module because all routes live behind the
//! native executor middleware.

use axum::extract::State;
use chrono::Duration;
use openwave_core::{
    CallId, ClaimDelegatedFileReadOutcome, HostRootId, ResolveSandboxToolCallOutcome,
    SandboxToolCall, SandboxToolCallStatus, ToolCallResolution, SANDBOX_READ_DELEGATED_FILE_TOOL,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::principal::ClientExecutor;
use crate::state::AppState;

const DELEGATED_FILE_EXECUTION_LEASE: Duration = Duration::seconds(60);
const PENDING_LIMIT: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingDelegatedFileRead {
    pub call_id: CallId,
    pub claimed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedFileClaimDisposition {
    Claimed,
    Existing,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimDelegatedFileReadBody {
    pub lease_token: uuid::Uuid,
}

#[derive(Debug, Serialize)]
pub struct ClaimedDelegatedFileRead {
    pub disposition: DelegatedFileClaimDisposition,
    pub call_id: CallId,
    pub chat_id: openwave_core::ChatId,
    pub root_id: HostRootId,
    pub relative_path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegatedFileLeaseBody {
    pub lease_token: uuid::Uuid,
}

#[derive(Debug, Serialize)]
pub struct DelegatedFileHeartbeat {
    pub extended: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedFileFailureReason {
    NotFound,
    NotUtf8,
    TooLarge,
    PermissionDenied,
    Unavailable,
}

impl DelegatedFileFailureReason {
    const fn error_code(self) -> &'static str {
        match self {
            Self::NotFound => "delegated_file_not_found",
            Self::NotUtf8 => "delegated_file_not_utf8",
            Self::TooLarge => "delegated_file_too_large",
            Self::PermissionDenied => "delegated_file_permission_denied",
            Self::Unavailable => "delegated_file_unavailable",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DelegatedFileResolution {
    Completed { content: String },
    Failed { reason: DelegatedFileFailureReason },
    Cancelled,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveDelegatedFileReadBody {
    pub lease_token: uuid::Uuid,
    pub resolution: DelegatedFileResolution,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedFileResolutionDisposition {
    Resolved,
    Existing,
}

#[derive(Debug, Serialize)]
pub struct ResolvedDelegatedFileRead {
    pub disposition: DelegatedFileResolutionDisposition,
}

pub async fn list_pending_delegated_file_reads(
    State(state): State<AppState>,
    _executor: ClientExecutor,
) -> Result<Json<Vec<PendingDelegatedFileRead>>, ServerError> {
    let pending = state
        .store
        .list_sandbox_tool_call_candidates_named(SANDBOX_READ_DELEGATED_FILE_TOOL, PENDING_LIMIT)
        .await?
        .into_iter()
        .map(|call| PendingDelegatedFileRead {
            call_id: call.id,
            claimed: call.status == SandboxToolCallStatus::Claimed,
        })
        .collect();
    Ok(Json(pending))
}

pub async fn claim_delegated_file_read(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Path(call_id): Path<CallId>,
    Json(body): Json<ClaimDelegatedFileReadBody>,
) -> Result<Json<ClaimedDelegatedFileRead>, ServerError> {
    ensure_token(body.lease_token)?;
    let (disposition, claim) = match state
        .store
        .claim_delegated_file_read(call_id, body.lease_token, DELEGATED_FILE_EXECUTION_LEASE)
        .await?
    {
        ClaimDelegatedFileReadOutcome::Claimed(claim) => {
            (DelegatedFileClaimDisposition::Claimed, claim)
        }
        ClaimDelegatedFileReadOutcome::Existing(claim) => {
            (DelegatedFileClaimDisposition::Existing, claim)
        }
        ClaimDelegatedFileReadOutcome::Unavailable => {
            // Claim may have terminalized revoked or expired work and resumed
            // its sandbox. Polling remains authoritative; this closes the
            // otherwise avoidable worker-scan delay.
            state.agent_run_wake.notify_one();
            return Err(ServerError::conflict(
                "delegated file read is not claimable",
            ));
        }
    };
    Ok(Json(ClaimedDelegatedFileRead {
        disposition,
        call_id: claim.call.id,
        chat_id: claim.call.chat_id,
        root_id: claim.root_id,
        relative_path: claim.relative_path,
    }))
}

pub async fn heartbeat_delegated_file_read(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Path(call_id): Path<CallId>,
    Json(body): Json<DelegatedFileLeaseBody>,
) -> Result<Json<DelegatedFileHeartbeat>, ServerError> {
    ensure_token(body.lease_token)?;
    let Some(_) = state
        .store
        .heartbeat_delegated_file_read(call_id, body.lease_token, DELEGATED_FILE_EXECUTION_LEASE)
        .await?
    else {
        // A failed revalidation may have committed a neutral revocation
        // receipt and made the sandbox runnable again.
        state.agent_run_wake.notify_one();
        return Err(ServerError::conflict(
            "delegated file read lease is not live",
        ));
    };
    Ok(Json(DelegatedFileHeartbeat { extended: true }))
}

pub async fn resolve_delegated_file_read(
    State(state): State<AppState>,
    _executor: ClientExecutor,
    Path(call_id): Path<CallId>,
    Json(body): Json<ResolveDelegatedFileReadBody>,
) -> Result<Json<ResolvedDelegatedFileRead>, ServerError> {
    ensure_token(body.lease_token)?;
    let resolution = canonical_resolution(body.resolution)?;
    let disposition = match state
        .store
        .resolve_delegated_file_read(call_id, body.lease_token, &resolution)
        .await?
    {
        ResolveSandboxToolCallOutcome::Resolved => DelegatedFileResolutionDisposition::Resolved,
        ResolveSandboxToolCallOutcome::Existing => DelegatedFileResolutionDisposition::Existing,
        ResolveSandboxToolCallOutcome::AlreadyTerminal => {
            return Err(ServerError::conflict(
                "delegated file read already has a different result",
            ));
        }
        ResolveSandboxToolCallOutcome::LeaseLost => {
            // The atomic resolution fence can reject supplied content by
            // committing a neutral failure receipt and resuming the sandbox.
            state.agent_run_wake.notify_one();
            return Err(ServerError::conflict(
                "delegated file read is not owned by this lease",
            ));
        }
        ResolveSandboxToolCallOutcome::NotFound => {
            return Err(ServerError::not_found("delegated file read not found"));
        }
    };
    state.agent_run_wake.notify_one();
    Ok(Json(ResolvedDelegatedFileRead { disposition }))
}

fn canonical_resolution(
    resolution: DelegatedFileResolution,
) -> Result<ToolCallResolution, ServerError> {
    let resolution = match resolution {
        DelegatedFileResolution::Completed { content } => {
            if content.contains('\0') {
                return Err(ServerError::bad_request(
                    "delegated file content contains a null byte",
                ));
            }
            ToolCallResolution::Completed {
                result: serde_json::json!({"content": content}).to_string(),
            }
        }
        DelegatedFileResolution::Failed { reason } => ToolCallResolution::Failed {
            result: serde_json::json!({
                "error": "The delegated file could not be read.",
                "code": reason.error_code(),
            })
            .to_string(),
            error_code: reason.error_code().into(),
            error_detail: None,
        },
        DelegatedFileResolution::Cancelled => ToolCallResolution::Cancelled {
            result: serde_json::json!({
                "error": "The delegated file read was cancelled.",
                "code": "cancelled",
            })
            .to_string(),
        },
    };
    if resolution.result().len() > SandboxToolCall::MAX_RESULT_BYTES {
        return Err(ServerError::bad_request(
            "delegated file content exceeds the supported limit",
        ));
    }
    Ok(resolution)
}

fn ensure_token(token: uuid::Uuid) -> Result<(), ServerError> {
    if token.is_nil() {
        return Err(ServerError::bad_request("lease_token must not be nil"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_results_are_bounded_and_hide_native_diagnostics() {
        let empty = canonical_resolution(DelegatedFileResolution::Completed {
            content: String::new(),
        })
        .unwrap_or_else(|_| panic!("empty UTF-8 file should be accepted"));
        assert_eq!(empty.result(), r#"{"content":""}"#);

        let failure = canonical_resolution(DelegatedFileResolution::Failed {
            reason: DelegatedFileFailureReason::PermissionDenied,
        })
        .unwrap_or_else(|_| panic!("fixed failure should be accepted"));
        assert_eq!(
            failure.result(),
            r#"{"code":"delegated_file_permission_denied","error":"The delegated file could not be read."}"#
        );
        let ToolCallResolution::Failed {
            error_code,
            error_detail,
            ..
        } = failure
        else {
            panic!("expected failed resolution");
        };
        assert_eq!(error_code, "delegated_file_permission_denied");
        assert_eq!(error_detail, None);

        let oversized = "\\".repeat(SandboxToolCall::MAX_RESULT_BYTES);
        assert!(
            canonical_resolution(DelegatedFileResolution::Completed { content: oversized })
                .is_err()
        );
        let cancelled = canonical_resolution(DelegatedFileResolution::Cancelled)
            .unwrap_or_else(|_| panic!("fixed cancellation should be accepted"));
        assert!(cancelled.result().contains("cancelled"));
        assert!(!cancelled.result().contains("root"));
        assert!(!cancelled.result().contains("path"));
    }
}
