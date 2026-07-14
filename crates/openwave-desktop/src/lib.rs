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

/// Connection details the webview needs to reach the in-process API.
#[derive(Clone, Debug, Serialize)]
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

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![server_info])
        .setup(move |app| {
            let handle = app.handle().clone();
            let data = data_dir(&handle)?;
            let scratch = private_scratch(&data)?;

            tauri::async_runtime::spawn(async move {
                if let Err(error) = boot_server(info_tx, data, scratch).await {
                    eprintln!("openwave-desktop: {error}");
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running OpenWave");
}

/// Bind the local API and park the accept loop for the life of the process.
async fn boot_server(
    info_tx: watch::Sender<Option<ServerInfo>>,
    data_dir: PathBuf,
    scratch_dir: PathBuf,
) -> Result<(), String> {
    let server = openwave_server::bind(Config::desktop(data_dir))
        .await
        .map_err(|e| e.to_string())?;
    let info = ServerInfo {
        base_url: format!("http://{}", server.local_addr()),
        token: server.token().to_string(),
        scratch_dir: scratch_dir.display().to_string(),
    };
    let _ = info_tx.send(Some(info));
    server.serve().await.map_err(|e| e.to_string())
}
