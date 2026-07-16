//! Durable native executor for foreground inspection of already connected folders.
//!
//! The renderer never supplies an execution context, host path, grant, or
//! broker handle. Canonical tool arguments are recovered from the checkpointed
//! server call, while the native executor derives current chat authority before
//! each broker operation.

use std::collections::HashSet;

use openwave_core::{
    validate_list_connected_folders_arguments, validate_list_folder_arguments,
    validate_read_connected_file_arguments, CallId, ListFolderArgs, ReadConnectedFileArgs,
    ToolCallExecution, ToolCallRecord, ToolCallStatus, LIST_CONNECTED_FOLDERS_TOOL,
    LIST_FOLDER_TOOL, READ_CONNECTED_FILE_TOOL,
};
use openwave_host_broker::{
    OperationEnvelope, OperationRequest, OperationResult, PathRequest, RelativePath, RootId,
    PROTOCOL_VERSION,
};
use tauri::Manager;

use crate::host_access::{AuthoritativeContext, HostAccess};

use super::{
    control_plane, control_plane_error, private_receipt_error, FolderOperationPhase,
    FolderOperationReceipt, StoredResolution,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const MAX_DIRECTORY_ENTRIES: usize = 128;
const MAX_RESULT_CONTENT_BYTES: usize = 60 * 1024;
const MAX_FILE_CONTENT_BYTES: usize = 56 * 1024;

/// Recover persisted outcomes, then discover new matching client calls. The
/// loop is deliberately native-owned: no renderer event or user action is an
/// execution authority.
pub(crate) async fn recover_connected_folder_operations(app: tauri::AppHandle) {
    loop {
        let failed = recover_once(&app).await;
        if failed {
            eprintln!("openwave-desktop: connected-folder executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &tauri::AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_operations() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("openwave-desktop: connected-folder receipt recovery failed: {error}");
            return true;
        }
    };
    let recovered_call_ids: HashSet<CallId> =
        receipts.iter().map(|receipt| receipt.call_id).collect();
    let mut failed = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(&state, receipt).await {
            eprintln!("openwave-desktop: connected-folder receipt deferred: {error}");
            failed = true;
        }
    }

    let Some(store) = state.store() else {
        return true;
    };
    let client = match control_plane(&state) {
        Ok(client) => client,
        Err(_) => return true,
    };
    let chats = match store.list_chats().await {
        Ok(chats) => chats,
        Err(_) => return true,
    };
    for chat in chats {
        let calls = match client.pending(chat.id).await {
            Ok(calls) => calls,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for call in calls
            .into_iter()
            .filter(|call| should_discover_call(call, &recovered_call_ids))
        {
            let receipt =
                FolderOperationReceipt::new(chat.id, call.id, state.receipts.executor_id());
            if let Err(error) = execute_receipt(&state, receipt).await {
                eprintln!("openwave-desktop: connected-folder execution deferred: {error}");
                failed = true;
            }
        }
    }
    failed
}

fn should_discover_call(call: &ToolCallRecord, recovered_call_ids: &HashSet<CallId>) -> bool {
    !recovered_call_ids.contains(&call.id) && is_connected_folder_call(call)
}

async fn execute_receipt(
    state: &HostAccess,
    mut receipt: FolderOperationReceipt,
) -> Result<(), String> {
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }
    if dispatch_is_ambiguous(&receipt) {
        return terminalize_interrupted_dispatch(state, &mut receipt).await;
    }

    let context = state.context(receipt.chat_id.0).await?;

    // Persist the chosen lease token before claiming. If a response is lost,
    // recovery retries the exact claim rather than leaving a live lease with no
    // local recovery authority.
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    let client = control_plane(state)?;
    let claim = match client
        .claim(
            receipt.chat_id,
            receipt.call_id,
            receipt.executor_id,
            receipt.lease_token,
        )
        .await
    {
        Ok(claim) => claim,
        Err(error) if error.is_conflict() => {
            return recover_after_claim_conflict(state, &mut receipt).await;
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    if claim.call.chat_id != receipt.chat_id
        || claim.call.id != receipt.call_id
        || claim.lease_token != receipt.lease_token
        || claim.call.client_executor_id != Some(receipt.executor_id)
        || !is_connected_folder_call(&claim.call)
    {
        return Err("local control plane returned an invalid connected-folder request".to_owned());
    }

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    // This is the final durable fence before host I/O. If the process stops
    // afterwards, a read may have reached the broker, so recovery must publish
    // the safe interrupted result rather than issue another request.
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    let resolution = execute_operation(state, context, &claim.call).await;
    receipt.resolution = Some(resolution);
    state
        .receipts
        .save_operation(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        &receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

fn dispatch_is_ambiguous(receipt: &FolderOperationReceipt) -> bool {
    receipt.phase == FolderOperationPhase::DispatchStarted && receipt.resolution.is_none()
}

async fn terminalize_interrupted_dispatch(
    state: &HostAccess,
    receipt: &mut FolderOperationReceipt,
) -> Result<(), String> {
    receipt.resolution = Some(interrupted_resolution());
    state
        .receipts
        .save_operation(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(
        state,
        receipt,
        receipt.resolution.as_ref().expect("stored above"),
    )
    .await
}

/// A read may have reached the broker before a desktop process died, but the
/// broker deliberately has no read-result receipt to replay. If the persisted
/// client claim can no longer be recovered, terminalize with the same exact
/// lease token instead of issuing another ambiguous read. The server accepts
/// that token through its expired-claim resolution path only after the lease is
/// actually expired.
async fn recover_after_claim_conflict(
    state: &HostAccess,
    receipt: &mut FolderOperationReceipt,
) -> Result<(), String> {
    let client = control_plane(state)?;
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        return state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error);
    };
    if call.chat_id != receipt.chat_id || !is_connected_folder_call(&call) {
        return Err("local control plane returned an invalid connected-folder request".to_owned());
    }
    if call.client_executor_id != Some(receipt.executor_id) {
        return state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error);
    }

    terminalize_interrupted_dispatch(state, receipt).await
}

async fn execute_operation(
    state: &HostAccess,
    context: AuthoritativeContext,
    call: &ToolCallRecord,
) -> StoredResolution {
    let request = match broker_request(call) {
        Ok(request) => request,
        Err(()) => return unavailable("invalid_request", "The folder request was not available."),
    };
    let result = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: openwave_host_broker::RequestId::new(),
            context: context.execution,
            request,
        })
        .await;
    match result {
        Ok(result) => serialize_result(result),
        // Broker transport errors are intentionally not surfaced to the model.
        // Authorization is rechecked by the broker before bytes are released.
        Err(_) => unavailable(
            "folder_unavailable",
            "That connected folder is no longer available to this conversation. You can ask the user to connect a folder again.",
        ),
    }
}

fn broker_request(call: &ToolCallRecord) -> Result<OperationRequest, ()> {
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => Ok(OperationRequest::ListRoots),
        LIST_FOLDER_TOOL => {
            let args: ListFolderArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let root_id = RootId::from_uuid(args.root_id).map_err(|_| ())?;
            let path = RelativePath::parse(&args.path).map_err(|_| ())?;
            Ok(OperationRequest::ListDirectory(PathRequest {
                root_id,
                path,
            }))
        }
        READ_CONNECTED_FILE_TOOL => {
            let args: ReadConnectedFileArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|_| ())?;
            let root_id = RootId::from_uuid(args.root_id).map_err(|_| ())?;
            let path = RelativePath::parse(&args.path).map_err(|_| ())?;
            if path.is_root() {
                return Err(());
            }
            Ok(OperationRequest::ReadFile(PathRequest { root_id, path }))
        }
        _ => Err(()),
    }
}

fn serialize_result(result: OperationResult) -> StoredResolution {
    let result = match result {
        OperationResult::ListRoots { roots } => serde_json::json!({
            "status": "ok",
            "folders": roots.into_iter().take(MAX_DIRECTORY_ENTRIES).map(|root| serde_json::json!({
                "root_id": root.root_id,
                "display_name": root.display_name,
            })).collect::<Vec<_>>(),
        }),
        OperationResult::ListDirectory { entries } => serde_json::json!({
            "status": "ok",
            "entries": entries.into_iter().take(MAX_DIRECTORY_ENTRIES).map(|entry| serde_json::json!({
                "name": entry.name,
                "kind": entry.kind,
            })).collect::<Vec<_>>(),
        }),
        OperationResult::ReadFile(file) => {
            let (content, truncated) = truncate_utf8(&file.content, MAX_FILE_CONTENT_BYTES);
            serde_json::json!({
                "status": "ok",
                "content": content,
                "truncated": truncated,
            })
        }
        _ => {
            return unavailable(
                "unsupported_result",
                "The connected-folder operation is not available.",
            )
        }
    };
    match serde_json::to_string(&result) {
        Ok(result) if result.len() <= MAX_RESULT_CONTENT_BYTES => {
            StoredResolution::Completed { result }
        }
        _ => unavailable(
            "result_too_large",
            "The connected-folder result was too large to return.",
        ),
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn unavailable(code: &str, message: &str) -> StoredResolution {
    StoredResolution::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

fn interrupted_resolution() -> StoredResolution {
    unavailable(
        "folder_operation_interrupted",
        "The folder operation could not be safely resumed after an interruption. Please try again.",
    )
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &FolderOperationReceipt,
    resolution: &StoredResolution,
) -> Result<(), String> {
    let client = control_plane(state)?;
    match client
        .resolve(
            receipt.chat_id,
            receipt.call_id,
            receipt.lease_token,
            resolution,
        )
        .await
    {
        Ok(()) => state
            .receipts
            .remove_operation(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if pending {
                Err("connected-folder result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_operation(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

fn is_connected_folder_call(call: &ToolCallRecord) -> bool {
    if call.execution != ToolCallExecution::Client || call.status != ToolCallStatus::Pending {
        return false;
    }
    match call.name.as_str() {
        LIST_CONNECTED_FOLDERS_TOOL => validate_list_connected_folders_arguments(&call.arguments),
        LIST_FOLDER_TOOL => validate_list_folder_arguments(&call.arguments),
        READ_CONNECTED_FILE_TOOL => validate_read_connected_file_arguments(&call.arguments),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openwave_core::ChatId;

    #[test]
    fn results_are_bounded_and_do_not_include_host_error_detail() {
        let result = serialize_result(OperationResult::ReadFile(
            openwave_host_broker::ReadFileResult {
                content: "x".repeat(MAX_FILE_CONTENT_BYTES + 1),
                bytes: MAX_FILE_CONTENT_BYTES + 1,
            },
        ));
        let StoredResolution::Completed { result } = result else {
            panic!("expected bounded result");
        };
        assert!(result.len() <= MAX_RESULT_CONTENT_BYTES);
        assert!(result.contains("\"truncated\":true"));
    }

    #[test]
    fn native_executor_accepts_only_valid_foreground_folder_calls() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: openwave_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: LIST_FOLDER_TOOL.into(),
            arguments: serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "notes" }),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        };
        assert!(is_connected_folder_call(&call));
        let mut forged = call;
        forged.arguments =
            serde_json::json!({ "root_id": uuid::Uuid::new_v4(), "path": "../secret" });
        assert!(!is_connected_folder_call(&forged));
    }

    #[test]
    fn pending_discovery_never_replaces_a_recovered_operation_receipt() {
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: openwave_core::TurnId::new(),
            provider_id: "tool-1".into(),
            name: LIST_CONNECTED_FOLDERS_TOOL.into(),
            arguments: serde_json::json!({}),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: None,
            error_detail: None,
            client_executor_id: None,
            client_lease_expires_at: None,
            created_at: chrono::Utc::now(),
            resolved_at: None,
        };
        assert!(should_discover_call(&call, &HashSet::new()));
        assert!(!should_discover_call(&call, &HashSet::from([call.id])));
    }

    #[test]
    fn interrupted_claims_are_terminalized_without_replaying_the_read() {
        let StoredResolution::Failed {
            result,
            error_code,
            error_detail,
        } = interrupted_resolution()
        else {
            panic!("interrupted folder work must have a stable terminal result");
        };
        assert_eq!(error_code, "folder_operation_interrupted");
        assert!(error_detail.is_none());
        assert!(result.contains("could not be safely resumed"));
        assert!(!result.contains("/Users/"));
    }

    #[test]
    fn dispatch_started_receipt_is_terminalized_even_with_a_live_lease() {
        let mut receipt =
            FolderOperationReceipt::new(ChatId::new(), CallId::new(), uuid::Uuid::new_v4());
        assert_eq!(receipt.phase, FolderOperationPhase::NotStarted);
        assert!(!dispatch_is_ambiguous(&receipt));
        receipt.phase = FolderOperationPhase::DispatchStarted;
        // Recovery checks this persisted phase before claiming, so it cannot
        // run a second read while the original lease still remains live.
        assert!(dispatch_is_ambiguous(&receipt));
    }
}
