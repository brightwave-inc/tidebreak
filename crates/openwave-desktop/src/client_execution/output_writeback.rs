//! Trusted executor for publishing immutable outputs into attached host roots.
//!
//! Model arguments carry only opaque output/root identities and a bounded
//! root-relative destination. Native recovery binds the current immutable
//! revision, digest, lease, and (for replacement) a fresh user approval before
//! any bytes cross into the host broker.

use std::collections::HashSet;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use openwave_core::{
    CallId, ChatId, HostRootId, OutputWriteMode, ToolCallRecord,
    WriteOutputToConnectedFolderArgs, WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};
use openwave_host_broker::{
    ErrorCode, OperationEnvelope, OperationRequest, OperationResult, RelativePath, RootId,
    WriteApproval, WriteFileMode, WriteFileRequest, PROTOCOL_VERSION,
};
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
    broker::BrokerClientError,
    deliverables::{read_output_revision_bytes, require_exact_revision, require_live_output},
    host_access::HostAccess,
};

use super::{
    control_plane, control_plane_error, private_receipt_error, FolderOperationPhase,
    OutputWritebackReceipt, StoredResolution,
};

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputWritebackDecision {
    Allow,
    Decline,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ResolveOutputWritebackRequest {
    chat_id: Uuid,
    call_id: Uuid,
    decision: OutputWritebackDecision,
}

#[tauri::command]
pub(crate) async fn resolve_output_writeback_request(
    app: AppHandle,
    state: State<'_, HostAccess>,
    request: ResolveOutputWritebackRequest,
) -> Result<(), String> {
    if request.chat_id.is_nil() || request.call_id.is_nil() {
        return Err("invalid output write-back request identity".to_owned());
    }
    let _exclusive = state.output_writebacks.lock().await;
    let chat_id = ChatId::from(request.chat_id);
    let call_id = CallId::from(request.call_id);
    let client = control_plane(&state)?;
    let call = client
        .pending(chat_id)
        .await
        .map_err(control_plane_error)?
        .into_iter()
        .find(|call| call.id == call_id)
        .ok_or_else(|| "output write-back request is no longer pending".to_owned())?;
    let arguments = canonical_arguments(&call, chat_id, call_id)?;
    if arguments.mode != OutputWriteMode::Replace {
        return Err("only replacement write-backs require a decision".to_owned());
    }

    let claim = client
        .claim(
            chat_id,
            call_id,
            state.receipts.executor_id(),
            Uuid::new_v4(),
        )
        .await
        .map_err(control_plane_error)?;
    let claimed = canonical_arguments(&claim.call, chat_id, call_id)?;
    if claimed != arguments || claim.lease_token.is_nil() {
        return Err("local control plane returned a mismatched write-back claim".to_owned());
    }
    if matches!(request.decision, OutputWritebackDecision::Decline) {
        return client
            .resolve(
                chat_id,
                call_id,
                claim.lease_token,
                &StoredResolution::Cancelled {
                    result: "Output replacement was declined.".to_owned(),
                },
            )
            .await
            .map_err(control_plane_error);
    }

    let receipt = prepare_receipt(
        &state,
        claim.call,
        claim.lease_token,
        claimed,
        Some(Uuid::new_v4()),
    )
    .await?;
    state
        .receipts
        .save_output_writeback(&receipt)
        .map_err(private_receipt_error)?;
    execute_receipt(&app, &state, receipt).await
}

pub(crate) async fn recover_output_writebacks(app: AppHandle) {
    loop {
        if recover_once(&app).await {
            eprintln!("openwave-desktop: output write-back executor deferred work");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn recover_once(app: &AppHandle) -> bool {
    let state = app.state::<HostAccess>();
    let _exclusive = state.output_writebacks.lock().await;
    let receipts = match state.receipts.load_output_writebacks() {
        Ok(receipts) => receipts,
        Err(_) => return true,
    };
    let recovered = receipts
        .iter()
        .map(|receipt| receipt.call_id)
        .collect::<HashSet<_>>();
    let mut failed = false;
    for receipt in receipts {
        if execute_receipt(app, &state, receipt).await.is_err() {
            failed = true;
        }
    }

    let client = match control_plane(&state) {
        Ok(client) => client,
        Err(_) => return true,
    };
    let summaries = match client.pending_output_writebacks().await {
        Ok(summaries) => summaries,
        Err(_) => return true,
    };
    for summary in summaries {
        let calls = match client.pending(summary.chat_id).await {
            Ok(calls) => calls,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for call_id in summary
            .output_writeback_call_ids
            .into_iter()
            .filter(|call_id| !recovered.contains(call_id))
        {
            let Some(call) = calls.iter().find(|call| call.id == call_id) else {
                continue;
            };
            let Ok(arguments) = canonical_arguments(call, summary.chat_id, call_id) else {
                continue;
            };
            if arguments.mode != OutputWriteMode::Create {
                continue;
            }
            let lease_token = Uuid::new_v4();
            let claim = match client
                .claim(
                    summary.chat_id,
                    call_id,
                    state.receipts.executor_id(),
                    lease_token,
                )
                .await
            {
                Ok(claim) => claim,
                Err(_) => {
                    failed = true;
                    continue;
                }
            };
            let claimed =
                match canonical_arguments(&claim.call, summary.chat_id, call_id) {
                    Ok(claimed) if claimed == arguments => claimed,
                    _ => {
                        failed = true;
                        continue;
                    }
                };
            let receipt =
                match prepare_receipt(&state, claim.call, claim.lease_token, claimed, None).await {
                    Ok(receipt) => receipt,
                    Err(_) => {
                        let resolution = unavailable("output_writeback_source_unavailable");
                        let _ = client
                            .resolve(summary.chat_id, call_id, claim.lease_token, &resolution)
                            .await;
                        continue;
                    }
                };
            if state
                .receipts
                .save_output_writeback(&receipt)
                .is_err()
                || execute_receipt(app, &state, receipt).await.is_err()
            {
                failed = true;
            }
        }
    }
    failed
}

async fn prepare_receipt(
    state: &HostAccess,
    call: ToolCallRecord,
    lease_token: Uuid,
    arguments: WriteOutputToConnectedFolderArgs,
    approval_id: Option<Uuid>,
) -> Result<OutputWritebackReceipt, String> {
    let chat_id = call.chat_id;
    let output_id = openwave_core::OutputId::from(arguments.output_id);
    let root_id = HostRootId::from_uuid(arguments.root_id)
        .map_err(|_| "invalid connected-root identity".to_owned())?;
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(chat_id)
        .await
        .map_err(|_| "could not verify connected-root authority".to_owned())?
        .filter(|chat| {
            chat.root_attachments
                .iter()
                .any(|attachment| attachment.root_id == root_id)
        })
        .ok_or_else(|| "connected root is no longer attached".to_owned())?;
    let _ = chat;
    let (output, revision) = require_live_output(Some(store), chat_id, output_id).await?;
    let mut receipt = OutputWritebackReceipt::new(
        chat_id,
        call.id,
        state.receipts.executor_id(),
        output.id,
        revision.id,
        root_id,
        arguments.path,
        arguments.mode,
        approval_id,
        revision.byte_len,
        revision.sha256,
    );
    receipt.lease_token = lease_token;
    Ok(receipt)
}

async fn execute_receipt(
    app: &AppHandle,
    state: &HostAccess,
    mut receipt: OutputWritebackReceipt,
) -> Result<(), String> {
    if receipt.executor_id != state.receipts.executor_id() {
        return Err("output write-back receipt has the wrong native executor".to_owned());
    }
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
            return Err("output write-back call is no longer pending".to_owned());
        }
        Err(error) => return Err(control_plane_error(error)),
    };
    let claimed = canonical_arguments(&claim.call, receipt.chat_id, receipt.call_id)?;
    if claimed.output_id != *receipt.output_id.as_uuid()
        || claimed.root_id != *receipt.root_id.as_uuid()
        || claimed.path != receipt.relative_path
        || claimed.mode != receipt.mode
        || claim.lease_token != receipt.lease_token
    {
        return Err("local control plane returned a stale write-back claim".to_owned());
    }
    let context = match state.context(receipt.chat_id.0).await {
        Ok(context) => context,
        Err(_) => {
            return terminalize(
                state,
                &mut receipt,
                unavailable("output_writeback_authority_unavailable"),
            )
            .await;
        }
    };
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let attached = store
        .get_chat(receipt.chat_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|chat| {
            chat.root_attachments
                .iter()
                .any(|attachment| attachment.root_id == receipt.root_id)
        });
    if !attached
        || require_exact_revision(
            Some(store),
            receipt.chat_id,
            receipt.output_id,
            receipt.revision_id,
            receipt.byte_len,
            receipt.sha256,
        )
        .await
        .is_err()
    {
        return terminalize(
            state,
            &mut receipt,
            unavailable("output_writeback_stale_or_detached"),
        )
        .await;
    }

    let scratch_root = crate::data_dir(app)?.join("scratch");
    let (output, revision) =
        require_live_output(Some(store), receipt.chat_id, receipt.output_id).await?;
    if revision.id != receipt.revision_id {
        return terminalize(
            state,
            &mut receipt,
            unavailable("output_writeback_stale_revision"),
        )
        .await;
    }
    let bytes = {
        let output = output.clone();
        let revision = revision.clone();
        tauri::async_runtime::spawn_blocking(move || {
            read_output_revision_bytes(
                &scratch_root,
                receipt.chat_id,
                &output,
                &revision,
            )
        })
        .await
        .map_err(|_| "could not read immutable output revision".to_owned())??
    };
    let root_id = RootId::from_uuid(*receipt.root_id.as_uuid())
        .map_err(|_| "invalid connected-root identity".to_owned())?;
    let path = RelativePath::parse(&receipt.relative_path)
        .map_err(|_| "invalid output write-back destination".to_owned())?;
    let mode = match receipt.mode {
        OutputWriteMode::Create => WriteFileMode::Create,
        OutputWriteMode::Replace => WriteFileMode::Replace,
    };
    let approval = receipt.approval_id.map(|approval_id| WriteApproval { approval_id });

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_output_writeback(&receipt)
        .map_err(private_receipt_error)?;
    let operation = OperationEnvelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: openwave_host_broker::RequestId::new(),
        context: context.execution,
        request: OperationRequest::WriteFile(WriteFileRequest {
            operation_id: receipt.operation_id,
            root_id,
            path,
            mode,
            approval,
            content_base64: BASE64.encode(&bytes),
            bytes: bytes.len(),
            sha256: receipt.sha256,
        }),
    };
    let resolution = match state.broker.operation(operation).await {
        Ok(OperationResult::WriteFile(result))
            if result.operation_id == receipt.operation_id
                && result.bytes as u64 == receipt.byte_len
                && result.replaced == matches!(receipt.mode, OutputWriteMode::Replace) =>
        {
            StoredResolution::Completed {
                result: format!(
                    "Published output {} revision {} to the connected folder.",
                    receipt.output_id, receipt.revision_id
                ),
            }
        }
        Ok(_) => unavailable("output_writeback_broker_protocol"),
        Err(error) => broker_resolution(&error),
    };
    terminalize(state, &mut receipt, resolution).await
}

async fn terminalize(
    state: &HostAccess,
    receipt: &mut OutputWritebackReceipt,
    resolution: StoredResolution,
) -> Result<(), String> {
    receipt.resolution = Some(resolution.clone());
    state
        .receipts
        .save_output_writeback(receipt)
        .map_err(private_receipt_error)?;
    publish_resolution(state, receipt, &resolution).await
}

async fn publish_resolution(
    state: &HostAccess,
    receipt: &OutputWritebackReceipt,
    resolution: &StoredResolution,
) -> Result<(), String> {
    control_plane(state)?
        .resolve(
            receipt.chat_id,
            receipt.call_id,
            receipt.lease_token,
            resolution,
        )
        .await
        .map_err(control_plane_error)
}

fn canonical_arguments(
    call: &ToolCallRecord,
    chat_id: ChatId,
    call_id: CallId,
) -> Result<WriteOutputToConnectedFolderArgs, String> {
    if call.id != call_id
        || call.chat_id != chat_id
        || call.name != WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL
    {
        return Err("invalid output write-back call identity".to_owned());
    }
    let arguments: WriteOutputToConnectedFolderArgs =
        serde_json::from_value(call.arguments.clone())
            .map_err(|_| "invalid output write-back arguments".to_owned())?;
    if !arguments.is_well_formed() {
        return Err("invalid output write-back arguments".to_owned());
    }
    Ok(arguments)
}

fn unavailable(code: &str) -> StoredResolution {
    StoredResolution::Failed {
        result: "That output could not be written to the connected folder.".to_owned(),
        error_code: code.to_owned(),
        error_detail: None,
    }
}

fn broker_resolution(error: &BrokerClientError) -> StoredResolution {
    let code = match error {
        BrokerClientError::Broker {
            code: ErrorCode::AlreadyExists,
            ..
        } => "output_writeback_destination_exists",
        BrokerClientError::Broker {
            code: ErrorCode::Denied | ErrorCode::InvalidRoot,
            ..
        } => "output_writeback_authority_unavailable",
        BrokerClientError::Broker {
            code: ErrorCode::AmbiguousWrite,
            ..
        } => "output_writeback_ambiguous_native_failure",
        _ => "output_writeback_unavailable",
    };
    unavailable(code)
}
