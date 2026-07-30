//! Native bridge for durable conversation outputs.
//!
//! The renderer names an output only by opaque identity. Authoritative output
//! ownership and immutable revision metadata come from the product store;
//! private scratch paths and selected export destinations terminate inside this
//! module. Native exports are keyed by a renderer-minted operation identity so
//! an ambiguous response can recover one exact terminal receipt.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use chrono::Utc;
use openwave_core::{
    deliverable_media_type, media_type_is_text, revision_byte_ceiling, ChatId, OutputId,
    OutputRecord, OutputRevision, OutputRevisionId, Store, MAX_BINARY_DELIVERABLE_BYTES,
    OUTPUTS_DIRECTORY,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::client_execution::{
    OutputExportFailureReason, OutputExportPhase, OutputExportReceipt, OutputExportTerminal,
    ReceiptStore,
};
use crate::documents::resolve_conversation_scope;
use crate::host_access::HostAccess;

const MAX_DELIVERABLES: usize = 100;
const MAX_PREVIEW_CHARACTERS: usize = 100_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeliverablesRequest {
    chat_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DeliverableRequest {
    chat_id: Uuid,
    output_id: OutputId,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExportDeliverableRequest {
    operation_id: Uuid,
    chat_id: Uuid,
    output_id: OutputId,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliverableSummary {
    output_id: OutputId,
    filename: String,
    media_type: String,
    size_bytes: u64,
    revision_count: u32,
    updated_at: String,
    /// Background run that produced the current revision, when the output was
    /// auto-merged from a background agent rather than a foreground turn. The
    /// renderer uses it to badge the output and correlate it with the agent-run
    /// surface; it is a display key, not authority.
    producing_run_id: Option<Uuid>,
}

/// Outcome of reverting a merged output.
///
/// Reverting an output with earlier revisions republishes the previous one;
/// reverting one that has only its initial merge retracts it. Both are durable
/// and reversible — a retract is undone by `restore_output`, and a revert can be
/// followed forward again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub(crate) enum OutputRevertResult {
    Reverted {
        output_id: OutputId,
        revision_id: OutputRevisionId,
    },
    Retracted {
        output_id: OutputId,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliverablesCatalog {
    deliverables: Vec<DeliverableSummary>,
    truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliverablePreview {
    output_id: OutputId,
    filename: String,
    media_type: String,
    content: String,
    truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutputExportResult {
    operation_id: Uuid,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    #[serde(flatten)]
    terminal: OutputExportTerminal,
}

#[tauri::command]
pub(crate) async fn list_deliverables(
    host_access: State<'_, HostAccess>,
    request: DeliverablesRequest,
) -> Result<DeliverablesCatalog, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let store = host_access
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let mut outputs = store
        .list_outputs(chat_id, (MAX_DELIVERABLES + 1) as u64)
        .await
        .map_err(|_| "Could not load this conversation's outputs".to_owned())?;
    let truncated = outputs.len() > MAX_DELIVERABLES;
    outputs.truncate(MAX_DELIVERABLES);
    let mut deliverables = Vec::with_capacity(outputs.len());
    for output in outputs {
        let revision = store
            .get_output_revision(output.current_revision)
            .await
            .map_err(|_| "Could not load this conversation's outputs".to_owned())?
            .ok_or_else(|| "Could not load this conversation's outputs".to_owned())?;
        deliverables.push(summary_from_record(&output, &revision)?);
    }
    Ok(DeliverablesCatalog {
        deliverables,
        truncated,
    })
}

#[tauri::command]
pub(crate) async fn read_deliverable(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: DeliverableRequest,
) -> Result<DeliverablePreview, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let (output, revision) =
        require_live_output(host_access.store(), chat_id, request.output_id).await?;
    // Binary artifacts have no inline preview; the renderer keys its placeholder
    // off the media type, so the response carries identity without content.
    if !media_type_is_text(&output.media_type) {
        return Ok(DeliverablePreview {
            output_id: output.id,
            filename: output.filename.clone(),
            media_type: output.media_type.clone(),
            content: String::new(),
            truncated: false,
        });
    }
    let scratch_root = crate::data_dir(&app)?.join("scratch");
    let read_output = output.clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        read_output_revision_bytes(&scratch_root, chat_id, &read_output, &revision)
    })
    .await
    .map_err(|_| "Could not preview that output".to_owned())??;
    preview_from_bytes(&output, bytes)
}

#[tauri::command]
pub(crate) async fn export_deliverable(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: ExportDeliverableRequest,
) -> Result<OutputExportResult, String> {
    if request.operation_id.is_nil() || request.output_id.as_uuid().is_nil() {
        return Err("Invalid output export request".to_owned());
    }
    let _export = host_access.output_exports.lock().await;
    if let Some(mut receipt) = host_access
        .receipts
        .load_output_export(request.operation_id)
        .map_err(private_receipt_error)?
    {
        if receipt.chat_id.as_uuid() != &request.chat_id || receipt.output_id != request.output_id {
            return Err("That export operation belongs to a different output".to_owned());
        }
        return recover_output_export_receipt(&host_access.receipts, &mut receipt);
    }

    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let (output, revision) =
        require_live_output(host_access.store(), chat_id, request.output_id).await?;
    let mut receipt = OutputExportReceipt::new(
        request.operation_id,
        chat_id,
        output.id,
        revision.id,
        output.filename.clone(),
        revision.byte_len,
        revision.sha256,
    );
    host_access
        .receipts
        .save_output_export(&receipt)
        .map_err(private_receipt_error)?;

    let scratch_root = crate::data_dir(&app)?.join("scratch");
    let read_output = output.clone();
    let read_revision = revision.clone();
    let content = match tauri::async_runtime::spawn_blocking(move || {
        read_output_revision_bytes(&scratch_root, chat_id, &read_output, &read_revision)
    })
    .await
    {
        Ok(Ok(content)) => content,
        Ok(Err(_)) | Err(_) => {
            return terminalize_output_export(
                &host_access.receipts,
                &mut receipt,
                OutputExportTerminal::Failed {
                    reason: OutputExportFailureReason::SourceUnavailable,
                },
            );
        }
    };

    let picker = match host_access.picker.try_lock() {
        Ok(picker) => picker,
        Err(_) => {
            return terminalize_output_export(
                &host_access.receipts,
                &mut receipt,
                OutputExportTerminal::Failed {
                    reason: OutputExportFailureReason::DestinationUnavailable,
                },
            );
        }
    };
    let destination = match pick_export_path(&app, &output.filename).await {
        Ok(Some(destination)) if destination.is_absolute() => destination,
        Ok(None) => {
            return terminalize_output_export(
                &host_access.receipts,
                &mut receipt,
                OutputExportTerminal::Cancelled,
            );
        }
        Ok(Some(_)) | Err(_) => {
            return terminalize_output_export(
                &host_access.receipts,
                &mut receipt,
                OutputExportTerminal::Failed {
                    reason: OutputExportFailureReason::DestinationUnavailable,
                },
            );
        }
    };
    drop(picker);

    receipt.phase = OutputExportPhase::DestinationSelected;
    receipt.destination = Some(destination.clone());
    host_access
        .receipts
        .save_output_export(&receipt)
        .map_err(private_receipt_error)?;

    // A picker can remain open while the output or conversation is deleted.
    // Revalidate the exact durable identities before the one authorized write.
    if require_exact_revision(
        host_access.store(),
        chat_id,
        output.id,
        revision.id,
        revision.byte_len,
        revision.sha256,
    )
    .await
    .is_err()
    {
        return terminalize_output_export(
            &host_access.receipts,
            &mut receipt,
            OutputExportTerminal::Failed {
                reason: OutputExportFailureReason::SourceUnavailable,
            },
        );
    }

    receipt.phase = OutputExportPhase::DispatchStarted;
    host_access
        .receipts
        .save_output_export(&receipt)
        .map_err(private_receipt_error)?;
    let write_destination = destination.clone();
    let write = tauri::async_runtime::spawn_blocking(move || {
        write_exported_deliverable(&write_destination, &content)
    })
    .await;
    let terminal = match write {
        Ok(Ok(())) => OutputExportTerminal::Completed,
        Ok(Err(_)) | Err(_) if destination_matches_revision(&destination, &revision) => {
            OutputExportTerminal::Completed
        }
        Ok(Err(_)) | Err(_) => OutputExportTerminal::Failed {
            reason: OutputExportFailureReason::AmbiguousNativeFailure,
        },
    };
    terminalize_output_export(&host_access.receipts, &mut receipt, terminal)
}

#[tauri::command]
pub(crate) async fn revert_output(
    host_access: State<'_, HostAccess>,
    request: DeliverableRequest,
) -> Result<OutputRevertResult, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let store = host_access
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let (output, current) =
        require_live_output(host_access.store(), chat_id, request.output_id).await?;
    // Reverting is a host action, never the model's. An output that has only its
    // initial merge cannot step back to an earlier version, so reverting it
    // retracts the merge entirely — a soft delete that `restore_output` undoes.
    if current.ordinal <= 1 {
        store
            .delete_output(output.id, Utc::now())
            .await
            .map_err(|_| "Could not revert that output".to_owned())?;
        return Ok(OutputRevertResult::Retracted {
            output_id: output.id,
        });
    }
    // Otherwise step the current pointer back to the immediately previous
    // revision. The superseded revision is retained and addressable, so a revert
    // destroys nothing and can be followed forward again.
    let previous = store
        .list_output_revisions(output.id)
        .await
        .map_err(|_| "Could not revert that output".to_owned())?
        .into_iter()
        .find(|revision| revision.ordinal == current.ordinal - 1)
        .ok_or_else(|| "That output has no earlier revision to revert to".to_owned())?;
    let record = store
        .set_current_output_revision(output.id, previous.id, Utc::now())
        .await
        .map_err(|_| "Could not revert that output".to_owned())?;
    Ok(OutputRevertResult::Reverted {
        output_id: record.id,
        revision_id: previous.id,
    })
}

#[tauri::command]
pub(crate) async fn restore_output(
    host_access: State<'_, HostAccess>,
    request: DeliverableRequest,
) -> Result<DeliverableSummary, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let store = host_access
        .store()
        .ok_or_else(|| "OpenWave is still starting".to_owned())?;
    // A restore targets a soft-deleted output, so it cannot go through the
    // live-only `require_live_output`. Bind it to the exact conversation before
    // clearing the retraction.
    let output = store
        .get_output(request.output_id)
        .await
        .map_err(|_| "Could not restore that output".to_owned())?
        .filter(|output| output.chat_id == chat_id)
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    store
        .restore_output(output.id, Utc::now())
        .await
        .map_err(|_| "Could not restore that output".to_owned())?;
    let (output, revision) =
        require_live_output(host_access.store(), chat_id, request.output_id).await?;
    summary_from_record(&output, &revision)
}

pub(crate) async fn require_live_output(
    store: Option<&std::sync::Arc<dyn Store>>,
    chat_id: ChatId,
    output_id: OutputId,
) -> Result<(OutputRecord, OutputRevision), String> {
    let store = store.ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let output = store
        .get_output(output_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|output| output.chat_id == chat_id && output.deleted_at.is_none())
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    let revision = store
        .get_output_revision(output.current_revision)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|revision| revision.output_id == output.id)
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    Ok((output, revision))
}

pub(crate) async fn require_exact_revision(
    store: Option<&std::sync::Arc<dyn Store>>,
    chat_id: ChatId,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    byte_len: u64,
    sha256: [u8; 32],
) -> Result<(), String> {
    let store = store.ok_or_else(|| "OpenWave is still starting".to_owned())?;
    let output = store
        .get_output(output_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|output| {
            output.chat_id == chat_id
                && output.deleted_at.is_none()
                && output.current_revision == revision_id
        })
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    let revision = store
        .get_output_revision(revision_id)
        .await
        .map_err(|_| "Could not load that output".to_owned())?
        .filter(|revision| {
            revision.output_id == output.id
                && revision.byte_len == byte_len
                && revision.sha256 == sha256
        })
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;
    if revision.id != revision_id {
        return Err("Output not found in this conversation".to_owned());
    }
    Ok(())
}

fn summary_from_record(
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<DeliverableSummary, String> {
    // Text outputs derive their media type from the filename; binary artifacts
    // carry an explicit media type with an arbitrary extension, so only text is
    // held to the filename-derived type. Each kind keeps its own size ceiling.
    if output.deleted_at.is_some()
        || output.current_revision != revision.id
        || output.id != revision.output_id
        || output.revision_count == 0
        || revision.byte_len > revision_byte_ceiling(&output.media_type) as u64
        || (media_type_is_text(&output.media_type)
            && deliverable_media_type(&output.filename) != Some(output.media_type.as_str()))
    {
        return Err("Could not load this conversation's outputs".to_owned());
    }
    Ok(DeliverableSummary {
        output_id: output.id,
        filename: output.filename.clone(),
        media_type: output.media_type.clone(),
        size_bytes: revision.byte_len,
        revision_count: output.revision_count,
        updated_at: output.updated_at.to_rfc3339(),
        producing_run_id: revision.producing_run_id.map(|run_id| *run_id.as_uuid()),
    })
}

pub(crate) fn read_output_revision_bytes(
    scratch_root: &Path,
    chat_id: ChatId,
    output: &OutputRecord,
    revision: &OutputRevision,
) -> Result<Vec<u8>, String> {
    let ceiling = revision_byte_ceiling(&output.media_type);
    if output.chat_id != chat_id
        || output.deleted_at.is_some()
        || revision.output_id != output.id
        || revision.byte_len > ceiling as u64
    {
        return Err("Output not found in this conversation".to_owned());
    }
    let Some(root) = open_regular_directory(scratch_root)? else {
        return Err("Output content is unavailable".to_owned());
    };
    let chat_name = chat_id.to_string();
    let output_name = output.id.to_string();
    if !is_regular_child_directory(&root, &chat_name)? {
        return Err("Output content is unavailable".to_owned());
    }
    let chat = root
        .open_dir_nofollow(&chat_name)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !is_regular_child_directory(&chat, OUTPUTS_DIRECTORY)? {
        return Err("Output content is unavailable".to_owned());
    }
    let outputs = chat
        .open_dir_nofollow(OUTPUTS_DIRECTORY)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !is_regular_child_directory(&outputs, &output_name)? {
        return Err("Output content is unavailable".to_owned());
    }
    let revisions = outputs
        .open_dir_nofollow(&output_name)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = revisions
        .open_with(revision.id.to_string(), &options)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if !metadata.is_file() || metadata.len() != revision.byte_len {
        return Err("Output content is unavailable".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((ceiling + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Output content is unavailable".to_owned())?;
    if bytes.len() as u64 != revision.byte_len
        || bytes.len() > ceiling
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != revision.sha256
    {
        return Err("Output content is unavailable".to_owned());
    }
    Ok(bytes)
}

fn preview_from_bytes(output: &OutputRecord, bytes: Vec<u8>) -> Result<DeliverablePreview, String> {
    let text =
        String::from_utf8(bytes).map_err(|_| "Output is not a supported text file".to_owned())?;
    if text.contains('\0')
        || deliverable_media_type(&output.filename) != Some(output.media_type.as_str())
    {
        return Err("Output is not a supported text file".to_owned());
    }
    let mut characters = text.chars();
    let content: String = characters.by_ref().take(MAX_PREVIEW_CHARACTERS).collect();
    let truncated = characters.next().is_some();
    Ok(DeliverablePreview {
        output_id: output.id,
        filename: output.filename.clone(),
        media_type: output.media_type.clone(),
        content,
        truncated,
    })
}

fn recover_output_export_receipt(
    store: &ReceiptStore,
    receipt: &mut OutputExportReceipt,
) -> Result<OutputExportResult, String> {
    if receipt.terminal.is_none() {
        let terminal = match receipt.phase {
            // No selected destination means no host write could have happened.
            OutputExportPhase::Prepared => OutputExportTerminal::Cancelled,
            // A destination was durably selected, but dispatch was not. Recovery
            // refuses to issue a delayed write after native state may have moved.
            OutputExportPhase::DestinationSelected => OutputExportTerminal::Failed {
                reason: OutputExportFailureReason::DestinationUnavailable,
            },
            // Once dispatch may have begun, recovery only verifies the exact
            // digest. It never repeats the write.
            OutputExportPhase::DispatchStarted => {
                if receipt.destination.as_ref().is_some_and(|destination| {
                    destination_matches(destination, receipt.byte_len, receipt.sha256)
                }) {
                    OutputExportTerminal::Completed
                } else {
                    OutputExportTerminal::Failed {
                        reason: OutputExportFailureReason::AmbiguousNativeFailure,
                    }
                }
            }
        };
        receipt.terminal = Some(terminal);
        store
            .save_output_export(receipt)
            .map_err(private_receipt_error)?;
    }
    output_export_result(receipt)
}

fn terminalize_output_export(
    store: &ReceiptStore,
    receipt: &mut OutputExportReceipt,
    terminal: OutputExportTerminal,
) -> Result<OutputExportResult, String> {
    receipt.terminal = Some(terminal);
    store
        .save_output_export(receipt)
        .map_err(private_receipt_error)?;
    output_export_result(receipt)
}

fn output_export_result(receipt: &OutputExportReceipt) -> Result<OutputExportResult, String> {
    Ok(OutputExportResult {
        operation_id: receipt.operation_id,
        output_id: receipt.output_id,
        revision_id: receipt.revision_id,
        terminal: receipt
            .terminal
            .ok_or_else(|| "Output export has no terminal receipt".to_owned())?,
    })
}

fn destination_matches_revision(destination: &Path, revision: &OutputRevision) -> bool {
    destination_matches(destination, revision.byte_len, revision.sha256)
}

fn destination_matches(destination: &Path, byte_len: u64, sha256: [u8; 32]) -> bool {
    if !destination.is_absolute() || byte_len > MAX_BINARY_DELIVERABLE_BYTES as u64 {
        return false;
    }
    let Some(parent) = destination.parent() else {
        return false;
    };
    let Some(filename) = destination.file_name() else {
        return false;
    };
    let Ok(directory) = Dir::open_ambient_dir(parent, ambient_authority()) else {
        return false;
    };
    let Ok(metadata) = directory.symlink_metadata(filename) else {
        return false;
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != byte_len {
        return false;
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let Ok(file) = directory.open_with(filename, &options) else {
        return false;
    };
    let mut bytes = Vec::with_capacity(byte_len as usize);
    if file
        .take((MAX_BINARY_DELIVERABLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return false;
    }
    bytes.len() as u64 == byte_len && <[u8; 32]>::from(Sha256::digest(&bytes)) == sha256
}

fn open_regular_directory(path: &Path) -> Result<Option<Dir>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not open private outputs".to_owned()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Private output storage is invalid".to_owned());
    }
    let directory = Dir::open_ambient_dir(path, ambient_authority())
        .map_err(|_| "Could not open private outputs".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let opened = directory
            .dir_metadata()
            .map_err(|_| "Could not open private outputs".to_owned())?;
        if metadata.dev() != cap_std::fs::MetadataExt::dev(&opened)
            || metadata.ino() != cap_std::fs::MetadataExt::ino(&opened)
        {
            return Err("Private output storage changed while it was opened".to_owned());
        }
    }
    Ok(Some(directory))
}

fn is_regular_child_directory(parent: &Dir, name: &str) -> Result<bool, String> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Private output storage is invalid".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("Could not inspect private outputs".to_owned()),
    }
}

async fn pick_export_path(app: &AppHandle, filename: &str) -> Result<Option<PathBuf>, String> {
    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Save output")
        .set_file_name(filename);
    if let Some(window) = app.get_webview_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.save_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The save dialog closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The save dialog returned an invalid destination".to_owned())
}

fn write_exported_deliverable(destination: &Path, content: &[u8]) -> Result<(), String> {
    if !destination.is_absolute() || content.len() > MAX_BINARY_DELIVERABLE_BYTES {
        return Err("The save destination is invalid".to_owned());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let filename = destination
        .file_name()
        .ok_or_else(|| "The save destination is invalid".to_owned())?;
    let directory = Dir::open_ambient_dir(parent, ambient_authority())
        .map_err(|_| "Could not open the selected folder".to_owned())?;
    let permissions = match directory.symlink_metadata(filename) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Some(metadata.permissions())
        }
        Ok(_) => return Err("The selected destination is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("Could not inspect the selected destination".to_owned()),
    };
    let temporary = format!(".openwave-export-{}.tmp", Uuid::new_v4());
    let result = (|| -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = directory.open_with(&temporary, &options)?;
        file.write_all(content)?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        drop(file);
        directory.rename(&temporary, &directory, filename)?;
        #[cfg(unix)]
        directory.open(".")?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = directory.remove_file(&temporary);
    }
    result.map_err(|_| "Could not save that output".to_owned())
}

fn private_receipt_error(_error: std::io::Error) -> String {
    "Could not recover that output export".to_owned()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::*;

    fn output_record(
        chat_id: ChatId,
        filename: &str,
        content: &[u8],
    ) -> (OutputRecord, OutputRevision) {
        let output_id = OutputId::new();
        let revision_id = OutputRevisionId::new();
        let created_at = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        (
            OutputRecord {
                id: output_id,
                chat_id,
                filename: filename.to_owned(),
                media_type: deliverable_media_type(filename).unwrap().to_owned(),
                current_revision: revision_id,
                revision_count: 1,
                created_at,
                updated_at: created_at,
                deleted_at: None,
            },
            OutputRevision {
                id: revision_id,
                output_id,
                ordinal: 1,
                byte_len: content.len() as u64,
                sha256: Sha256::digest(content).into(),
                turn_id: None,
                producing_run_id: None,
                created_at,
            },
        )
    }

    fn revision_path(root: &Path, output: &OutputRecord, revision: &OutputRevision) -> PathBuf {
        root.join(output.chat_id.to_string())
            .join(OUTPUTS_DIRECTORY)
            .join(output.id.to_string())
            .join(revision.id.to_string())
    }

    #[test]
    fn catalog_and_preview_project_durable_opaque_identity() {
        let content = b"# Brief";
        let (output, revision) = output_record(ChatId::new(), "brief.md", content);
        let summary = summary_from_record(&output, &revision).unwrap();
        assert_eq!(summary.output_id, output.id);
        assert_eq!(summary.size_bytes, content.len() as u64);
        assert_eq!(summary.revision_count, 1);

        let preview = preview_from_bytes(&output, content.to_vec()).unwrap();
        assert_eq!(preview.output_id, output.id);
        assert_eq!(preview.content, "# Brief");
        let serialized = serde_json::to_value(preview).unwrap();
        assert_eq!(
            serialized
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["content", "filename", "mediaType", "outputId", "truncated"]
        );
        for forbidden in ["path", "scratch", "chatId", "token", "sha256"] {
            assert!(!serialized.to_string().contains(forbidden));
        }
    }

    fn binary_output_record(
        chat_id: ChatId,
        filename: &str,
        media_type: &str,
        content: &[u8],
    ) -> (OutputRecord, OutputRevision) {
        let output_id = OutputId::new();
        let revision_id = OutputRevisionId::new();
        let created_at = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        (
            OutputRecord {
                id: output_id,
                chat_id,
                filename: filename.to_owned(),
                media_type: media_type.to_owned(),
                current_revision: revision_id,
                revision_count: 1,
                created_at,
                updated_at: created_at,
                deleted_at: None,
            },
            OutputRevision {
                id: revision_id,
                output_id,
                ordinal: 1,
                byte_len: content.len() as u64,
                sha256: Sha256::digest(content).into(),
                turn_id: None,
                producing_run_id: None,
                created_at,
            },
        )
    }

    #[test]
    fn binary_artifacts_beyond_the_text_cap_are_cataloged_read_and_exported() {
        let scratch = tempfile::tempdir().unwrap();
        // A binary artifact larger than the 512 KiB text cap.
        let mut content = b"\x89PNG\r\n\x1a\n".to_vec();
        content.resize(700 * 1024, 9);
        let (output, revision) =
            binary_output_record(ChatId::new(), "chart.png", "image/png", &content);

        // It is a valid catalog entry despite its non-text media type and its
        // size exceeding the text ceiling.
        let summary = summary_from_record(&output, &revision).unwrap();
        assert_eq!(summary.media_type, "image/png");
        assert_eq!(summary.size_bytes, content.len() as u64);

        // Its bytes round-trip out of private scratch unchanged.
        let path = revision_path(scratch.path(), &output, &revision);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &content).unwrap();
        assert_eq!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).unwrap(),
            content
        );

        // And they export to a chosen destination as raw bytes.
        let destination_root = tempfile::tempdir().unwrap();
        let destination = destination_root.path().join("chart.png");
        write_exported_deliverable(&destination, &content).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), content);
        assert!(destination_matches_revision(&destination, &revision));
    }

    #[test]
    fn immutable_revision_reads_are_exactly_scoped_and_content_addressed() {
        let scratch = tempfile::tempdir().unwrap();
        let content = b"private";
        let (output, revision) = output_record(ChatId::new(), "brief.txt", content);
        let path = revision_path(scratch.path(), &output, &revision);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();

        assert_eq!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).unwrap(),
            content
        );
        assert!(
            read_output_revision_bytes(scratch.path(), ChatId::new(), &output, &revision).is_err()
        );
        std::fs::write(path, b"tampered").unwrap();
        assert!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_revision_and_export_destinations_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let content = b"private";
        let (output, revision) = output_record(ChatId::new(), "brief.txt", content);
        let path = revision_path(scratch.path(), &output, &revision);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let outside_source = outside.path().join("source.txt");
        std::fs::write(&outside_source, content).unwrap();
        symlink(&outside_source, &path).unwrap();
        assert!(
            read_output_revision_bytes(scratch.path(), output.chat_id, &output, &revision).is_err()
        );

        let destination = outside.path().join("destination.txt");
        let target = outside.path().join("target.txt");
        std::fs::write(&target, "keep").unwrap();
        symlink(&target, &destination).unwrap();
        assert!(write_exported_deliverable(&destination, b"replace").is_err());
        assert!(!destination_matches_revision(&destination, &revision));
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }

    #[test]
    fn exact_export_retries_recover_completed_cancelled_and_ambiguous_receipts() {
        let private = tempfile::tempdir().unwrap();
        let receipts = ReceiptStore::open(private.path()).unwrap();
        let destination_root = tempfile::tempdir().unwrap();
        let content = b"exact";
        let (output, revision) = output_record(ChatId::new(), "brief.txt", content);

        let mut cancelled = OutputExportReceipt::new(
            Uuid::new_v4(),
            output.chat_id,
            output.id,
            revision.id,
            output.filename.clone(),
            revision.byte_len,
            revision.sha256,
        );
        receipts.save_output_export(&cancelled).unwrap();
        let first_cancel = recover_output_export_receipt(&receipts, &mut cancelled).unwrap();
        let mut cancelled_retry = receipts
            .load_output_export(cancelled.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            recover_output_export_receipt(&receipts, &mut cancelled_retry).unwrap(),
            first_cancel
        );
        assert!(matches!(
            first_cancel.terminal,
            OutputExportTerminal::Cancelled
        ));

        let interrupted_destination = destination_root.path().join("interrupted.txt");
        let mut interrupted = OutputExportReceipt::new(
            Uuid::new_v4(),
            output.chat_id,
            output.id,
            revision.id,
            output.filename.clone(),
            revision.byte_len,
            revision.sha256,
        );
        interrupted.phase = OutputExportPhase::DestinationSelected;
        interrupted.destination = Some(interrupted_destination.clone());
        receipts.save_output_export(&interrupted).unwrap();
        let interrupted_result =
            recover_output_export_receipt(&receipts, &mut interrupted).unwrap();
        assert!(matches!(
            interrupted_result.terminal,
            OutputExportTerminal::Failed {
                reason: OutputExportFailureReason::DestinationUnavailable
            }
        ));
        assert!(
            !interrupted_destination.exists(),
            "recovery must not replay a write that was never dispatched"
        );

        let destination = destination_root.path().join("brief.txt");
        std::fs::write(&destination, content).unwrap();
        let mut completed = OutputExportReceipt::new(
            Uuid::new_v4(),
            output.chat_id,
            output.id,
            revision.id,
            output.filename.clone(),
            revision.byte_len,
            revision.sha256,
        );
        completed.phase = OutputExportPhase::DispatchStarted;
        completed.destination = Some(destination);
        receipts.save_output_export(&completed).unwrap();
        let completed_result = recover_output_export_receipt(&receipts, &mut completed).unwrap();
        assert!(matches!(
            completed_result.terminal,
            OutputExportTerminal::Completed
        ));

        let ambiguous_destination = destination_root.path().join("ambiguous.txt");
        std::fs::write(&ambiguous_destination, b"different").unwrap();
        let mut ambiguous = OutputExportReceipt::new(
            Uuid::new_v4(),
            output.chat_id,
            output.id,
            revision.id,
            output.filename,
            revision.byte_len,
            revision.sha256,
        );
        ambiguous.phase = OutputExportPhase::DispatchStarted;
        ambiguous.destination = Some(ambiguous_destination);
        receipts.save_output_export(&ambiguous).unwrap();
        let first_ambiguous = recover_output_export_receipt(&receipts, &mut ambiguous).unwrap();
        let mut ambiguous_retry = receipts
            .load_output_export(ambiguous.operation_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            recover_output_export_receipt(&receipts, &mut ambiguous_retry).unwrap(),
            first_ambiguous
        );
        assert!(matches!(
            first_ambiguous.terminal,
            OutputExportTerminal::Failed {
                reason: OutputExportFailureReason::AmbiguousNativeFailure
            }
        ));
    }

    #[test]
    fn export_is_atomic_and_preserves_existing_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("brief.md");
        std::fs::write(&destination, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o640)).unwrap();
        }
        write_exported_deliverable(&destination, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                0o640
            );
        }
    }

    #[test]
    fn requests_and_export_results_are_renderer_safe() {
        assert!(
            serde_json::from_value::<ExportDeliverableRequest>(serde_json::json!({
                "operationId": Uuid::new_v4(),
                "chatId": Uuid::new_v4(),
                "outputId": OutputId::new(),
                "path": "/tmp/private"
            }))
            .is_err()
        );
        let result = OutputExportResult {
            operation_id: Uuid::new_v4(),
            output_id: OutputId::new(),
            revision_id: OutputRevisionId::new(),
            terminal: OutputExportTerminal::Failed {
                reason: OutputExportFailureReason::AmbiguousNativeFailure,
            },
        };
        let serialized = serde_json::to_value(result).unwrap();
        assert_eq!(
            serialized
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["operationId", "outputId", "reason", "revisionId", "status"]
        );
        for forbidden in ["path", "destination", "content", "sha256", "chatId"] {
            assert!(!serialized.to_string().contains(forbidden));
        }
    }
}
