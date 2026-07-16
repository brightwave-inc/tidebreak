//! Native owner of durable client-executed folder consent.
//!
//! The renderer discovers canonical pending requests, but never receives the
//! claim token, picker result, or broker control surface. This module keeps
//! those values together and persists enough private state to recover an exact
//! outcome after a desktop or transport failure.

use std::path::PathBuf;

use openwave_core::{
    validate_request_folder_access_arguments, CallId, ChatId, RequestFolderAccessArgs,
    RequestFolderAccessResult, RequestedFolderCapability, RequestedFolderHint, ToolCallExecution,
    ToolCallRecord, ToolCallStatus, REQUEST_FOLDER_ACCESS_TOOL,
};
use openwave_host_broker::{
    ConsentMethod, ControlRequest, ControlResult, LookupRegisterRootReceiptRequest, OperationId,
    RegisterRootReceipt, RegisterRootRequest,
};
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::host_access::{pick_folder, AuthoritativeContext, HostAccess};

mod control_plane;
pub(crate) mod folder_operations;
mod receipt_store;

pub(crate) use control_plane::ControlPlaneClient;
use control_plane::ControlPlaneError;
pub(crate) use receipt_store::ReceiptStore;
use receipt_store::{
    FolderAccessIntent, FolderAccessReceipt, FolderOperationPhase, FolderOperationReceipt,
    RegistrationPhase, StoredResolution,
};

const RECOVERY_IDLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const RECOVERY_MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecutionMode {
    Interactive,
    Recovery,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FolderAccessDecision {
    Allow,
    Decline,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResolveFolderAccessRequest {
    chat_id: Uuid,
    call_id: Uuid,
    decision: FolderAccessDecision,
}

#[tauri::command]
pub(crate) async fn resolve_folder_access_request(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ResolveFolderAccessRequest,
) -> Result<(), String> {
    if request.chat_id.is_nil() || request.call_id.is_nil() {
        return Err("invalid folder-access request identity".to_owned());
    }
    let _exclusive = state
        .picker
        .try_lock()
        .map_err(|_| "another native folder request is already active".to_owned())?;
    let chat_id = ChatId::from(request.chat_id);
    let call_id = CallId::from(request.call_id);
    if let Some(receipt) = state
        .receipts
        .load_all()
        .map_err(private_receipt_error)?
        .into_iter()
        .find(|receipt| receipt.call_id == call_id)
    {
        if receipt.chat_id != chat_id {
            return Err("folder-access recovery receipt has the wrong conversation".to_owned());
        }
        return execute_receipt(&state, receipt, ExecutionMode::Recovery).await;
    }

    let _ = state.context(request.chat_id).await?;
    let client = control_plane(&state)?;
    let pending = client.pending(chat_id).await.map_err(control_plane_error)?;
    let call = pending
        .into_iter()
        .find(|call| call.id == call_id)
        .ok_or_else(|| "folder-access request is no longer pending".to_owned())?;
    let arguments = validate_canonical_call(&call, chat_id, call_id)?;
    if call.client_executor_id.is_some() {
        return Err("folder-access request is already being handled".to_owned());
    }

    let intent = match request.decision {
        FolderAccessDecision::Decline => FolderAccessIntent::Decline,
        FolderAccessDecision::Allow => {
            let starting_directory = picker_start(
                app.path().document_dir().ok(),
                app.path().download_dir().ok(),
                arguments.folder_hint,
            );
            match pick_folder(&app, starting_directory).await? {
                Some(path) if path.is_absolute() => FolderAccessIntent::Selected { path },
                Some(_) => return Err("the folder picker returned an invalid path".to_owned()),
                None => FolderAccessIntent::Decline,
            }
        }
    };
    let receipt = FolderAccessReceipt::new(chat_id, call_id, state.receipts.executor_id(), intent);
    state
        .receipts
        .save(&receipt)
        .map_err(private_receipt_error)?;
    execute_receipt(&state, receipt, ExecutionMode::Interactive).await
}

pub(crate) async fn recover_folder_access_receipts(app: AppHandle) {
    let mut backoff = RECOVERY_IDLE_INTERVAL;
    loop {
        let had_failure = recover_folder_access_receipts_once(&app).await;
        let delay = if had_failure {
            let delay = backoff;
            backoff = backoff.saturating_mul(2).min(RECOVERY_MAX_BACKOFF);
            delay
        } else {
            backoff = RECOVERY_IDLE_INTERVAL;
            RECOVERY_IDLE_INTERVAL
        };
        tokio::time::sleep(delay).await;
    }
}

async fn recover_folder_access_receipts_once(app: &AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let receipts = match state.receipts.load_all() {
        Ok(receipts) => receipts,
        Err(error) => {
            eprintln!("openwave-desktop: private client-execution recovery failed: {error}");
            return true;
        }
    };
    let _exclusive = state.picker.lock().await;
    let mut had_failure = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(&state, receipt, ExecutionMode::Recovery).await {
            eprintln!("openwave-desktop: client-execution recovery deferred: {error}");
            had_failure = true;
        }
    }
    had_failure
}

async fn execute_receipt(
    state: &HostAccess,
    mut receipt: FolderAccessReceipt,
    mode: ExecutionMode,
) -> Result<(), String> {
    let context = state.context(receipt.chat_id.0).await?;
    if let Some(resolution) = receipt.resolution.clone() {
        return publish_resolution(state, &receipt, &resolution).await;
    }

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
            return recover_after_claim_conflict(state, receipt, context).await;
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    if claim.lease_token != receipt.lease_token
        || claim.call.client_executor_id != Some(receipt.executor_id)
        || validate_canonical_call(&claim.call, receipt.chat_id, receipt.call_id).is_err()
    {
        return Err("local control plane returned a mismatched client claim".to_owned());
    }

    let resolution = match receipt.intent.clone() {
        FolderAccessIntent::Decline => declined_resolution()?,
        FolderAccessIntent::Selected { path } => {
            if should_start_registration(mode, receipt.registration_phase) {
                match client
                    .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
                    .await
                {
                    Ok(()) => {
                        let dispatch_deadline =
                            tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
                        receipt.registration_phase = RegistrationPhase::Attempted;
                        state
                            .receipts
                            .save(&receipt)
                            .map_err(private_receipt_error)?;
                        register_selected_folder(state, &receipt, context, path, dispatch_deadline)
                            .await?
                    }
                    Err(_) => recover_broker_outcome(state, &receipt, context).await?,
                }
            } else {
                recover_broker_outcome(state, &receipt, context).await?
            }
        }
    };
    receipt.resolution = Some(resolution.clone());
    state
        .receipts
        .save(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(state, &receipt, &resolution).await
}

fn should_start_registration(mode: ExecutionMode, phase: RegistrationPhase) -> bool {
    mode == ExecutionMode::Interactive && phase == RegistrationPhase::NotStarted
}

async fn recover_after_claim_conflict(
    state: &HostAccess,
    mut receipt: FolderAccessReceipt,
    context: AuthoritativeContext,
) -> Result<(), String> {
    let client = control_plane(state)?;
    let pending = client
        .pending(receipt.chat_id)
        .await
        .map_err(control_plane_error)?;
    let Some(call) = pending.into_iter().find(|call| call.id == receipt.call_id) else {
        state
            .receipts
            .remove(receipt.call_id)
            .map_err(private_receipt_error)?;
        return Ok(());
    };
    validate_canonical_call(&call, receipt.chat_id, receipt.call_id)?;
    if call.client_executor_id != Some(receipt.executor_id) {
        if call.client_executor_id.is_none() {
            return Err("folder-access claim could not be recovered yet".to_owned());
        }
        state
            .receipts
            .remove(receipt.call_id)
            .map_err(private_receipt_error)?;
        return Err("folder-access request is owned by another desktop".to_owned());
    }

    let resolution = match receipt.intent {
        FolderAccessIntent::Decline => declined_resolution()?,
        FolderAccessIntent::Selected { .. } => {
            recover_broker_outcome(state, &receipt, context).await?
        }
    };
    receipt.resolution = Some(resolution.clone());
    state
        .receipts
        .save(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(state, &receipt, &resolution).await
}

async fn register_selected_folder(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    context: AuthoritativeContext,
    path: PathBuf,
    dispatch_deadline: tokio::time::Instant,
) -> Result<StoredResolution, String> {
    let operation_id = OperationId::from_uuid(receipt.call_id.0)
        .map_err(|_| "invalid folder-access operation identity".to_owned())?;
    let first = state
        .broker
        .control_without_retry(
            ControlRequest::RegisterRoot(RegisterRootRequest {
                operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
                path,
                consent_method: ConsentMethod::FolderPicker,
            }),
            dispatch_deadline,
        )
        .await;
    match first {
        Ok(ControlResult::RegisterRoot(_)) | Err(_) => {
            // The receipt query below is the authoritative post-effect check.
            // It also catches a revocation that raced the response.
        }
        Ok(_) => return Err("host broker returned an unexpected response".to_owned()),
    }
    recover_broker_outcome(state, receipt, context).await
}

async fn recover_broker_outcome(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    context: AuthoritativeContext,
) -> Result<StoredResolution, String> {
    let operation_id = OperationId::from_uuid(receipt.call_id.0)
        .map_err(|_| "invalid folder-access operation identity".to_owned())?;
    let result = state
        .broker
        .control(ControlRequest::LookupRegisterRootReceipt(
            LookupRegisterRootReceiptRequest {
                operation_id,
                subject: context.subject,
                conversation_id: context.chat_id,
            },
        ))
        .await
        .map_err(|error| error.to_string())?;
    let ControlResult::LookupRegisterRootReceipt(result) = result else {
        return Err("host broker returned an unexpected recovery response".to_owned());
    };
    if result.operation_id != operation_id {
        return Err("host broker returned a mismatched recovery receipt".to_owned());
    }
    match result.receipt {
        RegisterRootReceipt::Completed { root } => connected_resolution(root),
        RegisterRootReceipt::Disconnected { .. } => Ok(failed_resolution(
            "folder_access_disconnected",
            "The selected folder is no longer connected.",
            None,
        )),
        RegisterRootReceipt::Failed { error } => Ok(failed_resolution(
            "folder_access_registration_failed",
            "The selected folder could not be connected.",
            Some(error.message),
        )),
        RegisterRootReceipt::Unknown | RegisterRootReceipt::Pending => Ok(failed_resolution(
            "folder_access_outcome_unknown",
            "Folder access was not granted because the native outcome could not be confirmed.",
            None,
        )),
        _ => Ok(failed_resolution(
            "folder_access_receipt_unsupported",
            "Folder access was not granted because the native receipt was not understood.",
            None,
        )),
    }
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
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
        Ok(()) => {
            state
                .receipts
                .remove(receipt.call_id)
                .map_err(private_receipt_error)?;
            Ok(())
        }
        Err(error) if error.is_conflict() => {
            let still_pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if still_pending {
                Err("folder-access result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove(receipt.call_id)
                    .map_err(private_receipt_error)?;
                Ok(())
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
}

fn validate_canonical_call(
    call: &ToolCallRecord,
    chat_id: ChatId,
    call_id: CallId,
) -> Result<RequestFolderAccessArgs, String> {
    if call.id != call_id
        || call.chat_id != chat_id
        || call.name != REQUEST_FOLDER_ACCESS_TOOL
        || call.execution != ToolCallExecution::Client
        || call.status != ToolCallStatus::Pending
        || !validate_request_folder_access_arguments(&call.arguments)
    {
        return Err("local control plane returned an invalid folder-access request".to_owned());
    }
    serde_json::from_value(call.arguments.clone())
        .map_err(|_| "local control plane returned invalid folder-access arguments".to_owned())
}

fn picker_start(
    documents: Option<PathBuf>,
    downloads: Option<PathBuf>,
    hint: Option<RequestedFolderHint>,
) -> Option<PathBuf> {
    let candidate = match hint {
        Some(RequestedFolderHint::Documents) => documents?,
        Some(RequestedFolderHint::Downloads) => downloads?,
        None | Some(_) => return None,
    };
    candidate.is_dir().then_some(candidate)
}

fn connected_resolution(
    root: openwave_host_broker::RootSummary,
) -> Result<StoredResolution, String> {
    let result = RequestFolderAccessResult::Connected {
        root_id: root.root_id.as_uuid(),
        display_name: root.display_name,
        capabilities: vec![RequestedFolderCapability::ReadFiles],
    };
    Ok(StoredResolution::Completed {
        result: serde_json::to_string(&result)
            .map_err(|_| "could not encode folder-access result".to_owned())?,
    })
}

fn declined_resolution() -> Result<StoredResolution, String> {
    Ok(StoredResolution::Completed {
        result: serde_json::to_string(&RequestFolderAccessResult::Declined)
            .map_err(|_| "could not encode folder-access result".to_owned())?,
    })
}

fn failed_resolution(
    error_code: &str,
    message: &str,
    error_detail: Option<String>,
) -> StoredResolution {
    StoredResolution::Failed {
        result: serde_json::json!({ "status": "unavailable", "message": message }).to_string(),
        error_code: error_code.to_owned(),
        error_detail,
    }
}

fn control_plane(state: &HostAccess) -> Result<&ControlPlaneClient, String> {
    state
        .control_plane
        .get()
        .ok_or_else(|| "OpenWave is still starting".to_owned())
}

fn control_plane_error(error: ControlPlaneError) -> String {
    error.to_string()
}

fn private_receipt_error(_error: std::io::Error) -> String {
    "could not update private client-execution recovery state".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_folder_request_rejects_identity_and_contract_changes() {
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let mut call = ToolCallRecord {
            id: call_id,
            chat_id,
            turn_id: openwave_core::TurnId::new(),
            provider_id: "provider-call".into(),
            name: REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
                "folder_hint": "documents"
            }),
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
        assert!(validate_canonical_call(&call, chat_id, call_id).is_ok());
        call.arguments["path"] = serde_json::json!("/Users/example/Documents");
        assert!(validate_canonical_call(&call, chat_id, call_id).is_err());
        call.arguments = serde_json::json!({
            "reason": "Read reports",
            "requested_capabilities": ["read_files"]
        });
        assert!(validate_canonical_call(&call, ChatId::new(), call_id).is_err());
    }

    #[test]
    fn picker_hints_never_become_model_supplied_paths() {
        let temp = tempfile::tempdir().unwrap();
        let documents = temp.path().join("Documents");
        std::fs::create_dir(&documents).unwrap();
        assert_eq!(
            picker_start(
                Some(documents.clone()),
                None,
                Some(RequestedFolderHint::Documents)
            ),
            Some(documents)
        );
        assert_eq!(
            picker_start(None, None, Some(RequestedFolderHint::Downloads)),
            None
        );
        assert_eq!(picker_start(None, None, None), None);
    }

    #[test]
    fn terminal_results_are_typed_and_path_free() {
        let connected = connected_resolution(openwave_host_broker::RootSummary {
            root_id: openwave_host_broker::RootId::new(),
            display_name: "Documents".into(),
        })
        .unwrap();
        let StoredResolution::Completed { result } = connected else {
            panic!("expected completed result")
        };
        assert!(result.contains("connected"));
        assert!(!result.contains("/Users/"));
        assert!(matches!(
            declined_resolution().unwrap(),
            StoredResolution::Completed { result } if result.contains("declined")
        ));
    }

    #[test]
    fn recovery_never_starts_or_replays_registration() {
        assert!(should_start_registration(
            ExecutionMode::Interactive,
            RegistrationPhase::NotStarted
        ));
        assert!(!should_start_registration(
            ExecutionMode::Interactive,
            RegistrationPhase::Attempted
        ));
        assert!(!should_start_registration(
            ExecutionMode::Recovery,
            RegistrationPhase::NotStarted
        ));
        assert!(!should_start_registration(
            ExecutionMode::Recovery,
            RegistrationPhase::Attempted
        ));
    }
}
