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
mod documents;
mod host_access;

/// Connection details the webview needs to reach the in-process API.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Base URL, e.g. `http://127.0.0.1:54321`.
    pub base_url: String,
    /// Per-launch bearer token.
    pub token: String,
}

#[derive(Clone)]
struct NativeServerInfo {
    base_url: String,
    token: String,
    executor_token: String,
}

impl NativeServerInfo {
    fn renderer_info(&self) -> ServerInfo {
        ServerInfo {
            base_url: self.base_url.clone(),
            token: self.token.clone(),
        }
    }
}

struct AppState {
    /// Filled once the accept loop is bound; awaited by `server_info`.
    info_rx: watch::Receiver<Option<NativeServerInfo>>,
}

/// Return the bound server address and token (waits until bind completes).
#[tauri::command]
async fn server_info(state: tauri::State<'_, Arc<AppState>>) -> Result<ServerInfo, String> {
    Ok(wait_server_info(state.inner()).await?.renderer_info())
}

async fn wait_server_info(state: &Arc<AppState>) -> Result<NativeServerInfo, String> {
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

/// Platform file name of the PDFium shared library the liteparse parser loads.
fn pdfium_dylib_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "pdfium.dll"
    } else if cfg!(target_os = "macos") {
        "libpdfium.dylib"
    } else {
        "libpdfium.so"
    }
}

/// Point the PDFium loader at the library bundled next to the packaged app.
///
/// The build script stages `pdfium/<lib>` into the Tauri resource directory for
/// release bundles. `liteparse-pdfium-sys` searches `PDFIUM_LIB_PATH` first, so
/// exporting the bundled directory makes PDF parsing work in an installed app,
/// where the compile-time cache path baked into the binary does not exist.
///
/// Best-effort and idempotent: an explicit `PDFIUM_LIB_PATH` is respected, and a
/// missing resource (dev runs, or a bundle built before the runtime was staged)
/// is left alone so the loader falls through to its other search paths. The
/// parser still fails closed with a clear message if PDFium cannot be loaded.
fn point_pdfium_at_bundle(app: &tauri::AppHandle) {
    if std::env::var_os("PDFIUM_LIB_PATH").is_some() {
        return;
    }
    let Ok(resource_dir) = app.path().resource_dir() else {
        return;
    };
    let pdfium_dir = resource_dir.join("pdfium");
    if pdfium_dir.join(pdfium_dylib_name()).is_file() {
        std::env::set_var("PDFIUM_LIB_PATH", &pdfium_dir);
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
            documents::import_library_document,
            documents::list_library_documents,
            documents::search_library_documents,
            client_execution::resolve_folder_access_request,
            host_access::connect_folder,
            host_access::list_connected_folders,
            host_access::disconnect_folder
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            point_pdfium_at_bundle(&handle);
            let data = data_dir(&handle)?;
            let home = home_dir(&handle)?;
            let host_access = host_access::HostAccess::new(handle.clone(), data.clone(), home)?;
            app.manage(host_access);

            tauri::async_runtime::spawn(async move {
                if let Err(error) = boot_server(handle, info_tx, data).await {
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
    info_tx: watch::Sender<Option<NativeServerInfo>>,
    data_dir: PathBuf,
) -> Result<(), String> {
    let client_executor_id = app.state::<host_access::HostAccess>().client_executor_id();
    let server =
        openwave_server::bind_with_desktop_executor(Config::desktop(data_dir), client_executor_id)
            .await
            .map_err(|e| e.to_string())?;
    app.state::<host_access::HostAccess>()
        .initialize_store(server.store())?;
    let base_url = format!("http://{}", server.local_addr());
    let token = server.token().to_string();
    let executor_token = server.client_executor_token().to_string();
    app.state::<host_access::HostAccess>()
        .initialize_control_plane(base_url.clone(), token.clone(), executor_token.clone())?;
    let info = NativeServerInfo {
        base_url,
        token,
        executor_token,
    };
    let _ = info_tx.send(Some(info));
    let recovery_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::recover_folder_access_receipts(recovery_app).await;
    });
    let folder_operation_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::folder_operations::recover_connected_folder_operations(
            folder_operation_app,
        )
        .await;
    });
    let delegated_file_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::delegated_file_read::recover_delegated_file_read(delegated_file_app)
            .await;
    });
    let root_attachment_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::root_attachment_reconciliation::recover_root_attachment_changes(
            root_attachment_app,
        )
        .await;
    });
    server.serve().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod server_info_tests {
    use super::*;

    #[test]
    fn renderer_server_info_never_contains_native_credential() {
        let native = NativeServerInfo {
            base_url: "http://127.0.0.1:1234".to_owned(),
            token: "renderer-bearer".to_owned(),
            executor_token: "native-credential-sentinel".to_owned(),
        };
        let serialized = serde_json::to_string(&native.renderer_info()).unwrap();
        assert!(serialized.contains("renderer-bearer"));
        assert!(!serialized.contains("native-credential-sentinel"));
        assert!(!serialized.contains("executor"));
    }
}
