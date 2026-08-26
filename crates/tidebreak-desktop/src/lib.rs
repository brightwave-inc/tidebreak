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
#[allow(
    dead_code,
    reason = "the staged browser bridge is test-covered and will be wired in #2339 and #2340"
)]
mod browser_control;
mod browser_runtime_adapter;
#[allow(
    dead_code,
    reason = "the staged browser bridge is test-covered and will be wired in #2339 and #2340"
)]
mod browser_semantics;
mod channel;
mod chat_debug;
mod client_execution;
mod code_browser;
mod code_worktree;
#[cfg(test)]
mod command_parity;
mod deep_link;
mod deliverables;
mod documents;
mod host_access;
mod host_authority;
mod image_attachments;
mod menu;
mod node_install;
mod office_install;
mod office_pdf;
#[cfg(target_os = "macos")]
mod office_sandbox;
mod remote;
mod skill_import;
mod trusted_folders;
mod updater;
mod voice_transcription;

/// Connection details the webview needs to reach the API it is attached to.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Base URL, e.g. `http://127.0.0.1:54321`.
    pub base_url: String,
    /// Bearer token for that base URL.
    pub token: String,
    /// Which machine this is: the embedded server, or a remote one the user
    /// attached to. Host authority exists only on the local machine, so every
    /// caller that reaches the host branches on this.
    pub attachment: remote::Attachment,
    /// Whether the bearer is a short-lived Model Gateway resource token the
    /// shell can refresh from its existing OAuth session.
    pub gateway_auth: bool,
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
            attachment: remote::Attachment::Local,
            gateway_auth: false,
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

/// Return the API the renderer should use.
///
/// A remote attachment wins and does not wait for the embedded server: the
/// whole point of attaching to another machine is that this one's server is
/// not what the user is working on, and a local boot failure must not keep a
/// remote client off a machine that is running fine.
#[tauri::command]
async fn server_info(
    state: tauri::State<'_, Arc<AppState>>,
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
    pairing: tauri::State<'_, deep_link::PairingStore>,
) -> Result<ServerInfo, String> {
    if let Some(attached) = attachment.current().await {
        let (token, gateway_auth) = remote_token(&attached, &pairing).await?;
        return Ok(ServerInfo {
            base_url: attached.base_url,
            token,
            attachment: remote::Attachment::Remote,
            gateway_auth,
        });
    }
    Ok(wait_server_info(state.inner()).await?.renderer_info())
}

async fn remote_token(
    attached: &remote::Attached,
    pairing: &deep_link::PairingStore,
) -> Result<(String, bool), String> {
    match &attached.auth {
        remote::AttachedAuth::StaticToken(token) => Ok((token.clone(), false)),
        remote::AttachedAuth::Gateway { gateway_url } => {
            let resource = tidebreak_core::config::tidebreak_machine_resource(&attached.base_url);
            pairing
                .handle()
                .await?
                .hosted_tidebreak_access_token(gateway_url, &resource)
                .await
                .map(|token| (token, true))
                .map_err(|error| error.to_string())
        }
    }
}

/// Return a fresh credential for the currently attached machine. Gateway mode
/// rotates through the existing desktop OAuth session; static mode returns the
/// legacy stored bearer.
#[tauri::command]
async fn remote_machine_access_token(
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
    pairing: tauri::State<'_, deep_link::PairingStore>,
) -> Result<String, String> {
    let attached = attachment
        .current()
        .await
        .ok_or_else(|| "this window is not attached to a remote machine".to_string())?;
    remote_token(&attached, &pairing)
        .await
        .map(|(token, _)| token)
}

/// Report which machine this client is attached to.
#[tauri::command]
async fn remote_machine_state(
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
) -> Result<remote::RemoteMachineState, String> {
    Ok(attachment.state().await)
}

/// Attach this client to a remote machine.
///
/// Refusals carry a stable reason the renderer branches on; see
/// [`remote::RemoteConnectError`].
#[tauri::command]
async fn connect_remote_machine(
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
    base_url: String,
    token: String,
) -> Result<remote::RemoteMachineState, remote::RemoteConnectError> {
    attachment.connect(&base_url, &token).await
}

/// Attach to a hosted machine using the Model Gateway session this desktop
/// already holds. No user bearer is persisted or copied.
#[tauri::command]
async fn connect_gateway_remote_machine(
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
    pairing: tauri::State<'_, deep_link::PairingStore>,
    base_url: String,
) -> Result<remote::RemoteMachineState, remote::RemoteConnectError> {
    let (base_url, gateway_url, resource) = remote::discover_gateway(&base_url).await?;
    let handle = pairing.handle().await.map_err(|error| {
        remote::RemoteConnectError::detailed(remote::REASON_GATEWAY_AUTH_UNAVAILABLE, error)
    })?;
    let token = handle
        .hosted_tidebreak_access_token(&gateway_url, &resource)
        .await
        .map_err(|error| {
            remote::RemoteConnectError::detailed(remote::REASON_GATEWAY_AUTH_UNAVAILABLE, error)
        })?;
    attachment
        .connect_gateway(&base_url, &gateway_url, &token)
        .await
}

/// Detach from the remote machine and forget its token.
#[tauri::command]
async fn disconnect_remote_machine(
    attachment: tauri::State<'_, Arc<remote::RemoteAttachment>>,
) -> Result<remote::RemoteMachineState, String> {
    Ok(attachment.disconnect().await)
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
            .message(native_mcp_command_confirmation(&preview))
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

fn native_mcp_command_confirmation(preview: &str) -> String {
    format!(
        "Allow these MCP servers to run local programs with your operating-system account's permissions?\n\n{preview}"
    )
}

const MAX_NATIVE_APPROVAL_FIELD_CHARS: usize = 240;
const MAX_NATIVE_APPROVAL_MANIFEST_CHARS: usize = 16_384;

#[derive(Serialize)]
struct NativeCommandApproval<'a> {
    server: &'a str,
    argv: Vec<String>,
    cwd: NativeCommandCwd,
    environment: NativeCommandEnvironment,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case", tag = "source", content = "path")]
enum NativeCommandCwd {
    DesktopProcessCurrentDirectory,
    Configured(String),
}

#[derive(Serialize)]
struct NativeCommandEnvironment {
    ambient_environment: &'static str,
    inherited_from_desktop_process: Vec<String>,
    stored_secrets: Vec<NativeStoredSecret>,
}

#[derive(Serialize)]
struct NativeStoredSecret {
    name: String,
    effect: NativeStoredSecretEffect,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum NativeStoredSecretEffect {
    SetFromThisSave,
    PreserveExistingStoredValue,
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
        let command = native_command_token(command)?;
        if !std::path::Path::new(&command).is_absolute() {
            return Err(
                "enabled MCP local commands must use an absolute executable path".to_owned(),
            );
        }
        let mut argv = vec![command];
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

        let cwd = match server.get("cwd") {
            None | Some(Value::Null) => NativeCommandCwd::DesktopProcessCurrentDirectory,
            Some(Value::String(path)) => NativeCommandCwd::Configured(native_command_token(path)?),
            Some(_) => return Err("MCP command cwd must be a string or null".to_owned()),
        };
        let mut inherited_from_desktop_process =
            native_string_array(server, "env_from", "MCP env_from must be an array")?;
        inherited_from_desktop_process.sort_unstable();
        reject_duplicates(
            &inherited_from_desktop_process,
            "MCP env_from contains a duplicate name",
        )?;

        let mut stored_names = native_string_array(server, "env", "MCP env must be an array")?;
        stored_names.sort_unstable();
        reject_duplicates(&stored_names, "MCP env contains a duplicate name")?;
        if stored_names
            .iter()
            .any(|name| inherited_from_desktop_process.binary_search(name).is_ok())
        {
            return Err("an MCP environment name cannot be both stored and inherited".to_owned());
        }

        let env_values = match server.get("env_values") {
            None => None,
            Some(Value::Object(values)) => Some(values),
            Some(_) => return Err("MCP env_values must be an object".to_owned()),
        };
        if let Some(values) = env_values {
            for (name, value) in values {
                native_command_token(name)?;
                if !value.is_string() {
                    return Err("every MCP environment value must be a string".to_owned());
                }
                if stored_names.binary_search(name).is_err() {
                    return Err(
                        "every MCP env_values entry must name a stored environment variable"
                            .to_owned(),
                    );
                }
            }
        }
        let stored_secrets = stored_names
            .into_iter()
            .map(|name| NativeStoredSecret {
                effect: if env_values.is_some_and(|values| values.contains_key(&name)) {
                    NativeStoredSecretEffect::SetFromThisSave
                } else {
                    NativeStoredSecretEffect::PreserveExistingStoredValue
                },
                name,
            })
            .collect();
        let safe_name = native_command_token(name)?;
        let manifest = serde_json::to_string_pretty(&NativeCommandApproval {
            server: &safe_name,
            argv,
            cwd,
            environment: NativeCommandEnvironment {
                ambient_environment: "cleared",
                inherited_from_desktop_process,
                stored_secrets,
            },
        })
        .expect("native command approval manifests serialize infallibly");
        if manifest.chars().count() > MAX_NATIVE_APPROVAL_MANIFEST_CHARS {
            return Err("MCP command approval manifest is too long to display safely".to_owned());
        }
        previews.push(manifest);
    }
    let manifest_chars = previews
        .iter()
        .map(|preview| preview.chars().count())
        .sum::<usize>()
        .saturating_add(previews.len().saturating_sub(1));
    if manifest_chars > MAX_NATIVE_APPROVAL_MANIFEST_CHARS {
        return Err("MCP command approval manifest is too long to display safely".to_owned());
    }
    Ok(previews)
}

fn native_string_array(
    server: &Value,
    field: &str,
    type_error: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = server.get(field) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| type_error.to_owned())?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| format!("every MCP {field} entry must be a string"))
                .and_then(native_command_token)
        })
        .collect()
}

fn reject_duplicates(values: &[String], error: &str) -> Result<(), String> {
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(error.to_owned());
    }
    Ok(())
}

fn native_command_token(value: &str) -> Result<String, String> {
    if value.chars().count() > MAX_NATIVE_APPROVAL_FIELD_CHARS {
        return Err("MCP command approval field is too long to display safely".to_owned());
    }
    if value.chars().any(|character| {
        matches!(
            get_general_category(character),
            GeneralCategory::Control
                | GeneralCategory::Format
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        )
    }) {
        return Err("MCP command approval field contains unsafe formatting characters".to_owned());
    }
    Ok(value.to_owned())
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

/// Default root for code worktrees: `~/Tidebreak/workspaces`.
///
/// Not created here. The server creates each worktree's parents when it adds
/// one, so an install that never opens code mode leaves no empty folder in the
/// user's home directory.
fn worktree_root_default(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(home_dir(app)?
        .join(channel::PRODUCTION_PRODUCT_NAME)
        .join("workspaces"))
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

/// Derive the absolute path to a named sibling executable beside the running
/// desktop binary. The sibling must exist, be a regular file, and (on Unix)
/// have an executable permission bit.
///
/// The browser runtime resolves the `tidebreak` CLI sidecar through this
/// boundary before code-session recovery, so provider harnesses never depend
/// on an ambient `PATH` lookup.
pub(crate) fn desktop_sibling_exe(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    desktop_sibling_exe_from(&exe, name)
}

fn desktop_sibling_exe_from(exe: &Path, name: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(name);
    if candidate.components().count() != 1
        || candidate.file_name().and_then(|value| value.to_str()) != Some(name)
    {
        return Err("sibling exe name must be a single file name".to_string());
    }
    if !exe.is_absolute() {
        return Err(format!(
            "current exe path must be absolute, got: {}",
            exe.display()
        ));
    }
    let exe = exe
        .canonicalize()
        .map_err(|error| format!("could not resolve current exe {}: {error}", exe.display()))?;
    if !exe.is_absolute() {
        return Err(format!(
            "resolved current exe path must be absolute, got: {}",
            exe.display()
        ));
    }
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "current exe has no parent directory".to_string())?;
    let extension = cfg!(target_os = "windows").then_some(".exe").unwrap_or("");
    let path = exe_dir.join(format!("{name}{extension}"));
    if !path.is_absolute() {
        return Err(format!(
            "sibling exe path must be absolute, got: {}",
            path.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "sibling exe {name} not found at {} ({error})",
            path.display()
        )
    })?;
    if !metadata.file_type().is_file() {
        return Err(format!("sibling exe {name} is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("sibling exe {name} is not executable"));
        }
    }
    Ok(path)
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
            remote_machine_state,
            connect_remote_machine,
            connect_gateway_remote_machine,
            remote_machine_access_token,
            disconnect_remote_machine,
            put_native_mcp_servers,
            request_user_attention,
            code_browser::code_browser_command,
            code_worktree::open_code_worktree,
            attachments::attach_chat_files,
            attachments::attach_dropped_chat_files,
            image_attachments::publish_chat_image,
            image_attachments::publish_code_image,
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
            host_access::pick_code_directory,
            host_access::connect_folder,
            host_access::connect_approved_folder,
            host_access::attach_trusted_folders,
            host_access::set_trusted_folder,
            host_access::list_approved_folders,
            host_access::list_capability_consents,
            host_access::revoke_capability_consent,
            host_access::list_connected_folders,
            host_access::grant_folder_capability,
            host_access::disconnect_folder,
            host_access::forget_folder,
            host_access::purge_deleted_conversation_subject,
            skill_import::import_skills,
            updater::desktop_update_state,
            updater::check_for_update,
            updater::restart_for_update
        ])
        .on_menu_event(menu::handle_menu_event)
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
            menu::install_app_menu(app)?;
            updater::spawn_update_loop(handle.clone());
            let data = data_dir(&handle)?;
            // Before anything that can warn: the embedded server's tracing
            // events land in `logs/tidebreak.log` under the profile data dir
            // (stderr-only if that file cannot be created).
            tidebreak_server::logging::init_logging(&data);
            let browser_registry = browser_control::BrowserRegistry::default();
            browser_registry.initialize_private_state(&data)?;
            app.manage(browser_registry);
            let home = home_dir(&handle)?;
            // Built before anything that reaches the host: `server_info`
            // consults the attachment first, because a remote client must not
            // wait on a local boot it is not using, and every host-authority
            // command consults it to decide whether it may act at all.
            let attachment = Arc::new(remote::RemoteAttachment::new(
                &data,
                channel::current().keychain_service(),
            ));
            let host_access = host_access::HostAccess::new(
                handle.clone(),
                data.clone(),
                home,
                attachment.clone(),
            )?;
            app.manage(host_access);
            app.manage(attachment);

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
    // Code worktrees hold uncommitted work on real branches, so they land in a
    // visible folder in the user's home directory rather than in app data,
    // which uninstall and "reset app data" flows treat as disposable. Every
    // channel shares the one root on purpose: the app identifier keys app data
    // precisely so three builds cannot corrupt each other's *state*, and a dev
    // build growing its own second copy of the user's work is the problem, not
    // the protection. The stored `code_worktree_root` setting overrides this.
    config.code_worktree_root_default = Some(worktree_root_default(&app)?);
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
    let browser_runtime: Arc<dyn tidebreak_server::BrowserRuntime> =
        Arc::new(browser_runtime_adapter::DesktopBrowserRuntime::new(
            app.clone(),
            app.state::<browser_control::BrowserRegistry>()
                .inner()
                .clone(),
        ));
    let browser_binding = tidebreak_server::BrowserChannelBinding::new(
        browser_runtime,
        desktop_sibling_exe("tidebreak")?,
    );
    let server = tidebreak_server::bind_configured_with_desktop_foreground_browser_executor(
        config,
        client_executor_id,
        folder_grants,
        Some(office_converter),
        Some(host_tool_broker),
        Some(local_voice),
        Some(Arc::new(host_access::DesktopHostFolders::new(app.clone()))),
        Some(browser_binding),
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
    #[cfg(target_os = "macos")]
    {
        let foreground_browser_app = app.clone();
        tauri::async_runtime::spawn(async move {
            client_execution::browser::recover_foreground_browser_operations(
                foreground_browser_app,
            )
            .await;
        });
    }
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
    use super::{cargo_dev_resource_dir_from, desktop_sibling_exe_from};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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

    fn desktop_path(dir: &std::path::Path) -> std::path::PathBuf {
        let extension = cfg!(target_os = "windows").then_some(".exe").unwrap_or("");
        dir.join(format!("tidebreak-desktop{extension}"))
    }

    fn sibling_path(dir: &std::path::Path) -> std::path::PathBuf {
        let extension = cfg!(target_os = "windows").then_some(".exe").unwrap_or("");
        dir.join(format!("tidebreak{extension}"))
    }

    fn write_desktop(dir: &std::path::Path) -> std::path::PathBuf {
        let path = desktop_path(dir);
        fs::write(&path, []).expect("desktop exe");
        path
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

    #[test]
    fn desktop_sibling_exe_rejects_missing_file() {
        let dir = temp_dir("sibling-missing");
        let desktop = write_desktop(&dir);
        let err = desktop_sibling_exe_from(&desktop, "tidebreak").expect_err("should fail");
        assert!(
            err.contains("not found"),
            "error should mention not found: {err}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn desktop_sibling_exe_rejects_path_traversal() {
        let dir = temp_dir("sibling-traversal");
        let err =
            desktop_sibling_exe_from(&desktop_path(&dir), "../tidebreak").expect_err("should fail");
        assert!(err.contains("single file name"), "error: {err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn desktop_sibling_exe_rejects_relative_current_exe() {
        let err = desktop_sibling_exe_from(
            std::path::Path::new("relative/tidebreak-desktop"),
            "tidebreak",
        )
        .expect_err("should fail");
        assert!(err.contains("must be absolute"), "error: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn desktop_sibling_exe_rejects_non_executable_on_unix() {
        let dir = temp_dir("sibling-noexec");
        let desktop = write_desktop(&dir);
        let exe_path = sibling_path(&dir);
        fs::write(&exe_path, []).expect("write");
        let err = desktop_sibling_exe_from(&desktop, "tidebreak").expect_err("should fail");
        assert!(err.contains("not executable"), "error: {err}");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn desktop_sibling_exe_accepts_executable_on_unix() {
        let dir = temp_dir("sibling-exec");
        let desktop = write_desktop(&dir);
        let exe_path = sibling_path(&dir);
        fs::write(&exe_path, []).expect("write");
        let mut perms = fs::metadata(&exe_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&exe_path, perms).expect("chmod");
        assert_eq!(
            desktop_sibling_exe_from(&desktop, "tidebreak").unwrap(),
            exe_path.canonicalize().expect("canonical sibling")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn desktop_sibling_exe_resolves_beside_the_real_binary_not_a_launch_symlink() {
        use std::os::unix::fs::symlink;

        let real_dir = temp_dir("sibling-real");
        let launch_dir = temp_dir("sibling-launch-link");
        let real_desktop = write_desktop(&real_dir);
        let real_sibling = sibling_path(&real_dir);
        fs::write(&real_sibling, []).expect("write sibling");
        let mut perms = fs::metadata(&real_sibling).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&real_sibling, perms).expect("chmod");
        let launch_path = desktop_path(&launch_dir);
        symlink(&real_desktop, &launch_path).expect("desktop symlink");

        assert_eq!(
            desktop_sibling_exe_from(&launch_path, "tidebreak").unwrap(),
            real_sibling.canonicalize().expect("canonical sibling")
        );

        let _ = fs::remove_dir_all(launch_dir);
        let _ = fs::remove_dir_all(real_dir);
    }

    #[cfg(not(unix))]
    #[test]
    fn desktop_sibling_exe_accepts_regular_file() {
        let dir = temp_dir("sibling-file");
        let desktop = write_desktop(&dir);
        let exe_path = sibling_path(&dir);
        fs::write(&exe_path, []).expect("write");
        assert_eq!(
            desktop_sibling_exe_from(&desktop, "tidebreak").unwrap(),
            exe_path.canonicalize().expect("canonical sibling")
        );
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

    fn single_native_approval(config: Value) -> Value {
        let previews = native_command_previews(&config).expect("valid native command preview");
        assert_eq!(previews.len(), 1);
        serde_json::from_str(&previews[0]).expect("approval is a canonical JSON manifest")
    }

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
        // The embedded server is always the local machine; the renderer reads
        // host authority off this field.
        assert!(serialized.contains(r#""attachment":"local""#));
        assert!(serialized.contains(r#""gatewayAuth":false"#));
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
    fn native_command_confirmation_is_platform_neutral_and_names_the_boundary() {
        let prompt = native_mcp_command_confirmation(r#"{"command":"example"}"#);
        assert!(prompt.contains("operating-system account's permissions"));
        assert!(prompt.contains(r#"{"command":"example"}"#));
        assert!(!prompt.contains("macOS user"));
    }

    #[test]
    fn only_enabled_local_commands_require_a_native_preview() {
        let config = serde_json::json!({
            "servers": [
                {"name": "draft", "command": "editable-bare-command", "enabled": false},
                {"name": "remote", "url": "https://example.test/mcp", "enabled": true},
                {"name": "live", "command": "/bin/sh", "args": ["-c", "echo safe"], "enabled": true}
            ]
        });
        let approval = single_native_approval(config);
        assert_eq!(approval["server"], "live");
        assert_eq!(
            approval["argv"],
            serde_json::json!(["/bin/sh", "-c", "echo safe"])
        );
        assert_eq!(
            approval["cwd"]["source"],
            "desktop_process_current_directory"
        );
        assert_eq!(approval["environment"]["ambient_environment"], "cleared");
    }

    #[test]
    fn enabled_native_commands_require_an_absolute_executable_path() {
        for command in ["node", "./node", "tools/node"] {
            let config = serde_json::json!({
                "servers": [{"name": "ambiguous", "command": command, "enabled": true}]
            });
            assert_eq!(
                native_command_previews(&config).unwrap_err(),
                "enabled MCP local commands must use an absolute executable path"
            );
        }
    }

    #[test]
    fn native_approval_displays_the_exact_absolute_executable_path() {
        let config = serde_json::json!({
            "servers": [{
                "name": "pinned executable",
                "command": "/opt/tidebreak/bin/docs-mcp",
                "args": ["serve"],
                "enabled": true
            }]
        });
        let approval = single_native_approval(config);
        assert_eq!(
            approval["argv"],
            serde_json::json!(["/opt/tidebreak/bin/docs-mcp", "serve"])
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
        let ninth: Value = serde_json::from_str(&previews[8]).unwrap();
        assert_eq!(ninth["server"], "server-8");
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

    #[test]
    fn cwd_and_inherited_path_changes_are_explicit_in_native_approval() {
        let base = serde_json::json!({
            "servers": [{
                "name": "workspace",
                "command": "/usr/bin/node",
                "args": ["server.mjs"],
                "cwd": "/srv/first",
                "env_from": ["HOME"]
            }]
        });
        let changed = serde_json::json!({
            "servers": [{
                "name": "workspace",
                "command": "/usr/bin/node",
                "args": ["server.mjs"],
                "cwd": "/srv/second",
                "env_from": ["HOME", "PATH"]
            }]
        });
        let first = single_native_approval(base);
        let second = single_native_approval(changed);
        assert_ne!(first, second);
        assert_eq!(first["cwd"]["path"], "/srv/first");
        assert_eq!(second["cwd"]["path"], "/srv/second");
        assert_eq!(
            second["environment"]["inherited_from_desktop_process"],
            serde_json::json!(["HOME", "PATH"])
        );
    }

    #[test]
    fn stored_secret_sources_and_preservation_are_visible_without_values() {
        let preserved = serde_json::json!({
            "servers": [{
                "name": "private docs",
                "command": "/usr/bin/docs-mcp",
                "env": ["DOCS_TOKEN", "LOG_LEVEL"]
            }]
        });
        let replaced = serde_json::json!({
            "servers": [{
                "name": "private docs",
                "command": "/usr/bin/docs-mcp",
                "env": ["DOCS_TOKEN", "LOG_LEVEL"],
                "env_values": {"DOCS_TOKEN": "secret-sentinel-value"}
            }]
        });
        let preserved = single_native_approval(preserved);
        let replaced = single_native_approval(replaced);
        assert_ne!(preserved, replaced);
        assert_eq!(
            replaced["environment"]["stored_secrets"],
            serde_json::json!([
                {"name": "DOCS_TOKEN", "effect": "set_from_this_save"},
                {"name": "LOG_LEVEL", "effect": "preserve_existing_stored_value"}
            ])
        );
        let encoded = replaced.to_string();
        assert!(!encoded.contains("secret-sentinel-value"));
    }

    #[test]
    fn environment_source_changes_alter_approval_and_never_show_parent_values() {
        let inherited = serde_json::json!({
            "servers": [{
                "name": "docs",
                "command": "/usr/bin/docs-mcp",
                "env_from": ["PRIVATE_DOCS_TOKEN"]
            }]
        });
        let stored = serde_json::json!({
            "servers": [{
                "name": "docs",
                "command": "/usr/bin/docs-mcp",
                "env": ["PRIVATE_DOCS_TOKEN"]
            }]
        });
        let inherited = single_native_approval(inherited);
        let stored = single_native_approval(stored);
        assert_ne!(inherited, stored);
        assert_eq!(
            inherited["environment"]["inherited_from_desktop_process"],
            serde_json::json!(["PRIVATE_DOCS_TOKEN"])
        );
        assert_eq!(
            stored["environment"]["stored_secrets"],
            serde_json::json!([{
                "name": "PRIVATE_DOCS_TOKEN",
                "effect": "preserve_existing_stored_value"
            }])
        );
        if let Ok(parent_value) = std::env::var("PRIVATE_DOCS_TOKEN") {
            assert!(!inherited.to_string().contains(&parent_value));
        }
    }

    #[test]
    fn native_approval_refuses_unrepresentable_environment_manifest() {
        let config = serde_json::json!({
            "servers": [{
                "name": "oversized",
                "command": "/bin/true",
                "env_from": ["X".repeat(MAX_NATIVE_APPROVAL_FIELD_CHARS + 1)]
            }]
        });
        assert!(native_command_previews(&config).is_err());
    }
}
