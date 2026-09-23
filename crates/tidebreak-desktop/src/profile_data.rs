//! The native halves of Settings → Data and privacy.
//!
//! The server owns what a backup and an export contain (`POST /data/backup`
//! and `POST /data/export`, which the CLI reaches too). What is left here is
//! what only the desktop shell can do: open the data folder in the file
//! manager, ask where to save a file and write it there, and delete this
//! computer's profile, keychain items included, then quit.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tidebreak_core::keychain::KeychainSecretProvider;
use tidebreak_server::profile_data::{ConversationExportFormat, ConversationExportRequest};
use tokio::io::AsyncWriteExt as _;

use crate::documents::{native_auth, pick_export_path, streaming_local_client};
use crate::host_access::HostAccess;
use crate::remote::RemoteAttachment;
use crate::{wait_server_info, AppState};

/// What the person types to confirm Delete all data. The renderer asks for
/// it, and this command checks it again, so no single call can erase the
/// profile by accident.
pub(crate) const DELETE_ALL_DATA_PHRASE: &str = "delete all data";

const ATTACHED_ELSEWHERE: &str =
    "This window is attached to another machine, and its data lives there.";

/// A file this command wrote where the person chose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SavedFile {
    path: String,
    bytes: u64,
    /// Files in a backup, or conversations in an export, as the server
    /// reported them.
    count: Option<u64>,
}

/// Open the profile's data folder in this computer's file manager.
#[tauri::command]
pub(crate) async fn reveal_data_directory(
    app: AppHandle,
    attachment: State<'_, Arc<RemoteAttachment>>,
) -> Result<(), String> {
    if attachment.current().await.is_some() {
        return Err(ATTACHED_ELSEWHERE.to_owned());
    }
    let dir = crate::data_dir(&app)?;
    crate::code_worktree::open_directory(dir)
        .map(|_| ())
        .map_err(|_| "Tidebreak could not open the data folder.".to_owned())
}

/// Ask where to save a backup, then stream `POST /data/backup` there.
/// `None` when the person closes the save dialog.
#[tauri::command]
pub(crate) async fn save_profile_backup(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
) -> Result<Option<SavedFile>, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await?;
    let name = format!(
        "Tidebreak backup {}.tar.gz",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let Some(destination) = pick_destination(&app, &host_access, "Save backup", &name).await?
    else {
        return Ok(None);
    };
    let info = wait_server_info(app_state.inner()).await?;
    let response = native_auth(
        streaming_local_client().post(format!("{}/data/backup", info.base_url)),
        &info,
    )
    .send()
    .await
    .map_err(|error| format!("Tidebreak could not start the backup: {error}"))?;
    save_response(response, &destination, "x-tidebreak-backup-files")
        .await
        .map(Some)
}

/// Ask where to save an export, then stream `POST /data/export` there.
/// `None` when the person closes the save dialog.
#[tauri::command]
pub(crate) async fn save_conversation_export(
    app: AppHandle,
    app_state: State<'_, Arc<AppState>>,
    host_access: State<'_, HostAccess>,
    request: Value,
) -> Result<Option<SavedFile>, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await?;
    let request: ConversationExportRequest = serde_json::from_value(request)
        .map_err(|error| format!("The export request is not valid: {error}"))?;
    let extension = match request.format {
        ConversationExportFormat::Markdown => "zip",
        ConversationExportFormat::Json => "json",
    };
    let name = format!(
        "Tidebreak conversations {}.{extension}",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let Some(destination) =
        pick_destination(&app, &host_access, "Export conversations", &name).await?
    else {
        return Ok(None);
    };
    let info = wait_server_info(app_state.inner()).await?;
    let response = native_auth(
        streaming_local_client().post(format!("{}/data/export", info.base_url)),
        &info,
    )
    .json(&request)
    .send()
    .await
    .map_err(|error| format!("Tidebreak could not start the export: {error}"))?;
    save_response(response, &destination, "x-tidebreak-conversations")
        .await
        .map(Some)
}

async fn pick_destination(
    app: &AppHandle,
    host_access: &HostAccess,
    title: &str,
    name: &str,
) -> Result<Option<PathBuf>, String> {
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    pick_export_path(app, title, name).await
}

/// Write a streamed answer to `destination`: into a hidden file beside it
/// first, then renamed over it once every byte the server announced is on
/// disk. A download that stops partway leaves nothing at `destination`.
async fn save_response(
    response: reqwest::Response,
    destination: &Path,
    count_header: &str,
) -> Result<SavedFile, String> {
    let mut response = response;
    let status = response.status();
    if !status.is_success() {
        let body = response.bytes().await.unwrap_or_default();
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|body| {
                body.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("the server answered {status}"));
        return Err(message);
    }
    let count = response
        .headers()
        .get(count_header)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok());
    let announced = response.content_length();
    let Some(file_name) = destination.file_name() else {
        return Err("The save dialog returned an invalid destination".to_owned());
    };
    let temporary = destination.with_file_name(format!(
        ".{}.{}.part",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await.map_err(|error| {
        format!(
            "Tidebreak could not write {}: {error}",
            destination.display()
        )
    })?;
    let written = async {
        let mut written = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("the download stopped: {error}"))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|error| error.to_string())?;
            written += chunk.len() as u64;
        }
        if announced.is_some_and(|announced| announced != written) {
            return Err("the download ended early".to_owned());
        }
        file.sync_all().await.map_err(|error| error.to_string())?;
        Ok(written)
    }
    .await;
    drop(file);
    let written = match written {
        Ok(written) => written,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(format!(
                "Tidebreak could not save {}: {error}",
                destination.display()
            ));
        }
    };
    if let Err(error) = tokio::fs::rename(&temporary, destination).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!(
            "Tidebreak could not save {}: {error}",
            destination.display()
        ));
    }
    Ok(SavedFile {
        path: destination.display().to_string(),
        bytes: written,
        count,
    })
}

/// Delete everything this computer holds for Tidebreak, then quit.
///
/// Keys go first, from the keychain service this channel uses. If any of
/// them cannot be removed, nothing else is touched and the error says so.
/// Then the running work stops, native helpers wind down the way quitting
/// winds them down, and the data folder, the cache folder, and the settings
/// and log folders go. The process then exits at once, without the usual quit
/// path, whose window-state save would write a new file into the folder it
/// just removed.
///
/// Code worktrees under `~/Tidebreak/workspaces` hold work on real branches
/// and stay, and so do the repositories they came from.
#[tauri::command]
pub(crate) async fn delete_all_data(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    attachment: State<'_, Arc<RemoteAttachment>>,
    confirmation: String,
) -> Result<(), String> {
    if confirmation.trim() != DELETE_ALL_DATA_PHRASE {
        return Err(format!("Type {DELETE_ALL_DATA_PHRASE} to confirm."));
    }
    if attachment.current().await.is_some() {
        return Err(format!(
            "{ATTACHED_ELSEWHERE} Detach from it first to delete this computer's data."
        ));
    }
    let data = crate::data_dir(&app)?;
    let Some(store) = host_access.store().cloned() else {
        return Err("Tidebreak is still starting. Try again in a moment.".to_owned());
    };
    // New turns stop starting and running ones get a moment to finish, so
    // an engine is not left mid-edit in a worktree. The deletion goes ahead
    // either way.
    if let Err(error) = host_access.quiesce_for_update().await {
        eprintln!("tidebreak-desktop: delete all data: work did not stop cleanly: {error}");
    }
    let keychain = match crate::channel::current().keychain_service() {
        Some(service) => KeychainSecretProvider::with_service(service),
        None => KeychainSecretProvider::new(),
    };
    if let Err(error) =
        tidebreak_server::secret_rehome::erase_stored_secrets(store.as_ref(), &keychain).await
    {
        let _ = host_access.resume_after_failed_update().await;
        return Err(format!(
            "Tidebreak could not remove your keys from the keychain, so it deleted nothing \
             else: {error}"
        ));
    }
    if let Some(runtime) =
        app.try_state::<Arc<crate::computer_runtime_adapter::DesktopComputerRuntime>>()
    {
        runtime.shutdown().await;
    }
    host_access.shutdown().await;
    let mut folders = vec![data];
    let paths = app.path();
    folders.extend(
        [
            paths.app_cache_dir(),
            paths.app_config_dir(),
            paths.app_local_data_dir(),
            paths.app_log_dir(),
        ]
        .into_iter()
        .flatten(),
    );
    let failures = remove_profile_folders(&folders);
    tidebreak_server::logging::shutdown();
    for failure in failures {
        eprintln!("tidebreak-desktop: delete all data: {failure}");
    }
    std::process::exit(0);
}

/// Remove each folder, the first one being the data folder. Each is renamed
/// aside before it is deleted, so a writer that still holds a path into it
/// cannot put a file back where the next launch would find it, and it is
/// removed again if something recreated it meanwhile. Folders are removed
/// once each, however many names point at them. Returns what could not be
/// removed.
fn remove_profile_folders(folders: &[PathBuf]) -> Vec<String> {
    let mut failures = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for folder in folders {
        let canonical = folder.canonicalize().unwrap_or_else(|_| folder.clone());
        if seen.contains(&canonical) {
            continue;
        }
        seen.push(canonical);
        if std::fs::symlink_metadata(folder).is_err() {
            continue;
        }
        let aside = folder.with_file_name(format!(
            ".{}.deleting-{}",
            folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            uuid::Uuid::new_v4()
        ));
        let target = match std::fs::rename(folder, &aside) {
            Ok(()) => aside,
            Err(_) => folder.clone(),
        };
        if let Err(error) = std::fs::remove_dir_all(&target) {
            failures.push(format!("could not remove {}: {error}", folder.display()));
        }
        if std::fs::symlink_metadata(folder).is_ok() {
            if let Err(error) = std::fs::remove_dir_all(folder) {
                failures.push(format!("could not remove {}: {error}", folder.display()));
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_profile_folder_goes_and_nothing_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("io.example.tidebreak");
        let cache = root.path().join("Caches").join("io.example.tidebreak");
        let neighbor = root.path().join("io.example.other");
        for folder in [&data, &cache, &neighbor] {
            std::fs::create_dir_all(folder.join("nested")).unwrap();
            std::fs::write(folder.join("nested").join("file"), b"bytes").unwrap();
        }
        let missing = root.path().join("never-created");

        let failures =
            remove_profile_folders(&[data.clone(), cache.clone(), data.clone(), missing]);

        assert!(failures.is_empty(), "{failures:?}");
        assert!(!data.exists());
        assert!(!cache.exists());
        assert!(neighbor.join("nested").join("file").exists());
        let leftovers: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(leftovers.len(), 2, "{leftovers:?}");
    }
}
