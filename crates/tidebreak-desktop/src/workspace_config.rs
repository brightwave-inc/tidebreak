//! Native save and open dialogs for portable workspace configuration files.

use std::path::PathBuf;

use serde::Deserialize;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

use crate::host_access::HostAccess;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SaveWorkspaceConfigRequest {
    contents: String,
}

#[tauri::command]
pub(crate) async fn save_workspace_config(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
    request: SaveWorkspaceConfigRequest,
) -> Result<bool, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(destination) = crate::documents::pick_export_path(
        &app,
        "Save workspace configuration",
        "tidebreak-config.json",
    )
    .await?
    else {
        return Ok(false);
    };
    tokio::fs::write(&destination, request.contents.as_bytes())
        .await
        .map_err(|_| "Could not write the workspace configuration".to_owned())?;
    Ok(true)
}

#[tauri::command]
pub(crate) async fn pick_workspace_config(
    app: AppHandle,
    host_access: State<'_, HostAccess>,
) -> Result<Option<String>, String> {
    host_access
        .require_local(crate::host_authority::Authority::NativeExport)
        .await?;
    let _picker = host_access
        .picker
        .try_lock()
        .map_err(|_| "A file or folder picker is already open".to_owned())?;
    let Some(path) = pick_open_path(&app).await? else {
        return Ok(None);
    };
    let contents = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| "Could not read the workspace configuration".to_owned())?;
    Ok(Some(contents))
}

async fn pick_open_path(app: &AppHandle) -> Result<Option<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt as _;

    let (tx, rx) = oneshot::channel();
    let mut picker = app
        .dialog()
        .file()
        .set_title("Import workspace configuration")
        .add_filter("Tidebreak configuration", &["json"]);
    if let Some(window) = app.get_window("main") {
        picker = picker.set_parent(&window);
    }
    picker.pick_file(move |path| {
        let _ = tx.send(path);
    });
    rx.await
        .map_err(|_| "The file dialog closed unexpectedly".to_owned())?
        .map(tauri_plugin_dialog::FilePath::into_path)
        .transpose()
        .map_err(|_| "The file dialog returned an invalid path".to_owned())
}
