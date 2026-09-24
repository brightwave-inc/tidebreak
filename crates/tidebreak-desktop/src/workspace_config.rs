//! Native save and open dialogs for portable workspace configuration files,
//! and the native path for applying an import.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

use tidebreak_server::workspace_config::{local_commands_to_confirm, WorkspaceConfigApplyRequest};

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

/// Apply an imported workspace configuration through the native-only server
/// surface (decision 27).
///
/// When the import would start local MCP commands, an OS dialog lists them
/// first, and only an affirmative answer forwards the request with the
/// client-executor credential. The request is parsed here and forwarded as
/// parsed, and the list comes from the same resolution the server's apply
/// uses, so the dialog names exactly the commands the import writes. An
/// import that starts nothing is forwarded without a dialog.
///
/// The request carries the program the dialog showed for each bare command
/// as its approved program. Whatever the renderer sent in that field is
/// replaced, so the approved program only ever comes from the dialog.
#[tauri::command]
pub(crate) async fn apply_native_workspace_config(
    app: AppHandle,
    state: State<'_, Arc<crate::AppState>>,
    request: Value,
) -> Result<Value, String> {
    let mut request: WorkspaceConfigApplyRequest = serde_json::from_value(request)
        .map_err(|error| format!("The import request is not valid: {error}"))?;
    request.approved_executables.clear();
    let commands = local_commands_to_confirm(&request);
    if !commands.is_empty() {
        let config = serde_json::json!({ "servers": commands });
        let Some(resolved) =
            crate::approve_local_mcp_commands(&app, &config, "Allow and import").await?
        else {
            return Err(
                "You did not allow the local MCP commands, so nothing was imported.".to_owned(),
            );
        };
        request.approved_executables = crate::approved_bare_commands(&resolved);
    }

    let info = crate::wait_server_info(state.inner()).await?;
    let response = crate::documents::native_auth(
        crate::documents::local_client()
            .post(format!("{}/native/workspace-config/apply", info.base_url))
            .json(&request),
        &info,
    )
    .send()
    .await
    .map_err(|error| format!("import workspace configuration: {error}"))?;
    crate::native_json_response(response, "workspace configuration import").await
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
