//! Native save-to-disk for a conversation output.
//!
//! Everything a conversation output *is* — its catalog, previews, revision
//! history, restores, edits, and its bytes — now lives on the server's HTTP
//! surface, where a headless client reaches it too (decision record 7). What
//! is left here is the one thing that genuinely needs the native shell: the
//! save dialog, and the write to the path a reader chooses. The bytes come
//! from `GET /chats/{chat}/outputs/{output}/content` like any other client's
//! would, so there is no second implementation of output storage in the shell.
//!
//! Native exports are keyed by a renderer-minted operation identity so an
//! ambiguous bridge response can recover one exact terminal receipt rather
//! than repeating a write that may already have happened.

use std::io::{Read, Write};
use std::path::Path;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, State};
use tidebreak_core::{ChatId, OutputId, OutputRevisionId, MAX_BINARY_DELIVERABLE_BYTES};
use uuid::Uuid;

use crate::client_execution::{
    OutputExportFailureReason, OutputExportPhase, OutputExportReceipt, OutputExportTerminal,
    ReceiptStore,
};
use crate::documents::{local_client, native_auth, pick_export_path};
use crate::host_access::HostAccess;
use crate::{wait_server_info, AppState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExportDeliverableRequest {
    operation_id: Uuid,
    chat_id: Uuid,
    output_id: OutputId,
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

/// The fields of the server's output preview this command needs: which exact
/// revision it is about to export, and what to call the file it suggests.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OutputIdentity {
    revision_id: OutputRevisionId,
    filename: String,
}

/// Save one output's current revision wherever the reader chooses.
#[tauri::command]
pub(crate) async fn export_deliverable(
    app: AppHandle,
    app_state: State<'_, std::sync::Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: ExportDeliverableRequest,
) -> Result<OutputExportResult, String> {
    if request.operation_id.is_nil()
        || request.output_id.as_uuid().is_nil()
        || request.chat_id.is_nil()
    {
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

    let chat_id = ChatId::from(request.chat_id);
    let info = wait_server_info(app_state.inner()).await?;
    let identity = output_identity(&info, chat_id, request.output_id)
        .await
        .ok_or_else(|| "Output not found in this conversation".to_owned())?;

    let content = match output_bytes(&info, chat_id, request.output_id, identity.revision_id).await
    {
        Some(content) => content,
        None => {
            // Nothing durable has been recorded yet, so this failure is
            // reported directly rather than through a receipt.
            return Err("Could not read that output".to_owned());
        }
    };
    let mut receipt = OutputExportReceipt::new(
        request.operation_id,
        chat_id,
        request.output_id,
        identity.revision_id,
        identity.filename.clone(),
        content.len() as u64,
        Sha256::digest(&content).into(),
    );
    host_access
        .receipts
        .save_output_export(&receipt)
        .map_err(private_receipt_error)?;

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
    let destination = match pick_export_path(&app, "Save output", &identity.filename).await {
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
                destination_unavailable(),
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

    // A picker can remain open while the output or conversation is deleted, or
    // while something publishes a newer revision. Revalidate the exact durable
    // identity through the same route before the one authorized write.
    let still_current = output_identity(&info, chat_id, request.output_id)
        .await
        .is_some_and(|current| current.revision_id == receipt.revision_id);
    if !still_current {
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
    let byte_len = receipt.byte_len;
    let sha256 = receipt.sha256;
    let write = tauri::async_runtime::spawn_blocking(move || {
        write_exported_deliverable(&write_destination, &content)
    })
    .await;
    let terminal = match write {
        Ok(Ok(())) => OutputExportTerminal::Completed,
        Ok(Err(_)) | Err(_) if destination_matches(&destination, byte_len, sha256) => {
            OutputExportTerminal::Completed
        }
        Ok(Err(_)) | Err(_) => OutputExportTerminal::Failed {
            reason: OutputExportFailureReason::AmbiguousNativeFailure,
        },
    };
    terminalize_output_export(&host_access.receipts, &mut receipt, terminal)
}

/// The terminal both destination failures share: a dialog that could not be
/// opened, and a recovery that will not re-issue a delayed write.
fn destination_unavailable() -> OutputExportTerminal {
    OutputExportTerminal::Failed {
        reason: OutputExportFailureReason::DestinationUnavailable,
    }
}

fn outputs_path(chat_id: ChatId, output_id: OutputId) -> String {
    format!("/chats/{chat_id}/outputs/{output_id}")
}

/// Which revision the output is on right now, and what to call its file.
async fn output_identity(
    info: &crate::NativeServerInfo,
    chat_id: ChatId,
    output_id: OutputId,
) -> Option<OutputIdentity> {
    let response = native_auth(
        local_client().get(format!(
            "{}{}",
            info.base_url,
            outputs_path(chat_id, output_id)
        )),
        info,
    )
    .send()
    .await
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<OutputIdentity>().await.ok()
}

/// One exact revision's complete bytes, as the server serves them to every
/// other client.
async fn output_bytes(
    info: &crate::NativeServerInfo,
    chat_id: ChatId,
    output_id: OutputId,
    revision_id: OutputRevisionId,
) -> Option<Vec<u8>> {
    let response = native_auth(
        local_client().get(format!(
            "{}{}/content?revision_id={revision_id}",
            info.base_url,
            outputs_path(chat_id, output_id)
        )),
        info,
    )
    .send()
    .await
    .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    (!bytes.is_empty() && bytes.len() <= MAX_BINARY_DELIVERABLE_BYTES).then(|| bytes.to_vec())
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
            OutputExportPhase::DestinationSelected => destination_unavailable(),
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
    let temporary = format!(".tidebreak-export-{}.tmp", Uuid::new_v4());
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
    use super::*;

    #[test]
    fn exact_export_retries_recover_completed_cancelled_and_ambiguous_receipts() {
        let private = tempfile::tempdir().unwrap();
        let receipts = ReceiptStore::open(private.path()).unwrap();
        let destination_root = tempfile::tempdir().unwrap();
        let content = b"exact";
        let chat_id = ChatId::new();
        let output_id = OutputId::new();
        let revision_id = OutputRevisionId::new();
        let sha256: [u8; 32] = Sha256::digest(content).into();

        let new_receipt = |operation_id: Uuid| {
            OutputExportReceipt::new(
                operation_id,
                chat_id,
                output_id,
                revision_id,
                "brief.txt".to_owned(),
                content.len() as u64,
                sha256,
            )
        };

        let mut cancelled = new_receipt(Uuid::new_v4());
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
        let mut interrupted = new_receipt(Uuid::new_v4());
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
        let mut completed = new_receipt(Uuid::new_v4());
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
        let mut ambiguous = new_receipt(Uuid::new_v4());
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

    #[cfg(unix)]
    #[test]
    fn export_destinations_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let outside = tempfile::tempdir().unwrap();
        let destination = outside.path().join("destination.txt");
        let target = outside.path().join("target.txt");
        std::fs::write(&target, "keep").unwrap();
        symlink(&target, &destination).unwrap();
        assert!(write_exported_deliverable(&destination, b"replace").is_err());
        assert!(!destination_matches(
            &destination,
            7,
            Sha256::digest(b"replace").into()
        ));
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
    }

    /// The renderer names an output by opaque identity only: a request that
    /// tries to smuggle a host path in is rejected outright, and the receipt
    /// that comes back names no destination.
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
