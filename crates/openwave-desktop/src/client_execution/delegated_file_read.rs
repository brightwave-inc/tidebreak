//! Native executor for exact files delegated to depth-one sandbox agents.
//!
//! Discovery reveals only a call identity. The native claim supplies the
//! server-owned root/path pair after revalidating the immutable admission and
//! current attachment. Neither value is persisted or exposed to the renderer.

use std::collections::HashSet;

use openwave_core::CallId;
use openwave_host_broker::{
    ErrorCode, OperationEnvelope, OperationRequest, OperationResult, PathRequest, RelativePath,
    RootId, PROTOCOL_VERSION,
};
use tauri::Manager;

use crate::{broker::BrokerClientError, host_access::HostAccess};

use super::{
    control_plane, control_plane_error, delegated_file_content_fits_server, private_receipt_error,
    DelegatedFileFailureReason, DelegatedFileReadReceipt, DelegatedFileResolution,
    FolderOperationPhase,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimConflictDisposition {
    Defer,
    Retire,
}

pub(crate) async fn recover_delegated_file_read(app: tauri::AppHandle) {
    loop {
        if recover_once(&app).await {
            eprintln!("openwave-desktop: delegated-file executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &tauri::AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_delegated_file_reads() {
        Ok(receipts) => receipts,
        Err(_) => return true,
    };
    let recovered: HashSet<CallId> = receipts.iter().map(|receipt| receipt.call_id).collect();
    let mut failed = false;
    for receipt in receipts {
        if execute_receipt(&state, receipt).await.is_err() {
            failed = true;
        }
    }

    let client = match control_plane(&state) {
        Ok(client) => client,
        Err(_) => return true,
    };
    let pending = match client.pending_delegated_file_reads().await {
        Ok(pending) => pending,
        Err(_) => return true,
    };
    for candidate in pending
        .into_iter()
        .filter(|candidate| should_discover(candidate.call_id, &recovered))
    {
        let receipt =
            DelegatedFileReadReceipt::new(candidate.call_id, state.receipts.executor_id());
        if state.receipts.save_delegated_file_read(&receipt).is_err()
            || execute_receipt(&state, receipt).await.is_err()
        {
            failed = true;
        }
    }
    failed
}

async fn execute_receipt(
    state: &HostAccess,
    mut receipt: DelegatedFileReadReceipt,
) -> Result<(), String> {
    if receipt.executor_id != state.receipts.executor_id() {
        return Err("delegated-file receipt has the wrong native executor".to_owned());
    }
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }
    if receipt.phase == FolderOperationPhase::DispatchStarted {
        receipt.resolution = Some(interrupted_resolution());
        state
            .receipts
            .save_delegated_file_read(&receipt)
            .map_err(private_receipt_error)?;
        return publish_resolution(
            state,
            &receipt,
            receipt.resolution.as_ref().expect("stored above"),
        )
        .await;
    }

    // The receipt and exact lease exist durably before the claim. Losing the
    // claim response therefore never loses the only authority that may recover
    // it, and no host I/O has happened yet.
    state
        .receipts
        .save_delegated_file_read(&receipt)
        .map_err(private_receipt_error)?;
    let client = control_plane(state)?;
    let claim = match client
        .claim_delegated_file_read(receipt.call_id, receipt.lease_token)
        .await
    {
        Ok(claim) => claim,
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending_delegated_file_reads()
                .await
                .map_err(control_plane_error)?;
            match claim_conflict_disposition(receipt.call_id, &pending) {
                ClaimConflictDisposition::Defer => {
                    return Err("delegated-file claim could not be recovered yet".to_owned());
                }
                ClaimConflictDisposition::Retire => {}
            }
            return state
                .receipts
                .remove_delegated_file_read(receipt.call_id)
                .map_err(private_receipt_error);
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    if claim.call_id != receipt.call_id {
        return Err("local control plane returned an invalid delegated-file claim".to_owned());
    }
    let _ = claim.disposition;

    // Resolve all native inputs before entering the no-replay dispatch fence.
    let context = match state.context(claim.chat_id.0).await {
        Ok(context) => context,
        Err(_) => return terminalize_without_dispatch(state, &mut receipt).await,
    };
    let (root_id, relative_path) = match broker_resource(claim.root_id, &claim.relative_path) {
        Ok(resource) => resource,
        Err(()) => return terminalize_without_dispatch(state, &mut receipt).await,
    };
    let operation = OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: openwave_host_broker::RequestId::new(),
        context: context.execution,
        request: OperationRequest::ReadFile(PathRequest {
            root_id,
            path: relative_path,
        }),
    };

    // Persist ambiguity before the final authority check. A crash from this
    // point onward terminalizes the call and never replays a possible read.
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_delegated_file_read(&receipt)
        .map_err(private_receipt_error)?;
    client
        .heartbeat_delegated_file_read(receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;

    // No await that can alter product authority sits between the specialized
    // heartbeat/revalidation and broker admission.
    let resolution = match state.broker.operation(operation).await {
        Ok(OperationResult::ReadFile(file)) => content_resolution(file.content),
        Ok(_) => failed(DelegatedFileFailureReason::Unavailable),
        Err(error) => broker_failure(&error),
    };
    receipt.set_bounded_resolution(resolution);
    state
        .receipts
        .save_delegated_file_read(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        &receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

async fn terminalize_without_dispatch(
    state: &HostAccess,
    receipt: &mut DelegatedFileReadReceipt,
) -> Result<(), String> {
    // No broker operation was attempted. Reuse the conservative terminal phase
    // so a crash can only recover toward a neutral result, never toward I/O.
    receipt.phase = FolderOperationPhase::DispatchStarted;
    receipt.set_bounded_resolution(failed(DelegatedFileFailureReason::Unavailable));
    state
        .receipts
        .save_delegated_file_read(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

fn broker_resource(
    root_id: openwave_core::HostRootId,
    relative_path: &str,
) -> Result<(RootId, RelativePath), ()> {
    let root_id = RootId::from_uuid(*root_id.as_uuid()).map_err(|_| ())?;
    let relative_path = RelativePath::parse(relative_path).map_err(|_| ())?;
    if relative_path.is_root() {
        return Err(());
    }
    Ok((root_id, relative_path))
}

fn should_discover(call_id: CallId, recovered: &HashSet<CallId>) -> bool {
    !recovered.contains(&call_id)
}

fn claim_conflict_disposition(
    call_id: CallId,
    pending: &[super::control_plane::PendingDelegatedFileRead],
) -> ClaimConflictDisposition {
    if pending.iter().any(|candidate| candidate.call_id == call_id) {
        ClaimConflictDisposition::Defer
    } else {
        ClaimConflictDisposition::Retire
    }
}

fn content_resolution(content: String) -> DelegatedFileResolution {
    if content.contains('\0') {
        return failed(DelegatedFileFailureReason::NotUtf8);
    }
    let resolution = DelegatedFileResolution::Completed { content };
    let server_result_fits = match &resolution {
        DelegatedFileResolution::Completed { content } => {
            delegated_file_content_fits_server(content)
        }
        _ => unreachable!(),
    };
    if server_result_fits {
        resolution
    } else {
        failed(DelegatedFileFailureReason::TooLarge)
    }
}

fn broker_failure(error: &BrokerClientError) -> DelegatedFileResolution {
    let reason = match error {
        BrokerClientError::Broker {
            code: ErrorCode::Denied | ErrorCode::InvalidRoot,
            ..
        } => DelegatedFileFailureReason::PermissionDenied,
        BrokerClientError::Broker {
            code: ErrorCode::TooLarge,
            ..
        } => DelegatedFileFailureReason::TooLarge,
        BrokerClientError::Broker {
            code: ErrorCode::UnsupportedContent,
            ..
        } => DelegatedFileFailureReason::NotUtf8,
        _ => DelegatedFileFailureReason::Unavailable,
    };
    failed(reason)
}

fn failed(reason: DelegatedFileFailureReason) -> DelegatedFileResolution {
    DelegatedFileResolution::Failed { reason }
}

fn interrupted_resolution() -> DelegatedFileResolution {
    failed(DelegatedFileFailureReason::Unavailable)
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &DelegatedFileReadReceipt,
    resolution: &DelegatedFileResolution,
) -> Result<(), String> {
    let client = control_plane(state)?;
    match client
        .resolve_delegated_file_read(receipt.call_id, receipt.lease_token, resolution)
        .await
    {
        Ok(()) => state
            .receipts
            .remove_delegated_file_read(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            // Specialized resolve revalidates attachment authority. If a
            // detach terminalized the call while bytes were in flight, the
            // content is discarded here and the private receipt is retired.
            let still_pending = client
                .pending_delegated_file_reads()
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|candidate| candidate.call_id == receipt.call_id);
            if still_pending {
                Err("delegated-file result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_delegated_file_read(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_and_failure_results_are_bounded_and_path_free() {
        assert_eq!(
            content_resolution("x".repeat(openwave_core::SandboxToolCall::MAX_RESULT_BYTES)),
            failed(DelegatedFileFailureReason::TooLarge)
        );
        assert_eq!(
            content_resolution("contains\0null".to_owned()),
            failed(DelegatedFileFailureReason::NotUtf8)
        );
        let encoded = serde_json::to_string(&interrupted_resolution()).unwrap();
        assert!(!encoded.contains("/Users/"));
        assert!(!encoded.contains("relative_path"));
    }

    #[test]
    fn broker_failures_map_to_closed_safe_categories() {
        let denied = BrokerClientError::Broker {
            code: ErrorCode::Denied,
            message: "/private/secret".to_owned(),
            retryable: false,
        };
        assert_eq!(
            broker_failure(&denied),
            failed(DelegatedFileFailureReason::PermissionDenied)
        );
        assert!(!serde_json::to_string(&broker_failure(&denied))
            .unwrap()
            .contains("secret"));
    }

    #[test]
    fn dispatch_fence_forces_terminal_recovery_without_another_read() {
        let mut receipt = DelegatedFileReadReceipt::new(CallId::new(), uuid::Uuid::new_v4());
        assert_eq!(receipt.phase, FolderOperationPhase::NotStarted);
        receipt.phase = FolderOperationPhase::DispatchStarted;
        assert!(receipt.resolution.is_none());
    }

    #[test]
    fn claimed_candidates_are_recovered_instead_of_ignored() {
        let candidate = super::super::control_plane::PendingDelegatedFileRead {
            call_id: CallId::new(),
            claimed: true,
        };
        let recovered = HashSet::new();
        assert!(should_discover(candidate.call_id, &recovered));
        // Discovery deliberately does not filter on `claimed`: attempting the
        // specialized claim terminalizes an expired orphan without dispatch.
        assert!(candidate.claimed);
        assert_eq!(
            claim_conflict_disposition(candidate.call_id, &[]),
            ClaimConflictDisposition::Retire
        );
        assert_eq!(
            claim_conflict_disposition(candidate.call_id, &[candidate]),
            ClaimConflictDisposition::Defer
        );
    }

    #[test]
    fn exact_server_and_private_receipt_bounds_cover_json_escaping() {
        let control_heavy = "\u{0001}".repeat(64 * 1024);
        let resolution = content_resolution(control_heavy);
        let mut receipt = DelegatedFileReadReceipt::new(CallId::new(), uuid::Uuid::new_v4());
        receipt.phase = FolderOperationPhase::DispatchStarted;
        receipt.set_bounded_resolution(resolution);
        let encoded = serde_json::to_vec(&receipt).unwrap();
        assert!(encoded.len() <= 128 * 1024);
        assert_eq!(
            receipt.resolution,
            Some(failed(DelegatedFileFailureReason::TooLarge))
        );

        let ascii = content_resolution("x".repeat(64 * 1024));
        if let DelegatedFileResolution::Completed { content } = &ascii {
            let server = serde_json::json!({"content": content}).to_string();
            assert!(server.len() <= openwave_core::SandboxToolCall::MAX_RESULT_BYTES);
        }
    }

    #[test]
    fn broker_incompatible_delegated_paths_terminalize_neutrally() {
        let root_id = openwave_core::HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
        for path in ["report?.md", "CON", "report. "] {
            assert!(broker_resource(root_id, path).is_err(), "{path}");
        }
        assert!(broker_resource(root_id, "reports/summary.md").is_ok());
        let resolution = failed(DelegatedFileFailureReason::Unavailable);
        let encoded = serde_json::to_string(&resolution).unwrap();
        assert!(!encoded.contains("report?"));
        assert!(!encoded.contains("CON"));
        assert!(!encoded.contains(root_id.as_uuid().to_string().as_str()));
    }
}
