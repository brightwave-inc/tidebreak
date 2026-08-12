//! Native owner of durable client-executed folder consent.
//!
//! The renderer discovers canonical pending requests, but never receives the
//! claim token, picker result, or broker control surface. This module keeps
//! those values together and persists enough private state to recover an exact
//! outcome after a desktop or transport failure.

use std::path::PathBuf;

use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use tidebreak_core::{
    validate_request_folder_access_arguments, CallId, ChatId, RequestFolderAccessArgs,
    RequestFolderAccessResult, RequestedFolderHint, ToolCallExecution, ToolCallRecord,
    ToolCallStatus, REQUEST_FOLDER_ACCESS_TOOL,
};
use tidebreak_host_broker::{
    Capability, ConsentMethod, ControlRequest, ControlResult, LookupRegisterRootReceiptRequest,
    OperationEnvelope, OperationRequest, OperationResult, RegisterRootReceipt, RegisterRootRequest,
    RootSummary, PROTOCOL_VERSION,
};
use uuid::Uuid;

use crate::host_access::{pick_folder, AuthoritativeContext, HostAccess};

use self::folder_operations::granted_folder_capabilities;

pub(crate) mod computer_use;
mod control_plane;
pub(crate) mod delegated_file_read;
pub(crate) mod folder_operations;
pub(crate) mod output_writeback;
mod product_sync;
mod receipt_store;
pub(crate) mod root_attachment_reconciliation;
pub(crate) mod source_import;

pub(crate) use control_plane::ControlPlaneClient;
use control_plane::ControlPlaneError;
pub(crate) use receipt_store::ReceiptStore;
use receipt_store::{
    delegated_file_content_fits_server, ComputerUseReceipt, DelegatedFileFailureReason,
    DelegatedFileReadReceipt, DelegatedFileResolution, DispatchRecovery, FolderAccessIntent,
    FolderAccessReceipt, FolderOperationPhase, FolderOperationReceipt, ManualFolderConnectReceipt,
    ProductRootAttachmentSync, RegistrationPhase, StoredResolution,
};
pub(crate) use receipt_store::{
    OutputExportFailureReason, OutputExportPhase, OutputExportReceipt, OutputExportTerminal,
    OutputWritebackReceipt,
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
            eprintln!("tidebreak-desktop: private client-execution recovery failed: {error}");
            return true;
        }
    };
    let _exclusive = state.picker.lock().await;
    let mut had_failure = false;
    for receipt in receipts {
        if let Err(error) = execute_receipt(&state, receipt, ExecutionMode::Recovery).await {
            eprintln!("tidebreak-desktop: client-execution recovery deferred: {error}");
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
            let _root_change = state.root_changes.lock().await;
            drive_selected_folder(state, &mut receipt, context, path, mode).await?
        }
    };
    receipt.resolution = Some(resolution.clone());
    state
        .receipts
        .save(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(state, &receipt, &resolution).await
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

    let resolution = match receipt.intent.clone() {
        FolderAccessIntent::Decline => declined_resolution()?,
        FolderAccessIntent::Selected { path } => {
            let _root_change = state.root_changes.lock().await;
            drive_selected_folder(state, &mut receipt, context, path, ExecutionMode::Recovery)
                .await?
        }
    };
    receipt.resolution = Some(resolution.clone());
    state
        .receipts
        .save(&receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(state, &receipt, &resolution).await
}

enum RegistrationOutcome {
    Unknown,
    Registered(RootSummary),
    Terminal(StoredResolution),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegistrationAction {
    ResumeProduct,
    Dispatch,
    LookupOnly,
}

async fn drive_selected_folder(
    state: &HostAccess,
    receipt: &mut FolderAccessReceipt,
    context: AuthoritativeContext,
    path: PathBuf,
    mode: ExecutionMode,
) -> Result<StoredResolution, String> {
    let action = registration_action(
        mode,
        receipt.registration_phase,
        receipt.product_sync.is_some(),
    );
    if action == RegistrationAction::ResumeProduct {
        return product_sync::resume_product_attachment(state, receipt, context).await;
    }

    let mut outcome = if action == RegistrationAction::LookupOnly {
        recover_registration_outcome(state, receipt, context).await?
    } else {
        RegistrationOutcome::Unknown
    };

    if action == RegistrationAction::Dispatch {
        let client = control_plane(state)?;
        client
            .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
            .await
            .map_err(control_plane_error)?;
        receipt.registration_phase = RegistrationPhase::Attempted;
        state
            .receipts
            .save(receipt)
            .map_err(private_receipt_error)?;
        let dispatch_deadline =
            tokio::time::Instant::now() + crate::broker::MUTATION_DISPATCH_WINDOW;
        dispatch_registration(state, receipt, context, path, dispatch_deadline).await?;
        outcome = recover_registration_outcome(state, receipt, context).await?;
    }

    match outcome {
        RegistrationOutcome::Registered(root) => {
            product_sync::synchronize_product_attachment(state, receipt, context, root).await
        }
        RegistrationOutcome::Terminal(resolution) => Ok(resolution),
        RegistrationOutcome::Unknown => Err(
            "folder registration has no durable broker outcome yet; recovery will retry".to_owned(),
        ),
    }
}

fn registration_action(
    mode: ExecutionMode,
    phase: RegistrationPhase,
    has_product_sync: bool,
) -> RegistrationAction {
    if has_product_sync {
        RegistrationAction::ResumeProduct
    } else if mode == ExecutionMode::Interactive && phase == RegistrationPhase::NotStarted {
        RegistrationAction::Dispatch
    } else {
        RegistrationAction::LookupOnly
    }
}

async fn dispatch_registration(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    context: AuthoritativeContext,
    path: PathBuf,
    dispatch_deadline: tokio::time::Instant,
) -> Result<(), String> {
    let operation_id = receipt.registration_operation_id;
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
    Ok(())
}

async fn recover_registration_outcome(
    state: &HostAccess,
    receipt: &FolderAccessReceipt,
    context: AuthoritativeContext,
) -> Result<RegistrationOutcome, String> {
    let operation_id = receipt.registration_operation_id;
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
    registration_receipt_outcome(result.receipt)
}

fn registration_receipt_outcome(
    receipt: RegisterRootReceipt,
) -> Result<RegistrationOutcome, String> {
    match receipt {
        RegisterRootReceipt::Completed { root } => Ok(RegistrationOutcome::Registered(root)),
        RegisterRootReceipt::Disconnected { .. } => {
            Ok(RegistrationOutcome::Terminal(failed_resolution(
                "folder_access_disconnected",
                "The selected folder is no longer connected.",
                None,
            )))
        }
        RegisterRootReceipt::Failed { error } => {
            Ok(RegistrationOutcome::Terminal(failed_resolution(
                "folder_access_registration_failed",
                "The selected folder could not be connected.",
                Some(error.message),
            )))
        }
        RegisterRootReceipt::Unknown => Ok(RegistrationOutcome::Terminal(failed_resolution(
            "folder_access_outcome_unknown",
            "Folder access was not granted because the native outcome could not be confirmed.",
            None,
        ))),
        RegisterRootReceipt::Pending => Err(
            "folder registration is still pending in the host broker; recovery will retry"
                .to_owned(),
        ),
        _ => Ok(RegistrationOutcome::Terminal(failed_resolution(
            "folder_access_receipt_unsupported",
            "Folder access was not granted because the native receipt was not understood.",
            None,
        ))),
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

async fn connected_resolution(
    state: &HostAccess,
    context: AuthoritativeContext,
    root: tidebreak_host_broker::RootSummary,
) -> Result<StoredResolution, String> {
    let capabilities = state
        .broker
        .operation(OperationEnvelope {
            protocol_version: PROTOCOL_VERSION,
            request_id: tidebreak_host_broker::RequestId::new(),
            context: context.execution,
            request: OperationRequest::ListRoots,
        })
        .await
        .ok()
        .and_then(|listed| match listed {
            OperationResult::ListRoots { roots } => roots
                .into_iter()
                .find(|candidate| candidate.root_id == root.root_id)
                .map(|access| access.capabilities),
            _ => None,
        });
    connected_resolution_from_capabilities(root, capabilities.as_deref())
}

fn connected_resolution_from_capabilities(
    root: tidebreak_host_broker::RootSummary,
    capabilities: Option<&[Capability]>,
) -> Result<StoredResolution, String> {
    let result = RequestFolderAccessResult::Connected {
        root_id: root.root_id.as_uuid(),
        display_name: root.display_name,
        // A revocation or broker outage can race this post-consent projection.
        // Consent already succeeded, so return the pathless root identity and
        // under-report reach rather than turning that race into a user-facing
        // failure immediately after the picker closes.
        capabilities: capabilities
            .map(granted_folder_capabilities)
            .unwrap_or_default(),
    };
    Ok(StoredResolution::Completed {
        result: serde_json::to_string(&result)
            .map_err(|_| "could not encode folder-access result".to_owned())?,
        rows: None,
        images: None,
    })
}

fn declined_resolution() -> Result<StoredResolution, String> {
    Ok(StoredResolution::Completed {
        result: serde_json::to_string(&RequestFolderAccessResult::Declined)
            .map_err(|_| "could not encode folder-access result".to_owned())?,
        rows: None,
        images: None,
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
        .ok_or_else(|| "Tidebreak is still starting".to_owned())
}

fn control_plane_error(error: ControlPlaneError) -> String {
    error.to_string()
}

fn private_receipt_error(_error: std::io::Error) -> String {
    "could not update private client-execution recovery state".to_owned()
}

#[cfg(test)]
mod tests {
    use tidebreak_core::{
        HostRootId, RootAttachmentChangeAction, RootAttachmentChangeId, RootAttachmentChangePhase,
        RootAttachmentSubjectKind,
    };

    use super::product_sync::{attachment_operation_id, validate_product_change};
    use super::receipt_store::{AttachmentPhase, CleanupPhase, ProductRootAttachmentSync};
    use super::*;

    #[test]
    fn canonical_folder_request_rejects_identity_and_contract_changes() {
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let mut call = ToolCallRecord {
            id: call_id,
            chat_id,
            turn_id: tidebreak_core::TurnId::new(),
            provider_id: "provider-call".into(),
            name: REQUEST_FOLDER_ACCESS_TOOL.into(),
            arguments: serde_json::json!({
                "reason": "Read reports",
                "requested_capabilities": ["read_files"],
                "folder_hint": "documents"
            }),
            raw_arguments: None,
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            result_preview: None,
            provider_replay: None,
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
        let connected = connected_resolution_from_capabilities(
            tidebreak_host_broker::RootSummary {
                root_id: tidebreak_host_broker::RootId::new(),
                display_name: "Documents".into(),
            },
            Some(&[
                Capability::ReadFiles,
                Capability::WriteFiles,
                Capability::ExecuteCommands,
            ]),
        )
        .unwrap();
        let StoredResolution::Completed { result, .. } = connected else {
            panic!("expected completed result")
        };
        assert!(result.contains("connected"));
        assert!(!result.contains("/Users/"));
        assert!(matches!(
            declined_resolution().unwrap(),
            StoredResolution::Completed { result, .. } if result.contains("declined")
        ));
    }

    #[test]
    fn product_attachment_validation_fences_identity_authority_and_broker_state() {
        let chat_id = ChatId::new();
        let call_id = CallId::new();
        let mut receipt = FolderAccessReceipt::new(
            chat_id,
            call_id,
            Uuid::new_v4(),
            FolderAccessIntent::Selected {
                path: PathBuf::from("/tmp/Documents"),
            },
        );
        receipt.registration_phase = RegistrationPhase::Attempted;
        let sync = ProductRootAttachmentSync {
            change_id: RootAttachmentChangeId::new(),
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            display_name: "Documents".to_owned(),
            expected_attachment_revision: 4,
            created_at: chrono::Utc::now(),
            cleanup_operation_id: tidebreak_host_broker::OperationId::new(),
            cleanup_phase: CleanupPhase::NotStarted,
            attachment_phase: AttachmentPhase::DispatchAttempted,
        };
        receipt.product_sync = Some(sync.clone());
        let context = AuthoritativeContext {
            chat_id: chat_id.0,
            execution: tidebreak_host_broker::ExecutionContext::standalone(chat_id.0).unwrap(),
            subject: tidebreak_host_broker::GrantSubject::conversation(chat_id.0).unwrap(),
        };
        let mut change = control_plane::RootAttachmentChangeView {
            id: sync.change_id,
            chat_id,
            root_id: sync.root_id,
            action: RootAttachmentChangeAction::Attach,
            subject_kind: RootAttachmentSubjectKind::Conversation,
            subject_id: chat_id.0,
            expected_revision: 4,
            before_revision: 4,
            intent_revision: 5,
            projection_existed_before: false,
            phase: RootAttachmentChangePhase::Completed,
            result_revision: Some(5),
            broker_currently_attached: Some(true),
            failure: None,
            created_at: sync.created_at,
        };
        assert!(validate_product_change(&change, &receipt, &sync, context).is_ok());

        change.subject_id = Uuid::new_v4();
        assert!(validate_product_change(&change, &receipt, &sync, context).is_err());
        change.subject_id = chat_id.0;
        change.broker_currently_attached = Some(false);
        assert!(validate_product_change(&change, &receipt, &sync, context).is_err());
    }

    #[test]
    fn attempted_registration_is_lookup_only_and_attachment_reuses_exact_identity() {
        assert_eq!(
            registration_action(
                ExecutionMode::Interactive,
                RegistrationPhase::NotStarted,
                false,
            ),
            RegistrationAction::Dispatch
        );
        assert_eq!(
            registration_action(
                ExecutionMode::Interactive,
                RegistrationPhase::Attempted,
                false,
            ),
            RegistrationAction::LookupOnly
        );
        assert_eq!(
            registration_action(
                ExecutionMode::Recovery,
                RegistrationPhase::NotStarted,
                false,
            ),
            RegistrationAction::LookupOnly
        );
        assert_eq!(
            registration_action(ExecutionMode::Recovery, RegistrationPhase::Attempted, false,),
            RegistrationAction::LookupOnly
        );
        assert_eq!(
            registration_action(ExecutionMode::Recovery, RegistrationPhase::Attempted, true,),
            RegistrationAction::ResumeProduct
        );
        assert!(matches!(
            registration_receipt_outcome(RegisterRootReceipt::Unknown).unwrap(),
            RegistrationOutcome::Terminal(StoredResolution::Failed { error_code, .. })
                if error_code == "folder_access_outcome_unknown"
        ));

        let sync = ProductRootAttachmentSync {
            change_id: RootAttachmentChangeId::new(),
            root_id: HostRootId::from_uuid(Uuid::new_v4()).unwrap(),
            display_name: "Documents".to_owned(),
            expected_attachment_revision: 0,
            created_at: chrono::Utc::now(),
            cleanup_operation_id: tidebreak_host_broker::OperationId::new(),
            cleanup_phase: CleanupPhase::NotStarted,
            attachment_phase: AttachmentPhase::DispatchAttempted,
        };
        let first = attachment_operation_id(&sync).unwrap();
        let recovered = attachment_operation_id(&sync).unwrap();
        assert_eq!(first, recovered);
        assert_eq!(first.as_uuid(), *sync.change_id.as_uuid());
    }
}
