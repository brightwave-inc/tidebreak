//! Tauri host for OpenWave.
//!
//! On launch the shell binds [`openwave_server`] to an ephemeral loopback port,
//! then exposes the address and per-launch bearer token to the webview via the
//! `server_info` command. The UI talks to that local API over HTTP and
//! WebSocket (subprotocol auth for the browser upgrade).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::Manager;
use tokio::sync::watch;

use openwave_core::Config;

mod broker;
mod client_execution;
mod host_access;

/// Connection details the webview needs to reach the in-process API.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Base URL, e.g. `http://127.0.0.1:54321`.
    pub base_url: String,
    /// Per-launch bearer token.
    pub token: String,
    /// App-private scratch path used by legacy direct file tools until they are
    /// routed through the host broker.
    pub scratch_dir: String,
}

struct AppState {
    /// Filled once the accept loop is bound; awaited by `server_info`.
    info_rx: watch::Receiver<Option<ServerInfo>>,
}

/// Return the bound server address and token (waits until bind completes).
#[tauri::command]
async fn server_info(state: tauri::State<'_, Arc<AppState>>) -> Result<ServerInfo, String> {
    let mut rx = state.info_rx.clone();
    loop {
        if let Some(info) = rx.borrow().clone() {
            return Ok(info);
        }
        rx.changed()
            .await
            .map_err(|_| "server failed to start".to_string())?;
    }
}

/// Absolute data directory for the desktop profile (platform app-data).
fn data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create data dir: {e}"))?;
    Ok(dir)
}

fn home_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let home = app
        .path()
        .home_dir()
        .map_err(|e| format!("home dir: {e}"))?;
    home.canonicalize()
        .map_err(|e| format!("resolve home dir: {e}"))
}

/// Private scratch for the current desktop profile. This is operational app
/// data, not a connected user folder and never appears in a native picker.
fn private_scratch(data_dir: &std::path::Path) -> Result<PathBuf, String> {
    let scratch = data_dir.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create private scratch: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("restrict private scratch: {e}"))?;
    }
    Ok(scratch)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (info_tx, info_rx) = watch::channel(None);
    let state = Arc::new(AppState { info_rx });

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.unminimize();
            let _ = window.show();
            let _ = window.set_focus();
        }
    }));

    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            server_info,
            client_execution::resolve_folder_access_request,
            host_access::connect_folder,
            host_access::list_connected_folders,
            host_access::disconnect_folder
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let data = data_dir(&handle)?;
            let home = home_dir(&handle)?;
            let scratch = private_scratch(&data)?;
            let host_access = host_access::HostAccess::new(handle.clone(), data.clone(), home)?;
            app.manage(host_access);

            tauri::async_runtime::spawn(async move {
                if let Err(error) = boot_server(handle, info_tx, data, scratch).await {
                    eprintln!("openwave-desktop: {error}");
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building OpenWave");
    app.run(|app, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            tauri::async_runtime::block_on(app.state::<host_access::HostAccess>().shutdown());
        }
    });
}

/// Bind the local API and park the accept loop for the life of the process.
async fn boot_server(
    app: tauri::AppHandle,
    info_tx: watch::Sender<Option<ServerInfo>>,
    data_dir: PathBuf,
    scratch_dir: PathBuf,
) -> Result<(), String> {
    let server = openwave_server::bind(Config::desktop(data_dir))
        .await
        .map_err(|e| e.to_string())?;
    app.state::<host_access::HostAccess>()
        .initialize_store(server.store())?;
    let base_url = format!("http://{}", server.local_addr());
    let token = server.token().to_string();
    let executor_token = server.client_executor_token().to_string();
    app.state::<host_access::HostAccess>()
        .initialize_control_plane(base_url.clone(), token.clone(), executor_token)?;
    let info = ServerInfo {
        base_url,
        token,
        scratch_dir: scratch_dir.display().to_string(),
    };
    let _ = info_tx.send(Some(info));
    let recovery_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::recover_folder_access_receipts(recovery_app).await;
    });
    server.serve().await.map_err(|e| e.to_string())
}
