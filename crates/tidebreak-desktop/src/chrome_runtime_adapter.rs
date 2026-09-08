//! Native Chrome connection consent and session-owned browser processes.
//!
//! Register this adapter with the computer-use dispatcher and call `shutdown`
//! before app exit. The host supplies scope identity and local storage paths.
//! No renderer or model argument can choose an executable or debugger endpoint.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};
use tidebreak_core::chrome_connection::{
    validate_chrome_connection_arguments, ChromeConnectArgs, ChromeConnectionMode,
    CHROME_CONNECTION_STATE_TOOL, CHROME_CONNECT_TOOL, CHROME_DISCONNECT_TOOL,
};
use tidebreak_core::computer_session::{ComputerUseCall, ComputerUseOutcome, ComputerUseResult};
use tidebreak_core::{CancelToken, ChromeConnectionGrant, OwnerId, SessionId, WorkspaceId};
use tidebreak_server::chrome::{
    cdp::CdpSession, ChromeComputerUseService, ChromeConnectionSpec, ChromeScope,
};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);
const ENDPOINT_POLL: Duration = Duration::from_millis(150);
const MAX_ENDPOINT_FILE_BYTES: u64 = 4096;
const MAX_CACHED_CALLS: usize = 128;
const MAX_ACTION_REQUESTS: usize = 4096;
const EXISTING_SETUP_URL: &str = "chrome://inspect/#remote-debugging";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScopeKey {
    owner: OwnerId,
    workspace: WorkspaceId,
    session: SessionId,
}

impl From<&ChromeScope> for ScopeKey {
    fn from(scope: &ChromeScope) -> Self {
        Self {
            owner: scope.owner.clone(),
            workspace: scope.workspace,
            session: scope.session,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChromeConnectionStatus {
    Disconnected,
    Connected,
    Paused,
}

/// Safe connection information for tool results and desktop settings.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeConnectionView {
    pub status: ChromeConnectionStatus,
    pub mode: Option<ChromeConnectionMode>,
    pub tab_count: usize,
    pub last_error: Option<String>,
}

struct ControlState {
    paused: bool,
    stop: CancelToken,
    service: Option<ChromeComputerUseService>,
}

struct SessionState {
    connection: Option<Connection>,
    results: VecDeque<(ComputerUseCall, ComputerUseResult)>,
    action_requests: HashMap<Uuid, (ComputerUseCall, String)>,
    last_error: Option<String>,
}

struct Session {
    control: Mutex<ControlState>,
    state: AsyncMutex<SessionState>,
    revoked: AtomicBool,
}

impl Session {
    fn new() -> Self {
        Self {
            control: Mutex::new(ControlState {
                paused: false,
                stop: CancelToken::new(),
                service: None,
            }),
            state: AsyncMutex::new(SessionState {
                connection: None,
                results: VecDeque::new(),
                action_requests: HashMap::new(),
                last_error: None,
            }),
            revoked: AtomicBool::new(false),
        }
    }

    fn stop(&self) {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        control.paused = true;
        control.stop.cancel();
        if let Some(service) = &control.service {
            service.ownership().trip();
        }
    }

    fn begin_consent(&self) -> CancelToken {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        control.paused = true;
        // Reset only the prompt's cancellation token. Actions stay paused and
        // the service stays stopped until a native answer approves resumption.
        if control.stop.is_cancelled() {
            control.stop = CancelToken::new();
        }
        control.stop.clone()
    }

    fn activate(
        &self,
        service: ChromeComputerUseService,
        stop: &CancelToken,
    ) -> Result<(), String> {
        let mut control = self.control.lock().unwrap_or_else(|p| p.into_inner());
        if stop.is_cancelled() || self.revoked.load(Ordering::Acquire) {
            return Err("Chrome connection stopped before approval completed.".into());
        }
        service.ownership().resume();
        control.service = Some(service);
        control.paused = false;
        Ok(())
    }
}

struct ExistingLease {
    key: ScopeKey,
    holder: Arc<Mutex<Option<ScopeKey>>>,
}

impl Drop for ExistingLease {
    fn drop(&mut self) {
        let mut holder = self.holder.lock().unwrap_or_else(|p| p.into_inner());
        if holder.as_ref() == Some(&self.key) {
            *holder = None;
        }
    }
}

struct ManagedChrome {
    child: Child,
    _profile: tempfile::TempDir,
}

impl Drop for ManagedChrome {
    fn drop(&mut self) {
        // The child owns a new process group. Kill that group while the browser
        // PID is still our live child, so its helpers cannot outlive revocation.
        if self.child.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            if let Ok(pid) = i32::try_from(self.child.id()) {
                // SAFETY: spawn_managed creates this child's process group with
                // process_group(0). Its unreaped PID cannot have been recycled.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

struct Connection {
    service: ChromeComputerUseService,
    id: String,
    mode: ChromeConnectionMode,
    _managed: Option<ManagedChrome>,
    _existing: Option<ExistingLease>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.service.uninstall_connection(&self.id);
    }
}

/// One adapter per desktop host, with separate services and Stop latches per
/// coding session. Existing-profile sharing has one owner at a time.
pub struct ChromeRuntimeAdapter {
    app: AppHandle,
    profile_root: PathBuf,
    home: PathBuf,
    sessions: Mutex<HashMap<ScopeKey, Arc<Session>>>,
    existing_holder: Arc<Mutex<Option<ScopeKey>>>,
    revoked_sessions: Mutex<HashSet<ScopeKey>>,
    shutting_down: AtomicBool,
}

impl ChromeRuntimeAdapter {
    /// Paths come from Tauri's native path resolver, never from tool arguments.
    pub fn new(app: AppHandle, app_cache: PathBuf, home: PathBuf) -> Self {
        Self {
            app,
            profile_root: app_cache.join("computer-use").join("chrome"),
            home,
            sessions: Mutex::new(HashMap::new()),
            existing_holder: Arc::new(Mutex::new(None)),
            revoked_sessions: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub async fn execute(&self, scope: &ChromeScope, call: &ComputerUseCall) -> ComputerUseResult {
        if self.shutting_down.load(Ordering::Acquire) || scope.cancel.is_cancelled() {
            return rejected(call, "Chrome request cancelled.", "cancelled");
        }
        let is_connection = matches!(
            call.name.as_str(),
            CHROME_CONNECT_TOOL | CHROME_DISCONNECT_TOOL | CHROME_CONNECTION_STATE_TOOL
        );
        if is_connection && !validate_chrome_connection_arguments(&call.name, &call.arguments) {
            return rejected(
                call,
                "Invalid Chrome connection arguments.",
                "invalid_arguments",
            );
        }
        let key = ScopeKey::from(scope);
        if self
            .revoked_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&key)
        {
            return rejected(
                call,
                "Chrome access was revoked for this session.",
                "revoked",
            );
        }
        let session = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            if self.shutting_down.load(Ordering::Acquire) {
                return rejected(call, "Chrome is shutting down.", "cancelled");
            }
            sessions
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Session::new()))
                .clone()
        };
        let (paused_at_enqueue, queued_stop) = {
            let control = session.control.lock().unwrap_or_else(|p| p.into_inner());
            (control.paused, control.stop.clone())
        };
        let mut state = tokio::select! {
            biased;
            _ = scope.cancel.cancelled() => return rejected(call, "Chrome request cancelled.", "cancelled"),
            state = session.state.lock() => state,
        };
        if session.revoked.load(Ordering::Acquire)
            || self.shutting_down.load(Ordering::Acquire)
            || self
                .revoked_sessions
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&key)
        {
            session.revoked.store(true, Ordering::Release);
            session.stop();
            state.connection.take();
            return rejected(call, "Chrome access was revoked.", "revoked");
        }
        if let Some((prior, result)) = state
            .results
            .iter()
            .find(|(prior, _)| prior.request_id == call.request_id)
        {
            return if prior == call {
                result.clone()
            } else {
                rejected(
                    call,
                    "Request id belongs to a different Chrome operation.",
                    "request_id_conflict",
                )
            };
        }
        if !is_connection && (paused_at_enqueue || queued_stop.is_cancelled()) {
            return rejected(call, "Chrome stopped while this action was queued. Propose a new action after native approval to resume.", "stopped");
        }
        if let Some((prior, connection_id)) = state.action_requests.get(&call.request_id) {
            if prior != call {
                return rejected(
                    call,
                    "Request id belongs to a different Chrome operation.",
                    "request_id_conflict",
                );
            }
            if state.connection.as_ref().map(|connection| &connection.id) != Some(connection_id) {
                return unknown(call, "This request belongs to a previous Chrome connection. Inspect the page and propose a new action.", "connection_changed");
            }
        } else if !is_connection {
            if let Some(connection_id) = state
                .connection
                .as_ref()
                .map(|connection| connection.id.clone())
            {
                if state.action_requests.len() >= MAX_ACTION_REQUESTS {
                    return rejected(call, "This session reached its Chrome request limit. Start a new coding session.", "request_limit");
                }
                state
                    .action_requests
                    .insert(call.request_id, (call.clone(), connection_id));
            }
        }
        // Cache only lifecycle mutations. Pixel results stay in the shared
        // computer-use request journal rather than a second desktop image cache.
        let cache_result = matches!(
            call.name.as_str(),
            CHROME_CONNECT_TOOL | CHROME_DISCONNECT_TOOL
        );
        if cache_result && state.results.len() >= MAX_CACHED_CALLS {
            return rejected(call, "This session reached its Chrome connection request limit. Start a new coding session.", "connection_request_limit");
        }
        let result = match call.name.as_str() {
            CHROME_CONNECTION_STATE_TOOL => completed(
                call,
                "Chrome connection state.",
                view(&session, &state, scope),
            ),
            CHROME_DISCONNECT_TOOL => {
                session.stop();
                state.connection.take();
                state.last_error = None;
                completed(call, "Chrome disconnected.", view(&session, &state, scope))
            }
            CHROME_CONNECT_TOOL => {
                let args: ChromeConnectArgs = serde_json::from_value(call.arguments.clone())
                    .expect("validated connection args");
                self.connect(&key, &session, &mut state, scope, call, args.mode)
                    .await
            }
            _ => {
                let (paused, stop) = {
                    let control = session.control.lock().unwrap_or_else(|p| p.into_inner());
                    (control.paused, control.stop.clone())
                };
                if paused {
                    rejected(call, "Chrome is stopped. Request chrome_connect and wait for native approval to resume.", "stopped")
                } else if let Some(connection) = &state.connection {
                    if call.name == tidebreak_core::CHROME_ACTIVATE_TAB_TOOL {
                        let host = self.app.state::<crate::host_access::HostAccess>();
                        let approved = tokio::select! {
                            biased;
                            _ = scope.cancel.cancelled() => false,
                            _ = stop.cancelled() => false,
                            _ = host.computer_use.wait_for_halt() => false,
                            approved = native_focus_consent(&self.app) => approved.unwrap_or(false),
                        };
                        if !approved {
                            return rejected(
                                call,
                                "Bringing Chrome forward was not approved.",
                                "foreground_not_approved",
                            );
                        }
                    }
                    let active_scope = ChromeScope {
                        cancel: stop.clone(),
                        ..scope.clone()
                    };
                    let host = self.app.state::<crate::host_access::HostAccess>();
                    match dispatch_chrome_operation(
                        &host.computer_use,
                        scope.session,
                        call.name == tidebreak_core::CHROME_ACTIVATE_TAB_TOOL,
                        &scope.cancel,
                        &stop,
                        || session.stop(),
                        async {
                            let outcome = connection.service.dispatch(&active_scope, call).await;
                            if call.name == tidebreak_core::CHROME_ACTIVATE_TAB_TOOL
                                && outcome.result.outcome == ComputerUseOutcome::Unknown
                            {
                                // An unacknowledged activation may still change
                                // focus. Halt before releasing foreground ownership.
                                session.stop();
                                host.computer_use
                                    .cancel_session(scope.session, || Ok::<_, ()>(()))
                                    .unwrap();
                            }
                            outcome
                        },
                    )
                    .await
                    {
                        Ok(outcome) => outcome.result,
                        Err(()) => unknown(
                            call,
                            "Chrome stopped. Inspect the page before retrying an action.",
                            "stopped",
                        ),
                    }
                } else {
                    rejected(
                        call,
                        "Chrome is not connected. Call chrome_connect first.",
                        "not_connected",
                    )
                }
            }
        };
        if cache_result {
            state.results.push_back((call.clone(), result.clone()));
        }
        result
    }

    async fn connect(
        &self,
        key: &ScopeKey,
        session: &Session,
        state: &mut SessionState,
        scope: &ChromeScope,
        call: &ComputerUseCall,
        mode: ChromeConnectionMode,
    ) -> ComputerUseResult {
        let paused = session
            .control
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .paused;
        if state
            .connection
            .as_ref()
            .is_some_and(|connection| connection.mode == mode)
            && !paused
        {
            return completed(call, "Chrome is connected.", view(session, state, scope));
        }
        let stop = session.begin_consent();
        let setup = async {
            if !native_consent(&self.app, mode, paused).await? {
                return Err("Chrome connection was not approved.".to_owned());
            }
            if stop.is_cancelled()
                || scope.cancel.is_cancelled()
                || session.revoked.load(Ordering::Acquire)
            {
                return Err("Chrome connection was stopped.".to_owned());
            }
            if let Some(connection) = &state.connection {
                if connection.mode == mode {
                    session.activate(connection.service.clone(), &stop)?;
                    return Ok(());
                }
            }
            state.connection.take();
            let browser = installed_chrome(&self.home)?;
            verify_chrome_version(&browser).await?;
            let (endpoint, cdp, managed, existing) = match mode {
                ChromeConnectionMode::Managed => {
                    let mut managed = spawn_managed(&browser, &self.profile_root)?;
                    let (endpoint, cdp) =
                        connect_profile(managed._profile.path(), Some(&mut managed.child)).await?;
                    (endpoint, cdp, Some(managed), None)
                }
                ChromeConnectionMode::Existing => {
                    let lease = self.lease_existing(key)?;
                    let profile = default_profile(&self.home)?;
                    // Chrome 144+ owns enablement and its connection approval.
                    // Opening this page never toggles the setting for the user.
                    open_remote_debugging_settings(&browser)?;
                    let (endpoint, cdp) = connect_profile(&profile, None).await?;
                    (endpoint, cdp, None, Some(lease))
                }
            };
            let service = ChromeComputerUseService::new();
            let id = Uuid::new_v4().to_string();
            service.install_connection(
                ChromeConnectionSpec {
                    connection_id: id.clone(),
                    owner: scope.owner.clone(),
                    workspace: scope.workspace,
                    endpoint_label: match mode {
                        ChromeConnectionMode::Managed => "Chrome for this coding session",
                        ChromeConnectionMode::Existing => "Your existing Chrome browser",
                    }
                    .to_owned(),
                    websocket_endpoint: endpoint,
                    grant: ChromeConnectionGrant::DeveloperAllSites,
                    managed_isolated: mode == ChromeConnectionMode::Managed,
                },
                cdp,
            )?;
            let connection = Connection {
                service,
                id,
                mode,
                _managed: managed,
                _existing: existing,
            };
            let active_scope = ChromeScope {
                cancel: stop.clone(),
                ..scope.clone()
            };
            for tab in connection.service.discover_tabs(&connection.id).await? {
                if web_tab(&tab.url) {
                    connection
                        .service
                        .attach_existing_tab(&active_scope, &connection.id, &tab.target_id)
                        .await?;
                }
            }
            session.activate(connection.service.clone(), &stop)?;
            state.connection = Some(connection);
            Ok::<_, String>(())
        };
        let outcome = tokio::select! {
            biased;
            _ = scope.cancel.cancelled() => Err("Chrome connection cancelled.".to_owned()),
            _ = stop.cancelled() => Err("Chrome connection stopped.".to_owned()),
            outcome = tokio::time::timeout(CONNECT_TIMEOUT, setup) => outcome.unwrap_or_else(|_| Err("Chrome connection timed out. Enable remote debugging at chrome://inspect/#remote-debugging, approve Chrome's prompt, then request a new connection.".to_owned())),
        };
        match outcome {
            Ok(()) => {
                state.last_error = None;
                completed(call, "Chrome connected. Use chrome_new_tab to open your app or chrome_list_tabs to see shared tabs.", view(session, state, scope))
            }
            Err(error) => {
                session.stop();
                state.last_error = Some(error.clone());
                rejected(call, &error, "connection_failed")
            }
        }
    }

    fn lease_existing(&self, key: &ScopeKey) -> Result<ExistingLease, String> {
        let mut holder = self
            .existing_holder
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if holder.is_some() {
            return Err("Another coding session controls your existing Chrome browser. Disconnect it before sharing Chrome with this session.".into());
        }
        *holder = Some(key.clone());
        Ok(ExistingLease {
            key: key.clone(),
            holder: self.existing_holder.clone(),
        })
    }

    /// Native Stop/takeover entry point. This never resets another session.
    pub fn stop_session(&self, scope: &ChromeScope) {
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&ScopeKey::from(scope))
        {
            session.stop();
        }
    }

    /// Call on session termination. Its identity can never reconnect. The
    /// managed browser exits; a personal browser only loses its debugger.
    pub async fn revoke_session(&self, scope: &ChromeScope) {
        self.revoked_sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(ScopeKey::from(scope));
        let session = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&ScopeKey::from(scope));
        if let Some(session) = session {
            session.revoked.store(true, Ordering::Release);
            session.stop();
            session.state.lock().await.connection.take();
        }
    }

    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, session)| session)
            .collect();
        for session in &sessions {
            session.revoked.store(true, Ordering::Release);
            session.stop();
        }
        for session in sessions {
            session.state.lock().await.connection.take();
        }
    }
}

impl Drop for ChromeRuntimeAdapter {
    fn drop(&mut self) {
        for session in self
            .sessions
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .values()
        {
            session.revoked.store(true, Ordering::Release);
            session.stop();
        }
    }
}

/// Chrome page input remains concurrent with native apps. Explicit activation
/// owns the foreground until its protocol reply arrives, even after Stop.
async fn dispatch_chrome_operation<T>(
    computer_use: &crate::client_execution::computer_use::ComputerUseState,
    session: SessionId,
    foreground: bool,
    cancelled: &CancelToken,
    stop: &CancelToken,
    stop_session: impl FnOnce(),
    operation: impl std::future::Future<Output = T>,
) -> Result<T, ()> {
    let started = AtomicBool::new(false);
    let dispatch = async {
        if foreground {
            computer_use
                .dispatch_foreground_operation(session, || {
                    started.store(true, Ordering::Release);
                    operation
                })
                .await
        } else {
            Ok(operation.await)
        }
    };
    tokio::pin!(dispatch);
    tokio::select! {
        biased;
        _ = async {
            tokio::select! {
                _ = cancelled.cancelled() => {},
                _ = stop.cancelled() => {},
                _ = computer_use.wait_for_halt(), if foreground => {},
            }
        } => {
            stop_session();
            if started.load(Ordering::Acquire) {
                // Only this call's live owner sets the shared halt. A queued
                // Chrome call must not interrupt another session's native input.
                computer_use.cancel_session(session, || Ok::<_, ()>(())).unwrap();
                let _ = dispatch.await;
            }
            Err(())
        },
        result = &mut dispatch => result,
    }
}

fn view(session: &Session, state: &SessionState, scope: &ChromeScope) -> ChromeConnectionView {
    let paused = session
        .control
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .paused;
    ChromeConnectionView {
        status: match (&state.connection, paused) {
            (None, _) => ChromeConnectionStatus::Disconnected,
            (Some(_), true) => ChromeConnectionStatus::Paused,
            (Some(_), false) => ChromeConnectionStatus::Connected,
        },
        mode: state.connection.as_ref().map(|connection| connection.mode),
        tab_count: state
            .connection
            .as_ref()
            .map_or(0, |connection| connection.service.state(scope).tab_count),
        last_error: state.last_error.clone(),
    }
}

async fn native_focus_consent(app: &AppHandle) -> Result<bool, String> {
    let (send, receive) = oneshot::channel();
    let mut dialog = app.dialog()
        .message("Bring the shared Chrome tab to the front? This can change your keyboard focus. Other Chrome actions run in the background.")
        .title("Bring Chrome forward?")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Bring forward".into(), "Keep working".into()));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show_with_result(move |answer| {
        let accepted = matches!(answer, MessageDialogResult::Ok)
            || matches!(answer, MessageDialogResult::Custom(ref value) if value == "Bring forward");
        let _ = send.send(accepted);
    });
    receive
        .await
        .map_err(|_| "Chrome focus prompt closed.".to_owned())
}

async fn native_consent(
    app: &AppHandle,
    mode: ChromeConnectionMode,
    resume: bool,
) -> Result<bool, String> {
    let message = match mode {
        ChromeConnectionMode::Managed => "Allow Tidebreak to start and control Chrome in the background with a separate temporary profile for this coding session?\n\nThe agent can open and change pages, read all tabs in this profile, capture screenshots, and inspect console and network diagnostics. Visible content and diagnostics can be sent to the selected model and provider. This profile does not contain your existing Chrome sign-ins. Stop pauses control; disconnect closes this browser and removes its temporary profile.",
        ChromeConnectionMode::Existing => "Allow Tidebreak to control your running Google Chrome browser for this coding session?\n\nThis shares all web tabs exposed by that Chrome instance, including signed-in pages and tabs from other profiles it exposes. The agent can read and change those pages, capture screenshots, and inspect console and network diagnostics. Page content and diagnostics can be sent to the selected model and provider. This access covers the whole connected Chrome instance, not one site.\n\nChrome 144 or later must be running. In Chrome's remote debugging page, enable remote debugging and approve Chrome's own connection prompt. Tidebreak opens that page in the background after you continue. Bring Chrome forward when you are ready to approve its connection prompt. Disconnect leaves your Chrome open.",
    };
    let (send, receive) = oneshot::channel();
    let mut dialog = app
        .dialog()
        .message(message)
        .title(if resume {
            "Resume Chrome computer use?"
        } else {
            "Allow Chrome computer use?"
        })
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Allow connection".into(),
            "Cancel".into(),
        ));
    if let Some(window) = app.get_window("main") {
        dialog = dialog.parent(&window);
    }
    dialog.show_with_result(move |answer| { let _ = send.send(matches!(answer, MessageDialogResult::Ok) || matches!(answer, MessageDialogResult::Custom(ref value) if value == "Allow connection")); });
    receive
        .await
        .map_err(|_| "Chrome consent dialog closed.".into())
}

fn installed_chrome(home: &Path) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    for root in [PathBuf::from("/Applications"), home.join("Applications")] {
        let binary = root.join("Google Chrome.app/Contents/MacOS/Google Chrome");
        if binary.is_file() {
            return Ok(binary);
        }
    }
    let _ = home;
    Err("Chrome computer use requires Google Chrome installed in Applications on macOS.".into())
}

fn default_profile(home: &Path) -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        Ok(home.join("Library/Application Support/Google/Chrome"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        Err("Existing Chrome sharing is available on macOS.".into())
    }
}

async fn verify_chrome_version(binary: &Path) -> Result<(), String> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .map_err(|_| "Chrome did not report its version.".to_owned())?
        .map_err(|_| "Could not start Google Chrome.".to_owned())?;
    let version = String::from_utf8_lossy(&output.stdout);
    let major = version
        .split_whitespace()
        .find_map(|part| part.split('.').next()?.parse::<u32>().ok());
    if !output.status.success() || major.is_none_or(|major| major < 144) {
        return Err("Chrome computer use requires Google Chrome 144 or later.".into());
    }
    Ok(())
}

fn spawn_managed(binary: &Path, root: &Path) -> Result<ManagedChrome, String> {
    std::fs::create_dir_all(root)
        .map_err(|_| "Could not create Chrome's temporary profile directory.".to_owned())?;
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|_| "Could not inspect Chrome's profile directory.".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Chrome's profile directory must be a real directory.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid takes no arguments and returns the process identity.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Chrome's profile directory belongs to another user.".into());
        }
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect Chrome's temporary profile directory.".to_owned())?;
    }
    let profile = tempfile::Builder::new()
        .prefix("session-")
        .tempdir_in(root)
        .map_err(|_| "Could not create an isolated Chrome profile.".to_owned())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "Could not protect the isolated Chrome profile.".to_owned())?;
    }
    let mut command = Command::new(binary);
    command.args(managed_arguments(profile.path()));
    command.env_clear();
    for key in [
        "HOME",
        "PATH",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "__CF_USER_TEXT_ENCODING",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|_| "Could not launch Google Chrome.".to_owned())?;
    Ok(ManagedChrome {
        child,
        _profile: profile,
    })
}

fn managed_arguments(profile: &Path) -> Vec<std::ffi::OsString> {
    let mut user_data_dir = std::ffi::OsString::from("--user-data-dir=");
    user_data_dir.push(profile);
    vec![
        user_data_dir,
        "--remote-debugging-port=0".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--no-startup-window".into(),
    ]
}

fn open_remote_debugging_settings(binary: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let app_bundle = binary
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or_else(|| "Could not locate the Google Chrome app bundle.".to_owned())?;
        let status = Command::new("/usr/bin/open")
            .arg("-g")
            .arg("-a")
            .arg(app_bundle)
            .arg(EXISTING_SETUP_URL)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| "Could not open Chrome's remote debugging settings.".to_owned())?;
        if status.success() {
            return Ok(());
        }
    }
    let _ = binary;
    Err(
        "Open chrome://inspect/#remote-debugging in Google Chrome and enable remote debugging."
            .into(),
    )
}

async fn connect_profile(
    profile: &Path,
    child: Option<&mut Child>,
) -> Result<(String, CdpSession), String> {
    let endpoint = wait_for_endpoint(profile, child).await?;
    // A failed connection can mean the user declined Chrome's own prompt.
    // Never reconnect automatically or repeatedly ask for that permission.
    let cdp = CdpSession::connect(&endpoint).await.map_err(|_| {
        "Chrome did not connect. Enable remote debugging at chrome://inspect/#remote-debugging and approve Chrome's prompt, then request a new connection.".to_owned()
    })?;
    let version = cdp.command("Browser.getVersion", json!({})).await;
    let is_chrome = version
        .as_ref()
        .ok()
        .and_then(|value| value.get("product"))
        .and_then(Value::as_str)
        .is_some_and(|product| product.starts_with("Chrome/"));
    if !is_chrome {
        cdp.close();
        return Err("The local debugger did not identify itself as Google Chrome.".into());
    }
    Ok((endpoint, cdp))
}

async fn wait_for_endpoint(
    profile: &Path,
    mut child: Option<&mut Child>,
) -> Result<String, String> {
    loop {
        if let Some(child) = child.as_mut() {
            if child
                .try_wait()
                .map_err(|_| "Could not inspect the Chrome process.".to_owned())?
                .is_some()
            {
                return Err("Chrome closed before its debugger was ready.".into());
            }
        }
        match read_endpoint(profile) {
            Ok(Some(endpoint)) => return Ok(endpoint),
            Ok(None) => tokio::time::sleep(ENDPOINT_POLL).await,
            Err(error) => return Err(error),
        }
    }
}

fn read_endpoint(profile: &Path) -> Result<Option<String>, String> {
    let path = profile.join("DevToolsActivePort");
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Could not open Chrome's remote debugging state safely.".into()),
    };
    let metadata = file
        .metadata()
        .map_err(|_| "Could not inspect Chrome's remote debugging state.".to_owned())?;
    if !metadata.is_file() || metadata.len() > MAX_ENDPOINT_FILE_BYTES {
        return Err("Chrome's remote debugging state is not a regular bounded file.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid takes no arguments and returns the process identity.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err("Chrome's remote debugging state belongs to another user.".into());
        }
    }
    let mut text = String::new();
    file.take(MAX_ENDPOINT_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Could not read Chrome's remote debugging state.".to_owned())?;
    if text.lines().count() < 2 {
        return Ok(None);
    }
    parse_endpoint(&text).map(Some)
}

fn parse_endpoint(text: &str) -> Result<String, String> {
    let lines: Vec<_> = text.lines().collect();
    let invalid = || "Chrome published invalid remote debugging state.".to_owned();
    if text.len() > MAX_ENDPOINT_FILE_BYTES as usize || lines.len() != 2 {
        return Err(invalid());
    }
    let port = lines[0].parse::<u16>().map_err(|_| invalid())?;
    let path = lines[1];
    let id = path
        .strip_prefix("/devtools/browser/")
        .ok_or_else(invalid)?;
    if port == 0
        || id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid());
    }
    Ok(format!("ws://127.0.0.1:{port}{path}"))
}

fn web_tab(raw: &str) -> bool {
    url::Url::parse(raw)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn completed(call: &ComputerUseCall, text: &str, value: impl Serialize) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Completed,
        text: text.into(),
        data: serde_json::to_value(value).unwrap_or(Value::Null),
        error_code: None,
        images: Vec::new(),
    }
}
fn rejected(call: &ComputerUseCall, text: &str, code: &str) -> ComputerUseResult {
    ComputerUseResult {
        request_id: call.request_id,
        outcome: ComputerUseOutcome::Rejected,
        text: text.into(),
        data: json!({}),
        error_code: Some(code.into()),
        images: Vec::new(),
    }
}
fn unknown(call: &ComputerUseCall, text: &str, code: &str) -> ComputerUseResult {
    ComputerUseResult {
        outcome: ComputerUseOutcome::Unknown,
        ..rejected(call, text, code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chrome_activation_waits_for_native_input_while_background_calls_continue() {
        use crate::client_execution::computer_use::ComputerUseState;
        let computer_use = Arc::new(ComputerUseState::default());
        let native_session = SessionId::new();
        let chrome_session = SessionId::new();
        let (native_started, native_started_rx) = oneshot::channel();
        let (release_native, release_native_rx) = oneshot::channel();
        let native_host = computer_use.clone();
        let native = tokio::spawn(async move {
            native_host
                .test_dispatch_acting(native_session, async {
                    native_started.send(()).unwrap();
                    release_native_rx.await.unwrap();
                })
                .await
        });
        native_started_rx.await.unwrap();
        let (chrome_started, mut chrome_started_rx) = oneshot::channel();
        let chrome_host = computer_use.clone();
        let chrome = tokio::spawn(async move {
            dispatch_chrome_operation(
                &chrome_host,
                chrome_session,
                true,
                &CancelToken::new(),
                &CancelToken::new(),
                || {},
                async {
                    chrome_started.send(()).unwrap();
                },
            )
            .await
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            chrome_started_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(computer_use.owns_dispatch(native_session));
        assert_eq!(
            dispatch_chrome_operation(
                &computer_use,
                chrome_session,
                false,
                &CancelToken::new(),
                &CancelToken::new(),
                || {},
                async { "background completed" },
            )
            .await
            .unwrap(),
            "background completed"
        );
        release_native.send(()).unwrap();
        native.await.unwrap().unwrap();
        chrome_started_rx.await.unwrap();
        chrome.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelling_a_queued_chrome_activation_preserves_the_native_owner() {
        use crate::client_execution::computer_use::ComputerUseState;
        let computer_use = Arc::new(ComputerUseState::default());
        // Use the same identity to prove ownership belongs to this operation,
        // not any other live operation from the session.
        let session = SessionId::new();
        let (started, started_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let native_host = computer_use.clone();
        let native = tokio::spawn(async move {
            native_host
                .test_dispatch_acting(session, async {
                    started.send(()).unwrap();
                    release_rx.await.unwrap();
                })
                .await
        });
        started_rx.await.unwrap();
        let cancel = CancelToken::new();
        let chrome_cancel = cancel.clone();
        let chrome_host = computer_use.clone();
        let chrome = tokio::spawn(async move {
            dispatch_chrome_operation(
                &chrome_host,
                session,
                true,
                &chrome_cancel,
                &CancelToken::new(),
                || {},
                async { panic!("cancelled queued activation must not dispatch") },
            )
            .await
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(tokio::time::timeout(Duration::from_secs(1), chrome)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(!computer_use.is_halted());
        assert!(computer_use.owns_dispatch(session));
        release.send(()).unwrap();
        native.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stopping_chrome_activation_keeps_ownership_until_the_protocol_drains() {
        use crate::client_execution::computer_use::ComputerUseState;
        let computer_use = Arc::new(ComputerUseState::default());
        let session = SessionId::new();
        let cancel = CancelToken::new();
        let chrome_cancel = cancel.clone();
        let (started, started_rx) = oneshot::channel();
        let (stopped, stopped_rx) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        let chrome_host = computer_use.clone();
        let chrome = tokio::spawn(async move {
            dispatch_chrome_operation(
                &chrome_host,
                session,
                true,
                &chrome_cancel,
                &CancelToken::new(),
                || {
                    stopped.send(()).unwrap();
                },
                async {
                    started.send(()).unwrap();
                    release_rx.await.unwrap();
                },
            )
            .await
        });
        started_rx.await.unwrap();
        cancel.cancel();
        stopped_rx.await.unwrap();
        assert!(computer_use.owns_dispatch(session));
        assert!(computer_use.is_halted());
        assert!(!chrome.is_finished());
        release.send(()).unwrap();
        assert!(chrome.await.unwrap().is_err());
        computer_use.drain_acting().await;
        assert!(!computer_use.owns_dispatch(session));
    }

    #[test]
    fn debugger_state_cannot_supply_an_arbitrary_endpoint() {
        assert_eq!(
            parse_endpoint("9222\n/devtools/browser/abc-def\n").unwrap(),
            "ws://127.0.0.1:9222/devtools/browser/abc-def"
        );
        for invalid in [
            "0\n/devtools/browser/id",
            "65536\n/devtools/browser/id",
            "-1\n/devtools/browser/id",
            "9222\nws://remote.example/id",
            "9222\n//remote.example/id",
            "9222\n/devtools/browser/id?remote=yes",
            "9222\n/devtools/browser/id/extra",
            "9222\n/devtools/browser/",
            "9222\n/devtools/browser/id\nextra",
        ] {
            assert!(parse_endpoint(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn managed_arguments_preserve_background_startup_and_security() {
        let args = managed_arguments(Path::new("/private/profile with spaces"));
        assert!(args.contains(&"--user-data-dir=/private/profile with spaces".into()));
        assert!(args.contains(&"--remote-debugging-port=0".into()));
        assert!(args.contains(&"--no-startup-window".into()));
        for arg in args {
            let arg = arg.to_string_lossy();
            assert!(
                !arg.contains("headless")
                    && !arg.contains("no-sandbox")
                    && !arg.contains("disable-web-security")
                    && !arg.contains("ignore-certificate-errors")
            );
        }
    }

    #[test]
    fn chrome_internal_pages_are_not_attached_as_test_tabs() {
        assert!(web_tab("http://localhost:5173"));
        assert!(web_tab("https://example.com"));
        for url in [
            "about:blank",
            EXISTING_SETUP_URL,
            "chrome-extension://abc/index.html",
            "file:///private/secret",
            "data:text/html,hello",
        ] {
            assert!(!web_tab(url));
        }
    }

    #[test]
    fn stop_needs_explicit_activation_and_does_not_change_another_session() {
        let first = Session::new();
        let second = Session::new();
        let service = ChromeComputerUseService::new();
        let stop = first.begin_consent();
        first.activate(service.clone(), &stop).unwrap();
        first.stop();
        assert!(service.ownership().is_tripped());
        assert!(first.activate(service.clone(), &stop).is_err());
        let prompt_stop = first.begin_consent();
        assert!(first.control.lock().unwrap().paused);
        assert!(service.ownership().is_tripped());
        assert!(!second.control.lock().unwrap().paused);
        first.activate(service.clone(), &prompt_stop).unwrap();
        assert!(!service.ownership().is_tripped());
    }

    #[test]
    fn cancellation_or_revocation_cannot_activate_a_new_connection() {
        let session = Session::new();
        let stop = session.begin_consent();
        session.revoked.store(true, Ordering::Release);
        assert!(session
            .activate(ChromeComputerUseService::new(), &stop)
            .is_err());
        assert!(session.control.lock().unwrap().paused);
    }

    #[cfg(unix)]
    #[test]
    fn managed_process_and_owner_private_profile_end_together() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fake-chrome");
        std::fs::write(&executable, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let managed = spawn_managed(&executable, &root.path().join("profiles")).unwrap();
        let profile = managed._profile.path().to_owned();
        assert_eq!(std::fs::metadata(&profile).unwrap().mode() & 0o077, 0);
        let pid = i32::try_from(managed.child.id()).unwrap();
        drop(managed);
        assert!(!profile.exists());
        // SAFETY: signal 0 only probes whether the reaped child still exists.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "launches a visible isolated Chrome window"]
    async fn managed_chrome_launches_connects_and_cleans_up() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let binary = installed_chrome(&home).unwrap();
        verify_chrome_version(&binary).await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut managed = spawn_managed(&binary, &root.path().join("profiles")).unwrap();
        let profile = managed._profile.path().to_owned();
        let (endpoint, cdp) = tokio::time::timeout(
            Duration::from_secs(20),
            connect_profile(&profile, Some(&mut managed.child)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(endpoint.starts_with("ws://127.0.0.1:"));
        let version = cdp.command("Browser.getVersion", json!({})).await.unwrap();
        assert!(version["product"].as_str().unwrap().starts_with("Chrome/"));
        cdp.close();
        drop(managed);
        assert!(!profile.exists());
    }

    #[test]
    fn endpoint_discovery_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        assert!(read_endpoint(root.path()).unwrap().is_none());
        std::fs::write(root.path().join("actual"), "9222\n/devtools/browser/abc").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.path().join("actual"),
                root.path().join("DevToolsActivePort"),
            )
            .unwrap();
            assert!(read_endpoint(root.path()).is_err());
        }
    }
}
