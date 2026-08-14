//! Tauri host for Tidebreak.
//!
//! On launch the shell binds [`tidebreak_server`] to an ephemeral loopback port,
//! then exposes the address and per-launch bearer token to the webview via the
//! `server_info` command. The UI talks to that local API over HTTP and
//! WebSocket (subprotocol auth for the browser upgrade).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::Manager;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio::sync::watch;
use unicode_general_category::{get_general_category, GeneralCategory};

use tidebreak_core::Config;

mod attachments;
mod broker;
mod channel;
mod chat_debug;
mod client_execution;
#[cfg(test)]
mod command_parity;
mod deep_link;
mod deliverables;
mod documents;
mod host_access;
mod image_attachments;
mod node_install;
mod office_install;
mod office_pdf;
#[cfg(target_os = "macos")]
mod office_sandbox;
mod updater;
mod voice_transcription;

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

/// Save MCP configuration through the native-only server surface. Command
/// transports receive an OS-native confirmation before the host credential is
/// attached; renderer JavaScript can request the prompt but cannot approve it.
#[tauri::command]
async fn put_native_mcp_servers(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    config: Value,
) -> Result<Value, String> {
    let commands = native_command_previews(&config)?;
    if !commands.is_empty() {
        let preview = commands.join("\n");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let mut dialog = app
            .dialog()
            .message(format!(
                "Allow these MCP servers to run local programs as your macOS user?\n\n{preview}"
            ))
            .title("Allow local MCP commands?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Allow and save".to_owned(),
                "Cancel".to_owned(),
            ));
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.parent(&window);
        }
        dialog.show(move |approved| {
            let _ = sender.send(approved);
        });
        if !receiver.await.unwrap_or(false) {
            return Err("local MCP command configuration was not approved".to_owned());
        }
    }

    let info = wait_server_info(state.inner()).await?;
    let response = documents::native_auth(
        documents::local_client()
            .put(format!("{}/native/mcp/servers", info.base_url))
            .json(&config),
        &info,
    )
    .send()
    .await
    .map_err(|error| format!("save MCP servers: {error}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| format!("read MCP server response: {error}"))?;
    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|body| {
                body.get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| format!("MCP server returned {status}"));
        return Err(message);
    }
    serde_json::from_slice(&body).map_err(|error| format!("decode MCP server response: {error}"))
}

fn native_command_previews(config: &Value) -> Result<Vec<String>, String> {
    let servers = config
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP configuration must contain a servers array".to_owned())?;
    let mut previews = Vec::new();
    for server in servers {
        if !server
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            continue;
        }
        let Some(command) = server.get("command").and_then(Value::as_str) else {
            continue;
        };
        let name = server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unnamed");
        let mut argv = vec![native_command_token(command)?];
        if let Some(args) = server.get("args") {
            let args = args
                .as_array()
                .ok_or_else(|| "MCP command args must be an array".to_owned())?;
            for argument in args {
                let argument = argument
                    .as_str()
                    .ok_or_else(|| "every MCP command argument must be a string".to_owned())?;
                argv.push(native_command_token(argument)?);
            }
        }
        let argv = serde_json::to_string(&argv).expect("string vectors serialize");
        previews.push(format!("{}: {argv}", native_security_label(name)));
    }
    Ok(previews)
}

fn native_command_token(value: &str) -> Result<String, String> {
    if value.chars().count() > 240 {
        return Err("MCP command or argument is too long to display safely".to_owned());
    }
    Ok(value
        .chars()
        .map(|character| {
            if matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect())
}

pub(crate) fn native_security_label(value: &str) -> String {
    value
        .chars()
        .take(160)
        .map(|character| {
            if matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
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

/// Resolve the directory Tauri stages bundle resources into.
///
/// Packaged builds use Tauri's normal resource path (`Contents/Resources` on
/// macOS, etc.). Dev builds usually resolve to the Cargo output directory next
/// to the binary, but Tauri only recognizes that layout when a path component
/// is literally named `target`. Custom `CARGO_TARGET_DIR` values such as
/// `~/.cache/tidebreak-target` fail that check, so `resource_dir()` returns
/// `unknown path` even though Tauri already staged `exec-scripts/`, `skills/`,
/// and `plugins/` beside the binary. Fall back to the executable directory
/// when it carries Cargo's `.cargo-lock` marker.
fn app_resource_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    match app.path().resource_dir() {
        Ok(dir) => Ok(dir),
        Err(error) => cargo_dev_resource_dir().ok_or_else(|| format!("app resource dir: {error}")),
    }
}

fn cargo_dev_resource_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    cargo_dev_resource_dir_from(exe_dir)
}

fn cargo_dev_resource_dir_from(exe_dir: &Path) -> Option<PathBuf> {
    exe_dir
        .join(".cargo-lock")
        .is_file()
        .then(|| exe_dir.to_path_buf())
}

fn exec_scripts_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    const REQUIRED_HELPERS: [&str; 13] = [
        "_tidebreak_preview.py",
        "_tidebreak_calc.py",
        "_tidebreak_ooxml.py",
        "render_pdf.py",
        "extract_pdf_figures.py",
        "render_office.py",
        "analyze_xlsx.py",
        "office_unpack.py",
        "office_pack.py",
        "pptx_clean.py",
        "calc_uno.py",
        "xlsx_recalc.py",
        "docx_clean.py",
    ];
    let directory = app_resource_dir(app)?.join("exec-scripts");
    for name in REQUIRED_HELPERS {
        if !directory.join(name).is_file() {
            return Err(format!("bundled exec document helper is missing: {name}"));
        }
    }
    Ok(directory)
}

/// The document skills every packaged build must carry; boot fails without
/// them, and the bundle test below pins the resource map that ships them.
const REQUIRED_SKILLS: [&str; 5] = [
    "charts",
    "pdf-documents",
    "presentations",
    "spreadsheets",
    "word-documents",
];

/// The plugins those skills are grouped into. Boot requires all three: a
/// plugin that fails to load takes its skills out of the grouped catalog
/// silently, which is exactly the kind of drift a packaged build should not
/// ship.
const REQUIRED_PLUGINS: [&str; 3] = ["charts", "documents", "spreadsheets"];

fn exec_skills_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let directory = app_resource_dir(app)?.join("skills");
    for name in REQUIRED_SKILLS {
        if !directory.join(name).join("SKILL.md").is_file() {
            return Err(format!("bundled document skill is missing: {name}"));
        }
    }
    Ok(directory)
}

fn exec_plugins_dir(app: &tauri::AppHandle, skills_dir: &Path) -> Result<PathBuf, String> {
    let directory = app_resource_dir(app)?.join("plugins");
    verify_required_plugins(skills_dir, &directory)?;
    Ok(directory)
}

/// Load both bundled trees the way the server will and check that the required
/// plugins are present and together cover every required skill.
///
/// Loading is what is checked, not file presence: a manifest that parses but
/// names a skill that did not load is skipped by the loader, and the resulting
/// gap would otherwise appear only as an ungrouped catalog at runtime.
fn verify_required_plugins(skills_dir: &Path, plugins_dir: &Path) -> Result<(), String> {
    let skills = tidebreak_code_execution::load_skills(
        skills_dir,
        tidebreak_code_execution::SkillOrigin::Builtin,
    );
    // The bundle ships no built-in prompts; a manifest claiming one would be
    // skipped, which this check would then see as a missing plugin.
    let plugins = tidebreak_code_execution::load_plugins(
        plugins_dir,
        &skills,
        &[],
        tidebreak_code_execution::PluginOrigin::Builtin,
    );
    let loaded: Vec<&str> = plugins
        .iter()
        .map(|plugin| plugin.package.name.as_str())
        .collect();
    for name in REQUIRED_PLUGINS {
        if !loaded.contains(&name) {
            return Err(format!(
                "bundled plugin '{name}' is missing or failed to load from {}; loaded: {loaded:?}",
                plugins_dir.display()
            ));
        }
    }
    let mut covered: Vec<&str> = plugins
        .iter()
        .flat_map(|plugin| plugin.package.skills.iter().map(String::as_str))
        .collect();
    covered.sort_unstable();
    if covered != REQUIRED_SKILLS {
        return Err(format!(
            "bundled plugins cover {covered:?}, but the required document skills are {REQUIRED_SKILLS:?}"
        ));
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut context = tauri::generate_context!();
    // Debug and staging each run under a distinct identifier so they hold
    // their own single-instance lock and app-data dir instead of colliding
    // with an installed release — or with each other.
    let channel = channel::current();
    if channel != channel::Channel::Production {
        let config = context.config_mut();
        config.identifier = channel.identifier().into();
        // The suffix is what the app menu and the frontend's `getName()`
        // report, so the UI titlebar follows it.
        config.product_name = Some(channel.product_name().into());
        if let Some(window) = config.app.windows.first_mut() {
            window.title = channel.product_name().into();
        }
        context.package_info_mut().name = channel.product_name().into();
    }

    let (info_tx, info_rx) = watch::channel(None);
    let state = Arc::new(AppState { info_rx });
    // Filled once the embedded server binds; the deep-link pairing handler
    // waits on it, because a provision link often launches the app.
    let (store_tx, store_rx) = watch::channel(None);

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        // On Windows and Linux an `tidebreak://` link opens a second instance
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
            put_native_mcp_servers,
            request_user_attention,
            attachments::attach_chat_files,
            attachments::attach_dropped_chat_files,
            image_attachments::publish_chat_image,
            documents::export_library_document,
            deliverables::export_deliverable,
            chat_debug::copy_chat_debug_bundle,
            chat_debug::save_chat_debug_bundle,
            office_pdf::convert_office_to_pdf,
            office_install::install_presentation_converter,
            office_install::cancel_presentation_converter_install,
            office_install::warm_presentation_converter,
            node_install::install_node_runtime,
            client_execution::resolve_folder_access_request,
            client_execution::output_writeback::resolve_output_writeback_request,
            client_execution::computer_use::computer_use_state,
            client_execution::computer_use::stop_computer_use_control,
            client_execution::computer_use::resume_computer_use_control,
            host_access::connect_folder,
            host_access::connect_approved_folder,
            host_access::list_approved_folders,
            host_access::list_capability_consents,
            host_access::revoke_capability_consent,
            host_access::list_connected_folders,
            host_access::grant_folder_capability,
            host_access::disconnect_folder,
            host_access::forget_folder,
            host_access::purge_deleted_conversation_subject,
            updater::desktop_update_state,
            updater::check_for_update,
            updater::restart_for_update
        ])
        .on_menu_event(updater::handle_menu_event)
        .setup(move |app| {
            // The dev server's origin is remote as far as the IPC is
            // concerned, so `tauri dev` needs it granted explicitly. Added
            // here rather than in `capabilities/` so a release binary cannot
            // carry it: a packaged app serves its frontend from Tauri's own
            // protocol and has no business trusting anything on loopback.
            #[cfg(debug_assertions)]
            app.add_capability(include_str!("../capabilities-dev/dev-server.json"))?;
            let handle = app.handle().clone();
            deep_link::install(&handle);
            #[cfg(target_os = "macos")]
            updater::install_update_menu(app)?;
            updater::spawn_update_loop(handle.clone());
            let data = data_dir(&handle)?;
            // Before anything that can warn: the embedded server's tracing
            // events land in `logs/tidebreak.log` under the profile data dir
            // (stderr-only if that file cannot be created).
            tidebreak_server::logging::init_logging(&data);
            let home = home_dir(&handle)?;
            let host_access = host_access::HostAccess::new(handle.clone(), data.clone(), home)?;
            app.manage(host_access);

            tauri::async_runtime::spawn(async move {
                if let Err(error) = boot_server(handle, &info_tx, store_tx, data.clone()).await {
                    // stderr for terminal launches, the app-data log for
                    // GUI launches, and the watch channel for the window.
                    eprintln!("tidebreak-desktop: {error}");
                    log_boot_failure(&data, &error);
                    let _ = info_tx.send(Some(Err(error)));
                }
            });
            Ok(())
        })
        .build(context)
        .expect("error while building Tidebreak");
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
    store_tx: watch::Sender<Option<tidebreak_server::PairingHandle>>,
    data_dir: PathBuf,
) -> Result<(), String> {
    let client_executor_id = app.state::<host_access::HostAccess>().client_executor_id();
    let mut config = Config::desktop(data_dir.clone());
    config.exec_scripts_dir = Some(exec_scripts_dir(&app)?);
    config.exec_skills_dir = Some(exec_skills_dir(&app)?);
    let skills_dir = config
        .exec_skills_dir
        .clone()
        .expect("skills dir was just set");
    config.exec_plugins_dir = Some(exec_plugins_dir(&app, &skills_dir)?);
    // The effective identifier — including the debug and staging overrides —
    // keys the macOS managed-preferences (MDM) domain the server reads policy from.
    config.bundle_id = Some(app.config().identifier.clone());
    // Non-production channels keep their own keychain service, completing the
    // identifier and app-data split: they must not share mutable secret state
    // with each other or with an installed release.
    if let Some(service) = channel::current().keychain_service() {
        config.keychain_service = Some(service.into());
    }
    let folder_grants = Arc::new(host_access::DesktopExecFolderGrantResolver::new(
        app.clone(),
    ));
    // The exec provider renders office outputs with the same managed/system
    // LibreOffice the preview panel converts with.
    let office_converter = Arc::new(office_pdf::ExecOfficeConverter::new(data_dir.clone()));
    // Skill-declared host tools warm and report through the managed installer.
    let host_tool_broker = Arc::new(office_install::DesktopHostToolBroker::new(app.clone()));
    let local_voice = Arc::new(voice_transcription::DesktopLocalVoiceRunner::new(
        data_dir.clone(),
    ));
    let server = tidebreak_server::bind_configured_with_desktop_executor_and_folder_grants(
        config,
        client_executor_id,
        folder_grants,
        Some(office_converter),
        Some(host_tool_broker),
        Some(local_voice),
        Some(Arc::new(host_access::DesktopHostFolders::new(app.clone()))),
    )
    .await
    .map_err(|e| e.to_string())?;
    app.state::<host_access::HostAccess>()
        .initialize_store(server.store())?;
    app.state::<host_access::HostAccess>()
        .initialize_staged_folders(server.staged_folders())?;
    let orphan_app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = orphan_app
            .state::<host_access::HostAccess>()
            .reconcile_orphaned_conversation_authority()
            .await
        {
            eprintln!(
                "tidebreak: could not purge orphaned host-broker conversation authority: {error}"
            );
        }
    });
    // Unblock any pairing task parked on a deep link that arrived pre-boot.
    let _ = store_tx.send(Some(server.pairing_handle()));
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
    let computer_use_app = app.clone();
    tauri::async_runtime::spawn(async move {
        client_execution::computer_use::recover_computer_use_operations(computer_use_app).await;
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
mod resource_dir_tests {
    use super::cargo_dev_resource_dir_from;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tidebreak-resource-dir-{label}-{nanos}"));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn cargo_dev_fallback_accepts_custom_target_dir_with_lock() {
        let dir = temp_dir("with-lock");
        fs::write(dir.join(".cargo-lock"), []).expect("lock");
        assert_eq!(
            cargo_dev_resource_dir_from(&dir).as_deref(),
            Some(dir.as_path())
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn cargo_dev_fallback_rejects_directories_without_cargo_lock() {
        let dir = temp_dir("without-lock");
        assert_eq!(cargo_dev_resource_dir_from(&dir), None);
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::{verify_required_plugins, REQUIRED_SKILLS};

    /// Packaged-style skill and plugin resolution, pinned at test time: the
    /// `tauri.conf.json` resource map must stage both trees into the app
    /// bundle, that skills tree must yield every skill `exec_skills_dir`
    /// requires, and the plugins tree must group all of them. Dropping a
    /// resource line or breaking a manifest would otherwise surface only as a
    /// packaged-app boot failure.
    #[test]
    fn bundled_resources_carry_all_required_skills() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let conf: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(manifest_dir.join("tauri.conf.json")).unwrap(),
        )
        .unwrap();
        let resources = conf["bundle"]["resources"]
            .as_object()
            .expect("tauri.conf.json maps bundle resources");
        let resource = |target: &str| {
            resources
                .iter()
                .find_map(|(source, mapped)| {
                    (mapped.as_str() == Some(target)).then(|| manifest_dir.join(source))
                })
                .unwrap_or_else(|| panic!("tauri.conf.json bundles a {target} resource"))
        };
        let skills_dir = resource("skills/");
        let skills = tidebreak_code_execution::load_skills(
            &skills_dir,
            tidebreak_code_execution::SkillOrigin::Builtin,
        );
        let names: Vec<&str> = skills
            .iter()
            .map(|skill| skill.package.name.as_str())
            .collect();
        assert_eq!(names, REQUIRED_SKILLS);
        verify_required_plugins(&skills_dir, &resource("plugins/")).unwrap();
    }
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

    #[test]
    fn native_security_labels_strip_format_controls_and_are_bounded() {
        let label = native_security_label(&format!("trusted\u{202e}\u{200b}{}", "x".repeat(200)));
        assert!(!label.contains('\u{202e}'));
        assert!(!label.contains('\u{200b}'));
        assert_eq!(label.chars().count(), 160);
        assert!(label.starts_with("trusted\u{fffd}\u{fffd}"));
    }

    #[test]
    fn only_enabled_local_commands_require_a_native_preview() {
        let config = serde_json::json!({
            "servers": [
                {"name": "draft", "command": "/bin/echo", "enabled": false},
                {"name": "remote", "url": "https://example.test/mcp", "enabled": true},
                {"name": "live", "command": "/bin/sh", "args": ["-c", "echo safe"], "enabled": true}
            ]
        });
        assert_eq!(
            native_command_previews(&config).unwrap(),
            vec![r#"live: ["/bin/sh","-c","echo safe"]"#]
        );
    }

    #[test]
    fn native_command_preview_refuses_arguments_it_cannot_show_completely() {
        let config = serde_json::json!({
            "servers": [{
                "name": "hidden suffix",
                "command": "/bin/sh",
                "args": ["-c", "x".repeat(241)],
                "enabled": true
            }]
        });
        assert!(native_command_previews(&config).is_err());
    }

    #[test]
    fn omitted_enabled_defaults_to_an_approved_command_and_every_command_is_shown() {
        let servers = (0..9)
            .map(|index| serde_json::json!({"name": format!("server-{index}"), "command": "/bin/true"}))
            .collect::<Vec<_>>();
        let previews = native_command_previews(&serde_json::json!({"servers": servers})).unwrap();
        assert_eq!(previews.len(), 9);
        assert!(previews[8].starts_with("server-8:"));
    }

    #[test]
    fn command_arguments_are_not_truncated_inside_the_displayable_bound() {
        let suffix = "DANGEROUS_SUFFIX";
        let argument = format!("{}{}", "x".repeat(180), suffix);
        let config = serde_json::json!({
            "servers": [{"name": "long", "command": "/bin/sh", "args": ["-c", argument]}]
        });
        let previews = native_command_previews(&config).unwrap();
        assert!(previews[0].contains(suffix));
    }
}
