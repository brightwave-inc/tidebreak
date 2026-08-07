//! Trusted executor for publishing immutable outputs into attached host roots.
//!
//! Model arguments carry only opaque output/root identities and a bounded
//! root-relative destination. Native recovery binds the current immutable
//! revision, digest, lease, and (for replacement) a fresh user approval before
//! any bytes cross into the host broker.

use std::collections::HashSet;

use openwave_code_execution::{
    MaterializationPrecondition, MaterializedChangeKind, RejectedChangeReason,
};
use openwave_core::{
    CallId, ChatId, HostRootId, OutputWriteMode, ToolCallRecord, WriteOutputToConnectedFolderArgs,
    WRITE_OUTPUT_TO_CONNECTED_FOLDER_TOOL,
};
use openwave_host_broker::RelativePath;
use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

use crate::{
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
    if !requires_user_decision(&state, chat_id, arguments.mode).await? {
        return Err("this write-back does not require a decision".to_owned());
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
                    result: match arguments.mode {
                        OutputWriteMode::Create => {
                            "Writing that output to the connected folder was declined.".to_owned()
                        }
                        OutputWriteMode::Replace => "Output replacement was declined.".to_owned(),
                    },
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
            // A write-back the reader still has to decide belongs to the
            // approval card, not to this executor. The mode is read live, so a
            // chat moved to `Auto` while one was parked simply proceeds here.
            match requires_user_decision(&state, summary.chat_id, arguments.mode).await {
                Ok(false) => {}
                Ok(true) => continue,
                Err(_) => {
                    failed = true;
                    continue;
                }
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
            let claimed = match canonical_arguments(&claim.call, summary.chat_id, call_id) {
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
            if state.receipts.save_output_writeback(&receipt).is_err()
                || execute_receipt(app, &state, receipt).await.is_err()
            {
                failed = true;
            }
        }
    }
    failed
}

/// Whether this write-back parks on the approval card instead of running
/// unattended, under the chat's current permission mode.
async fn requires_user_decision(
    state: &HostAccess,
    chat_id: ChatId,
    mode: OutputWriteMode,
) -> Result<bool, String> {
    let store = state
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let chat = store
        .get_chat(chat_id)
        .await
        .map_err(|_| "could not read the conversation's permission mode".to_owned())?
        .ok_or_else(|| "conversation is no longer available".to_owned())?;
    Ok(mode.requires_user_decision(chat.permission_mode))
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
    let Some(materializer) = state.staged_folders() else {
        return terminalize(
            state,
            &mut receipt,
            unavailable("output_writeback_authority_unavailable"),
        )
        .await;
    };
    let path = RelativePath::parse(&receipt.relative_path)
        .map_err(|_| "invalid output write-back destination".to_owned())?;
    if receipt.phase == FolderOperationPhase::DispatchStarted {
        let resolution = if materializer
            .connected_file_matches(
                receipt.chat_id,
                receipt.root_id,
                path.as_str(),
                receipt.byte_len,
                receipt.sha256,
            )
            .await
        {
            completed(&receipt)
        } else {
            unavailable("output_writeback_ambiguous_native_failure")
        };
        return terminalize(state, &mut receipt, resolution).await;
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
            read_output_revision_bytes(&scratch_root, receipt.chat_id, &output, &revision)
        })
        .await
        .map_err(|_| "could not read immutable output revision".to_owned())??
    };

    client
        .heartbeat(receipt.chat_id, receipt.call_id, receipt.lease_token)
        .await
        .map_err(control_plane_error)?;
    receipt.phase = FolderOperationPhase::DispatchStarted;
    state
        .receipts
        .save_output_writeback(&receipt)
        .map_err(private_receipt_error)?;
    let expected = match receipt.mode {
        OutputWriteMode::Create => MaterializationPrecondition::Absent,
        OutputWriteMode::Replace => MaterializationPrecondition::Existing,
    };
    let expected_change = match receipt.mode {
        OutputWriteMode::Create => MaterializedChangeKind::Created,
        OutputWriteMode::Replace => MaterializedChangeKind::Overwritten,
    };
    let resolution = match materializer
        .materialize_connected_file(
            receipt.chat_id,
            claim.call.turn_id,
            receipt.root_id,
            path.as_str(),
            &bytes,
            expected,
        )
        .await
    {
        Ok(change) if change == expected_change => completed(&receipt),
        Ok(_) => unavailable("output_writeback_materialization_protocol"),
        Err(reason) => materialization_resolution(receipt.mode, reason),
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
            .remove_output_writeback(receipt.call_id)
            .map_err(private_receipt_error),
        Err(error) if error.is_conflict() => {
            let pending = client
                .pending(receipt.chat_id)
                .await
                .map_err(control_plane_error)?
                .into_iter()
                .any(|call| call.id == receipt.call_id);
            if pending {
                Err("output write-back result no longer owns the pending request".to_owned())
            } else {
                state
                    .receipts
                    .remove_output_writeback(receipt.call_id)
                    .map_err(private_receipt_error)
            }
        }
        Err(error) => Err(control_plane_error(error)),
    }
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

fn completed(receipt: &OutputWritebackReceipt) -> StoredResolution {
    StoredResolution::Completed {
        result: format!(
            "Published output {} revision {} to the connected folder.",
            receipt.output_id, receipt.revision_id
        ),
        rows: Some(serde_json::json!({
            "entries": [openwave_core::ResultEntry::new(
                openwave_core::ResultEntryKind::Output,
                file_name(&receipt.relative_path),
            )
            .with_detail(receipt.relative_path.clone())
            .with_meta(openwave_core::format_bytes(receipt.byte_len))],
        })),
    }
}

fn materialization_resolution(
    mode: OutputWriteMode,
    reason: RejectedChangeReason,
) -> StoredResolution {
    let code = match (mode, reason) {
        (OutputWriteMode::Create, RejectedChangeReason::Stale) => {
            "output_writeback_destination_exists"
        }
        (OutputWriteMode::Replace, RejectedChangeReason::Stale) => {
            "output_writeback_destination_changed"
        }
        (_, RejectedChangeReason::SnapshotUnavailable) => "output_writeback_snapshot_unavailable",
        (_, RejectedChangeReason::StagedFileTooLarge) => "output_writeback_source_unavailable",
        (_, RejectedChangeReason::TrashUnavailable) => "output_writeback_unavailable",
        (_, RejectedChangeReason::Unavailable) => "output_writeback_unavailable",
    };
    unavailable(code)
}

/// The last segment of a connected-folder path, so a row leads with the file
/// rather than the path it sits under.
fn file_name(path: &str) -> String {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(path)
        .to_owned()
}
