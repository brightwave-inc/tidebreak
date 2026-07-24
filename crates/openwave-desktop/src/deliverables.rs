//! Native bridge for conversation-private generated outputs.
//!
//! The model writes only to a closed directory inside private scratch. The
//! renderer receives bounded text and safe metadata, while a user-selected save
//! destination stays entirely inside this native boundary.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use chrono::{DateTime, Utc};
use openwave_core::{
    deliverable_media_type, validate_deliverable_name, ChatId, DELIVERABLES_DIRECTORY,
    MAX_DELIVERABLE_BYTES,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::documents::resolve_conversation_scope;
use crate::host_access::HostAccess;

const MAX_DELIVERABLES: usize = 100;
const MAX_DELIVERABLE_DIRECTORY_ENTRIES: usize = 1_000;
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
    filename: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliverableSummary {
    filename: String,
    media_type: String,
    size_bytes: u64,
    updated_at: String,
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
    filename: String,
    media_type: String,
    content: String,
    truncated: bool,
}

#[tauri::command]
pub(crate) async fn list_deliverables(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: DeliverablesRequest,
) -> Result<DeliverablesCatalog, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    let scratch_root = crate::data_dir(&app)?.join("scratch");
    tauri::async_runtime::spawn_blocking(move || list_deliverables_in_root(&scratch_root, chat_id))
        .await
        .map_err(|_| "Could not load this conversation's outputs".to_owned())?
}

#[tauri::command]
pub(crate) async fn read_deliverable(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: DeliverableRequest,
) -> Result<DeliverablePreview, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    validate_deliverable_name(&request.filename)
        .map_err(|_| "Invalid output filename".to_owned())?;
    let scratch_root = crate::data_dir(&app)?.join("scratch");
    let filename = request.filename;
    tauri::async_runtime::spawn_blocking(move || {
        let content = read_deliverable_bytes(&scratch_root, chat_id, &filename)?;
        preview_from_bytes(filename, content)
    })
    .await
    .map_err(|_| "Could not preview that output".to_owned())?
}

#[tauri::command]
pub(crate) async fn export_deliverable(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: DeliverableRequest,
) -> Result<bool, String> {
    let chat_id = resolve_conversation_scope(&host_access, request.chat_id).await?;
    validate_deliverable_name(&request.filename)
        .map_err(|_| "Invalid output filename".to_owned())?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let scratch_root = crate::data_dir(&app)?.join("scratch");
    let filename = request.filename;
    let read_filename = filename.clone();
    let content = tauri::async_runtime::spawn_blocking(move || {
        read_deliverable_bytes(&scratch_root, chat_id, &read_filename)
    })
    .await
    .map_err(|_| "Could not read that output".to_owned())??;

    let Some(destination) = pick_export_path(&app, &filename).await? else {
        return Ok(false);
    };
    // A picker may remain open while the selected conversation is deleted.
    // Revalidate before the one user-authorized host write.
    resolve_conversation_scope(&host_access, request.chat_id).await?;
    tauri::async_runtime::spawn_blocking(move || {
        write_exported_deliverable(&destination, &content)
    })
    .await
    .map_err(|_| "Could not save that output".to_owned())??;
    Ok(true)
}

fn list_deliverables_in_root(
    scratch_root: &Path,
    chat_id: ChatId,
) -> Result<DeliverablesCatalog, String> {
    let Some(directory) = open_deliverables_directory(scratch_root, chat_id)? else {
        return Ok(DeliverablesCatalog {
            deliverables: Vec::new(),
            truncated: false,
        });
    };
    let entries = directory
        .read_dir(".")
        .map_err(|_| "Could not load this conversation's outputs".to_owned())?;
    let mut deliverables = Vec::new();
    let mut inspected = 0usize;
    for entry in entries {
        inspected += 1;
        if inspected > MAX_DELIVERABLE_DIRECTORY_ENTRIES {
            return Err("This conversation has too many output files".to_owned());
        }
        let Ok(entry) = entry else {
            continue;
        };
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if validate_deliverable_name(&filename).is_err() {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_DELIVERABLE_BYTES as u64 {
            continue;
        }
        let Some(media_type) = deliverable_media_type(&filename) else {
            continue;
        };
        let updated_at = metadata
            .modified()
            .map(|time| DateTime::<Utc>::from(time.into_std()))
            .unwrap_or_else(|_| DateTime::<Utc>::UNIX_EPOCH)
            .to_rfc3339();
        deliverables.push(DeliverableSummary {
            filename,
            media_type: media_type.to_owned(),
            size_bytes: metadata.len(),
            updated_at,
        });
    }
    deliverables.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.filename.cmp(&right.filename))
    });
    let truncated = deliverables.len() > MAX_DELIVERABLES;
    deliverables.truncate(MAX_DELIVERABLES);
    Ok(DeliverablesCatalog {
        deliverables,
        truncated,
    })
}

fn open_deliverables_directory(
    scratch_root: &Path,
    chat_id: ChatId,
) -> Result<Option<Dir>, String> {
    let Some(root) = open_regular_directory(scratch_root)? else {
        return Ok(None);
    };
    let chat_name = chat_id.to_string();
    if !is_regular_child_directory(&root, &chat_name)? {
        return Ok(None);
    }
    let chat = root
        .open_dir_nofollow(&chat_name)
        .map_err(|_| "Could not open this conversation's private outputs".to_owned())?;
    if !is_regular_child_directory(&chat, DELIVERABLES_DIRECTORY)? {
        return Ok(None);
    }
    chat.open_dir_nofollow(DELIVERABLES_DIRECTORY)
        .map(Some)
        .map_err(|_| "Could not open this conversation's private outputs".to_owned())
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

fn read_deliverable_bytes(
    scratch_root: &Path,
    chat_id: ChatId,
    filename: &str,
) -> Result<Vec<u8>, String> {
    validate_deliverable_name(filename).map_err(|_| "Invalid output filename".to_owned())?;
    let directory = open_deliverables_directory(scratch_root, chat_id)?
        .ok_or_else(|| "Output not found".to_owned())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(filename, &options)
        .map_err(|_| "Output not found".to_owned())?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not read that output".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_DELIVERABLE_BYTES as u64 {
        return Err("Output is not a supported text file".to_owned());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_DELIVERABLE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read that output".to_owned())?;
    if bytes.len() > MAX_DELIVERABLE_BYTES
        || bytes.contains(&0)
        || std::str::from_utf8(&bytes).is_err()
    {
        return Err("Output is not a supported text file".to_owned());
    }
    Ok(bytes)
}

fn preview_from_bytes(filename: String, bytes: Vec<u8>) -> Result<DeliverablePreview, String> {
    let text =
        String::from_utf8(bytes).map_err(|_| "Output is not a supported text file".to_owned())?;
    if text.contains('\0') {
        return Err("Output is not a supported text file".to_owned());
    }
    let mut characters = text.chars();
    let content: String = characters.by_ref().take(MAX_PREVIEW_CHARACTERS).collect();
    let truncated = characters.next().is_some();
    let media_type = deliverable_media_type(&filename)
        .ok_or_else(|| "Output is not a supported text file".to_owned())?;
    Ok(DeliverablePreview {
        filename,
        media_type: media_type.to_owned(),
        content,
        truncated,
    })
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
    if !destination.is_absolute() || content.len() > MAX_DELIVERABLE_BYTES {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact_path(root: &Path, chat_id: ChatId, filename: &str) -> PathBuf {
        root.join(chat_id.to_string())
            .join(DELIVERABLES_DIRECTORY)
            .join(filename)
    }

    #[test]
    fn catalog_and_preview_are_exactly_conversation_scoped() {
        let scratch = tempfile::tempdir().unwrap();
        let first = ChatId::new();
        let second = ChatId::new();
        let first_path = artifact_path(scratch.path(), first, "brief.md");
        let second_path = artifact_path(scratch.path(), second, "other.txt");
        std::fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        std::fs::write(&first_path, "# Brief").unwrap();
        std::fs::write(&second_path, "private").unwrap();

        let catalog = list_deliverables_in_root(scratch.path(), first).unwrap();
        assert_eq!(catalog.deliverables.len(), 1);
        assert_eq!(catalog.deliverables[0].filename, "brief.md");
        assert!(!catalog.truncated);
        let preview = preview_from_bytes(
            "brief.md".to_owned(),
            read_deliverable_bytes(scratch.path(), first, "brief.md").unwrap(),
        )
        .unwrap();
        assert_eq!(preview.content, "# Brief");
        assert!(read_deliverable_bytes(scratch.path(), first, "other.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_catalog_and_export_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let scratch = tempfile::tempdir().unwrap();
        let chat_id = ChatId::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.md"), "secret").unwrap();
        let directory = artifact_path(scratch.path(), chat_id, "link.md");
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        symlink(outside.path().join("secret.md"), &directory).unwrap();
        assert!(read_deliverable_bytes(scratch.path(), chat_id, "link.md").is_err());

        let linked_chat = ChatId::new();
        symlink(outside.path(), scratch.path().join(linked_chat.to_string())).unwrap();
        assert!(list_deliverables_in_root(scratch.path(), linked_chat).is_err());

        let destination = outside.path().join("destination.md");
        let target = outside.path().join("target.md");
        std::fs::write(&target, "keep").unwrap();
        symlink(&target, &destination).unwrap();
        assert!(write_exported_deliverable(&destination, b"replace").is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "keep");
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
    fn renderer_projection_is_bounded_and_pathless() {
        let preview = preview_from_bytes(
            "brief.md".to_owned(),
            "x".repeat(MAX_PREVIEW_CHARACTERS + 5).into_bytes(),
        )
        .unwrap();
        assert_eq!(preview.content.chars().count(), MAX_PREVIEW_CHARACTERS);
        assert!(preview.truncated);
        let serialized = serde_json::to_value(preview).unwrap();
        assert_eq!(
            serialized
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["content", "filename", "mediaType", "truncated"]
        );
        for forbidden in ["path", "scratch", "chatId", "token"] {
            assert!(!serialized.to_string().contains(forbidden));
        }
    }

    #[test]
    fn output_reads_reject_non_text_content() {
        let scratch = tempfile::tempdir().unwrap();
        let chat_id = ChatId::new();
        let nul_path = artifact_path(scratch.path(), chat_id, "nul.txt");
        std::fs::create_dir_all(nul_path.parent().unwrap()).unwrap();
        std::fs::write(&nul_path, b"not\0text").unwrap();
        std::fs::write(artifact_path(scratch.path(), chat_id, "binary.txt"), [0xff]).unwrap();

        assert!(read_deliverable_bytes(scratch.path(), chat_id, "nul.txt").is_err());
        assert!(read_deliverable_bytes(scratch.path(), chat_id, "binary.txt").is_err());
    }

    #[test]
    fn requests_reject_scope_and_path_injection() {
        assert!(
            serde_json::from_value::<DeliverableRequest>(serde_json::json!({
                "chatId": Uuid::new_v4(),
                "filename": "brief.md",
                "path": "/tmp/private"
            }))
            .is_err()
        );
    }
}
