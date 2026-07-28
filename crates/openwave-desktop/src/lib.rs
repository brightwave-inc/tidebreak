//! Tauri host for OpenWave.
//!
//! On launch the shell binds [`openwave_server`] to an ephemeral loopback port,
//! then exposes the address and per-launch bearer token to the webview via the
//! `server_info` command. The UI talks to that local API over HTTP and
//! WebSocket (subprotocol auth for the browser upgrade).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::Manager;
use tokio::sync::watch;

use openwave_core::Config;

mod attachments;
mod broker;
mod client_execution;
mod deep_link;
mod deliverables;
mod documents;
mod host_access;
mod image_attachments;
mod media_type;
mod updater;

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

/// What booting the embedded server produced: the bound server info, or the
/// boot error. Delivered over the same channel the success case uses so the
/// renderer can display the actual cause — a GUI launch has no visible
/// stderr, and every failure (store error, keychain, instance lock) used to
/// collapse into a bare "server failed to start".
type BootOutcome = Result<NativeServerInfo, String>;

struct AppState {
    /// Filled once the accept loop is bound or boot has failed; awaited by
    /// `server_info`.
    info_rx: watch::Receiver<Option<BootOutcome>>,
}

/// Return the bound server address and token (waits until bind completes).
#[tauri::command]
async fn server_info(state: tauri::State<'_, Arc<AppState>>) -> Result<ServerInfo, String> {
    Ok(wait_server_info(state.inner()).await?.renderer_info())
}

/// Best-effort attention hint for a newly parked user question.
///
/// Durable server state and renderer recovery remain authoritative; failure to
/// notify never changes or acknowledges a question.
#[tauri::command]
fn request_user_attention(window: tauri::WebviewWindow) {
    if window.is_focused().unwrap_or(true) {
        return;
    }
    let _ = window.request_user_attention(Some(tauri::UserAttentionType::Informational));
}

async fn wait_server_info(state: &Arc<AppState>) -> Result<NativeServerInfo, String> {
    let mut rx = state.info_rx.clone();
    loop {
        if let Some(outcome) = rx.borrow().clone() {
            return outcome;
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
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut context = tauri::generate_context!();
    // Debug builds run under a distinct identifier so `cargo tauri dev` holds its
    // own single-instance lock and app-data dir instead of colliding with an
    // installed release build.
    #[cfg(debug_assertions)]
    {
        context.config_mut().identifier = "io.brightwave.openwave.dev".into();
    }

    let (info_tx, info_rx) = watch::channel(None);
    let state = Arc::new(AppState { info_rx });
    // Filled once the embedded server binds; the deep-link pairing handler
    // waits on it, because a provision link often launches the app.
    let (store_tx, store_rx) = watch::channel(None);

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        // On Windows and Linux an `openwave://` link opens a second instance
        // with the link as its argument. The plugin's `deep-link` feature has
        // already forwarded `_args` to the deep-link plugin (raising the same
        // open-URL event macOS delivers natively) before this callback runs,
        // so the callback itself only surfaces the window.
        deep_link::focus_main_window(app);
    }));

    let app = builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(state)
        .manage(deep_link::PairingStore::new(store_rx))
        .manage(documents::PendingLibraryDrop::default())
        .manage(updater::UpdateManager::default())
        .invoke_handler(tauri::generate_handler![
            server_info,
            request_user_attention,
            attachments::attach_chat_files,
            image_attachments::publish_chat_image,
            documents::import_library_document,
            documents::import_library_documents,
            documents::import_dropped_library_documents,
            documents::list_library_documents,
            documents::search_library_documents,
            documents::delete_library_document,
            documents::retry_library_document,
            documents::export_library_document,
            deliverables::list_deliverables,
            deliverables::read_deliverable,
            deliverables::export_deliverable,
            client_execution::resolve_folder_access_request,
            client_execution::output_writeback::resolve_output_writeback_request,
            host_access::connect_folder,
            host_access::connect_approved_folder,
            host_access::list_approved_folders,
            host_access::list_connected_folders,
            host_access::disconnect_folder,
            updater::desktop_update_state,
            updater::check_for_update,
            updater::restart_for_update
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            point_pdfium_at_bundle(&handle);
            deep_link::install(&handle);
            updater::spawn_update_loop(handle.clone());
            let data = data_dir(&handle)?;
            let home = home_dir(&handle)?;
            let host_access = host_access::HostAccess::new(handle.clone(), data.clone(), home)?;
            app.manage(host_access);

            tauri::async_runtime::spawn(async move {
                if let Err(error) = boot_server(handle, &info_tx, store_tx, data.clone()).await {
                    // stderr for terminal launches, the app-data log for
                    // GUI launches, and the watch channel for the window.
                    eprintln!("openwave-desktop: {error}");
                    log_boot_failure(&data, &error);
                    let _ = info_tx.send(Some(Err(error)));
                }
            });
            Ok(())
        })
        .build(context)
        .expect("error while building OpenWave");
    app.run(|app, event| match event {
        tauri::RunEvent::WindowEvent { label, event, .. } => {
            documents::handle_window_drag_drop(app, &label, &event);
        }
        tauri::RunEvent::Exit => {
            tauri::async_runtime::block_on(app.state::<host_access::HostAccess>().shutdown());
        }
        _ => {}
    });
}

/// Append a server failure — a boot that never bound, or a later death of
/// the accept loop — to `boot-failures.log` under the app-data dir, so a
/// GUI-launched app leaves a diagnosable trace without a terminal relaunch.
/// Best-effort: logging must never mask the failure being logged.
fn log_boot_failure(data_dir: &Path, error: &str) {
    use std::io::Write;
    let line = format!("{} {error}\n", chrono::Local::now().to_rfc3339());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("boot-failures.log"))
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

/// Bind the local API and park the accept loop for the life of the process.
async fn boot_server(
    app: tauri::AppHandle,
    info_tx: &watch::Sender<Option<BootOutcome>>,
    store_tx: watch::Sender<Option<Arc<dyn openwave_core::Store>>>,
    data_dir: PathBuf,
) -> Result<(), String> {
    let client_executor_id = app.state::<host_access::HostAccess>().client_executor_id();
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut config = Config::desktop(data_dir);
    // Debug builds keep their own keychain service, completing the identifier
    // and app-data split: dev and release must not share mutable secret state,
    // and items created by a dev-signed binary fail the release app's keychain
    // ACL check anyway.
    #[cfg(debug_assertions)]
    {
        config.keychain_service = Some("openwave.dev".into());
    }
    let server = openwave_server::bind_configured_with_desktop_executor(config, client_executor_id)
        .await
        .map_err(|e| e.to_string())?;
    app.state::<host_access::HostAccess>()
        .initialize_store(server.store())?;
    // Unblock any pairing task parked on a deep link that arrived pre-boot.
    let _ = store_tx.send(Some(server.store()));
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
    let _ = info_tx.send(Some(Ok(info)));
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
    let output_writeback_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::output_writeback::recover_output_writebacks(output_writeback_app).await;
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

    #[tokio::test]
    async fn wait_server_info_returns_the_published_boot_error() {
        let (tx, rx) = watch::channel(None);
        let state = Arc::new(AppState { info_rx: rx });
        tx.send(Some(Err("store error: migration file missing".to_string())))
            .unwrap();
        let error = wait_server_info(&state)
            .await
            .err()
            .expect("the published boot error reaches the renderer");
        assert_eq!(error, "store error: migration file missing");
    }

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
