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
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
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
/// Answers `false` when the person cancels the native confirmation, and an
/// error when nothing was deleted. On success the process exits and the
/// promise never settles.
///
/// The renderer's typed phrase shows intent, but a renderer script could send
/// it, so this command asks again in a native dialog before it uses host
/// authority (decision 27). Then, in order:
///
/// 1. Every agent stops. If one will not, nothing is deleted.
/// 2. The keychain items go. If one will not, nothing else is deleted.
/// 3. The embedded server and its workers stop, so nothing refreshes a token
///    or writes a file from here on, and every code browser closes.
/// 4. The browsers' website data stores, the data, cache, settings, and log
///    folders, and the app window's own website data go.
/// 5. The keychain items go again, last, in case anything wrote one back.
///
/// The process then exits at once, without the usual quit path, whose
/// window-state save would write a new file into the folder it just removed.
/// A step after the keys that fails is reported in a native dialog first.
///
/// Code worktrees under `~/Tidebreak/workspaces` hold work on real branches
/// and stay, and so do the repositories they came from. Worktrees from before
/// version 0.59 live inside the data folder, so the confirmation names them.
#[tauri::command]
pub(crate) async fn delete_all_data(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    attachment: State<'_, Arc<RemoteAttachment>>,
    confirmation: String,
) -> Result<bool, String> {
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
    let folders = profile_folders(&app, data.clone());
    refuse_linked_folders(&folders)?;
    let channel = crate::channel::current();
    let message = delete_all_data_message(
        legacy_worktree_count(&data),
        channel.keychain_service().is_none(),
    );
    if !confirm_natively(&app, message).await? {
        return Ok(false);
    }

    if let Err(error) = host_access.stop_for_quit().await {
        host_access.resume_after_cancelled_quit();
        return Err(format!(
            "Tidebreak could not stop every agent, so it deleted nothing. Stop them and try \
             again. {error}"
        ));
    }
    let keychain = match channel.keychain_service() {
        Some(service) => KeychainSecretProvider::with_service(service),
        None => KeychainSecretProvider::new(),
    };
    let keys = match tidebreak_server::secret_rehome::erasable_secret_keys(store.as_ref()).await {
        Ok(keys) => keys,
        Err(error) => {
            host_access.resume_after_cancelled_quit();
            return Err(format!(
                "Tidebreak could not list your keys, so it deleted nothing: {error}"
            ));
        }
    };
    if let Err(error) = tidebreak_server::secret_rehome::erase_secret_keys(&keychain, &keys).await {
        host_access.resume_after_cancelled_quit();
        return Err(format!(
            "Tidebreak could not remove your keys from the keychain, so it deleted nothing \
             else: {error}"
        ));
    }

    // From here on the deletion goes through. What fails is reported below.
    let mut failures = Vec::new();
    if let Err(error) = host_access.stop_server().await {
        failures.push(error);
    }
    if let Err(error) = crate::code_browser::remove_all_browser_data(&app).await {
        failures.push(format!("the code browser's website data: {error}"));
    }
    if let Some(runtime) =
        app.try_state::<Arc<crate::computer_runtime_adapter::DesktopComputerRuntime>>()
    {
        runtime.shutdown().await;
    }
    host_access.shutdown().await;
    tidebreak_server::logging::shutdown();
    failures.extend(remove_profile_folders(&folders));
    if let Err(error) = crate::code_browser::remove_default_website_data(&app).await {
        failures.push(format!("the app window's website data: {error}"));
    }
    if let Err(error) = tidebreak_server::secret_rehome::erase_secret_keys(&keychain, &keys).await {
        failures.push(format!("a keychain item: {error}"));
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("tidebreak-desktop: delete all data: {failure}");
        }
        report_leftovers(&app, &failures).await;
    }
    std::process::exit(0);
}

/// Every folder this computer keeps for Tidebreak, the data folder first.
fn profile_folders(app: &AppHandle, data: PathBuf) -> Vec<PathBuf> {
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
    folders
}

/// Refuse before anything is deleted when a profile folder is a link: only
/// the link would go, and the data it points at would stay.
fn refuse_linked_folders(folders: &[PathBuf]) -> Result<(), String> {
    for folder in folders {
        let linked = std::fs::symlink_metadata(folder)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !linked {
            continue;
        }
        let target = std::fs::read_link(folder)
            .map(|target| target.display().to_string())
            .unwrap_or_else(|_| "another folder".to_owned());
        return Err(format!(
            "{} is a link to {target}, so deleting it would leave your data where it is. \
             Replace the link with the folder it points to, then try again. Nothing was deleted.",
            folder.display()
        ));
    }
    Ok(())
}

/// How many worktrees from before version 0.59 live inside the data folder,
/// under `code/worktrees/<repository>/<workspace>`.
fn legacy_worktree_count(data: &Path) -> usize {
    // Where the server kept every worktree before the root moved out of the
    // data folder (the server's `data_dir_worktree_root`).
    let root = data.join("code").join("worktrees");
    let Ok(repositories) = std::fs::read_dir(root) else {
        return 0;
    };
    repositories
        .flatten()
        .filter_map(|repository| std::fs::read_dir(repository.path()).ok())
        .map(|workspaces| workspaces.flatten().count())
        .sum()
}

/// What the native confirmation says. Every clause is true for this build
/// and this data folder.
fn delete_all_data_message(legacy_worktrees: usize, shares_cli_keychain: bool) -> String {
    let mut message = String::from(
        "Tidebreak stops every agent, deletes every conversation, memory, setting, attachment, \
         output, log, and backup on this computer, removes your keys from the keychain, and \
         quits. This cannot be undone.",
    );
    if shares_cli_keychain {
        message.push_str(
            "\n\nThe tidebreak command-line tool keeps its keys in the same keychain item, so it \
             is signed out too.",
        );
    }
    if legacy_worktrees == 0 {
        message.push_str("\n\nCode worktrees in ~/Tidebreak and your repositories stay.");
    } else {
        let worktrees = if legacy_worktrees == 1 {
            "1 worktree".to_owned()
        } else {
            format!("{legacy_worktrees} worktrees")
        };
        message.push_str(&format!(
            "\n\n{worktrees} from before version 0.59 live in the data folder, so they are \
             deleted too, with any work in them you have not pushed. Worktrees in ~/Tidebreak \
             and your repositories stay."
        ));
    }
    message
}

/// Ask in a native dialog. `true` when the person chose to delete.
async fn confirm_natively(app: &AppHandle, message: String) -> Result<bool, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(message)
        .title("Delete all Tidebreak data?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Delete all data".to_owned(),
            "Cancel".to_owned(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show(move |confirmed| {
        let _ = sender.send(confirmed);
    });
    receiver
        .await
        .map_err(|_| "The confirmation closed before you answered.".to_owned())
}

/// Say what a deletion that went through could not remove, before the app
/// quits.
async fn report_leftovers(app: &AppHandle, failures: &[String]) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(format!(
            "Tidebreak deleted your data, but some of it is still on this computer. Remove it \
             yourself:\n\n{}",
            failures.join("\n")
        ))
        .title("Some data is still here")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCustom("Quit".to_owned()))
        .show(move |_| {
            let _ = sender.send(());
        });
    let _ = receiver.await;
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

    #[cfg(unix)]
    #[test]
    fn a_linked_profile_folder_stops_the_deletion_before_it_starts() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("elsewhere");
        std::fs::create_dir_all(&real).unwrap();
        let data = root.path().join("io.example.tidebreak");
        std::os::unix::fs::symlink(&real, &data).unwrap();
        let cache = root.path().join("cache");
        std::fs::create_dir_all(&cache).unwrap();

        let refused = refuse_linked_folders(&[cache.clone(), data.clone()]).unwrap_err();
        assert!(refused.contains(&real.display().to_string()), "{refused}");
        assert!(refused.ends_with("Nothing was deleted."), "{refused}");
        assert!(refuse_linked_folders(&[cache]).is_ok());
    }

    #[test]
    fn worktrees_from_before_0_59_are_counted_and_named() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(legacy_worktree_count(root.path()), 0);
        let worktrees = root.path().join("code").join("worktrees");
        for workspace in ["repo-a/ws-1", "repo-a/ws-2", "repo-b/ws-3"] {
            std::fs::create_dir_all(worktrees.join(workspace)).unwrap();
        }
        assert_eq!(legacy_worktree_count(root.path()), 3);

        let message = delete_all_data_message(3, false);
        assert!(
            message.contains("3 worktrees from before version 0.59"),
            "{message}"
        );
        assert!(!message.contains("command-line"), "{message}");
        let message = delete_all_data_message(0, true);
        assert!(
            message.contains("Code worktrees in ~/Tidebreak"),
            "{message}"
        );
        assert!(message.contains("command-line tool"), "{message}");
    }
}
