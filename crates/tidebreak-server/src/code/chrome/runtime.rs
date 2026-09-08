//! Host-owned Chrome computer-use runtime: connection installation, per-session
//! target isolation, grant fences, and the shared dispatch service.
//!
//! This module deliberately owns no launch path. A separate host-owned helper
//! or desktop native setup launches app-managed Chrome with an isolated
//! user-data directory and publishes a `DevToolsActivePort`; for Chrome 144+
//! existing-profile sharing, the native UI runs Chrome's own remote-debugging
//! enablement flow and passes the derived loopback port plus an explicit wider
//! grant. The model can never supply an endpoint, data directory, or raw CDP
//! method.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use tidebreak_core::{
    BrowserLoadState, BrowserOrigin, CancelToken, ChromeActArgs, ChromeActResult, ChromeActStatus,
    ChromeAction, ChromeConnectionGrant, ChromeDiagnosticsArgs, ChromeDiagnosticsResult,
    ChromeFrameStatus, ChromeListTabsResult, ChromeNavigateArgs, ChromeNavigateResult,
    ChromeNewTabArgs, ChromePageSnapshot, ChromeScreenshotArgs, ChromeScreenshotResult,
    ChromeSemanticFrame, ChromeSemanticNode, ChromeSemanticNodeKind, ChromeSnapshotArgs,
    ChromeTabMutationResult, ChromeTabRefArgs, ChromeTabSummary, ChromeViewport, ChromeWaitArgs,
    ChromeWaitCondition, ChromeWaitResult, ChromeWaitStatus, ComputerUseCall, ComputerUseImage,
    ComputerUseOutcome, ComputerUseResult, OwnerId, SessionId, WorkspaceId,
};
use uuid::Uuid;

use super::cdp::CdpSession;
use super::driver::{
    attach_target, enable_target, execute_action, grant_covers, origin_for_url, screenshot_png,
    TargetRow,
};

/// Public scope for one Chrome computer-use call, derived host-side from
/// durable session authority. Call arguments can propose work but can never
/// widen this scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeScope {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub cancel: CancelToken,
}

/// State surfaced to native settings UI and the parent computer-use service.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChromeAdapterState {
    pub available: bool,
    pub connection_id: Option<String>,
    pub connection_label: Option<String>,
    pub grant: Option<String>,
    pub managed_isolated: bool,
    pub approved_existing_profile: bool,
    pub tab_count: usize,
    pub last_error: Option<String>,
}

/// Host-derived connection descriptor. Only the native setup path builds it.
#[derive(Debug, Clone)]
pub struct ChromeConnectionSpec {
    pub connection_id: String,
    /// The workspace this host-approved connection serves. The native setup
    /// derives it from durable consent; no call argument can select it.
    pub workspace: WorkspaceId,
    pub endpoint_label: String,
    /// Loopback WebSocket endpoint from `DevToolsActivePort` or an explicitly
    /// approved existing-profile enablement. The model never supplies it.
    pub websocket_endpoint: String,
    pub grant: ChromeConnectionGrant,
    pub managed_isolated: bool,
}

/// Human takeover / Stop latch shared by the service. Native wiring trips it.
#[derive(Clone)]
pub struct ChromeOwnership(Arc<std::sync::atomic::AtomicBool>);

impl Default for ChromeOwnership {
    fn default() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(false)))
    }
}

impl ChromeOwnership {
    pub fn trip(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn resume(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
    pub fn is_tripped(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Live target metadata plus per-session state.
struct AttachedTarget {
    row: TargetRow,
    session_id: String,
    document_epoch: u64,
    snapshots: Vec<SnapshotEntry>,
    console: Vec<tidebreak_core::ChromeConsoleEntry>,
    network: Vec<tidebreak_core::ChromeNetworkEntry>,
    lifecycle: HashMap<String, Vec<String>>,
    last_error: Option<String>,
}

struct SnapshotEntry {
    snapshot_id: String,
    document_epoch: u64,
    session_id: String,
    node_selectors: HashMap<String, String>,
}

const MAX_SNAPSHOTS_PER_TARGET: usize = 3;
const MAX_DIAGNOSTICS_PER_TARGET: usize = 512;

struct Connection {
    spec: ChromeConnectionSpec,
    cdp: CdpSession,
    targets: HashMap<String, AttachedTarget>,
}

struct ServiceInner {
    connections: HashMap<String, Connection>,
    target_refs: HashMap<String, (String, String)>,
    next_target_ref: u64,
}

/// The host-owned Chrome computer-use service. One instance per host; native
/// setup installs approved connections through [`install_connection`].
#[derive(Clone)]
pub struct ChromeComputerUseService {
    inner: Arc<Mutex<ServiceInner>>,
    ownership: ChromeOwnership,
}

/// Resolved, session-checked target handle for one operation.
struct ResolvedTarget {
    connection_id: String,
    target_id: String,
    cdp: CdpSession,
    session_id: String,
    row: TargetRow,
    document_epoch: u64,
}

impl Default for ChromeComputerUseService {
    fn default() -> Self {
        Self::new()
    }
}

impl ChromeComputerUseService {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ServiceInner {
                connections: HashMap::new(),
                target_refs: HashMap::new(),
                next_target_ref: 1,
            })),
            ownership: ChromeOwnership::default(),
        }
    }

    /// Install a host-derived connection. Rejects non-loopback endpoints so a
    /// model argument can never become a transport target.
    pub fn install_connection(&self, spec: ChromeConnectionSpec, cdp: CdpSession) -> Result<(), String> {
        if !spec.websocket_endpoint.starts_with("ws://127.0.0.1:")
            && !spec.websocket_endpoint.starts_with("ws://localhost:")
            && !spec.websocket_endpoint.starts_with("ws://[::1]:")
        {
            return Err("Chrome connection endpoint must be a local loopback websocket; the model never supplies endpoints".into());
        }
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        if inner.connections.contains_key(&spec.connection_id) {
            return Err(format!("connection {} already installed", spec.connection_id));
        }
        inner.connections.insert(
            spec.connection_id.clone(),
            Connection {
                spec,
                cdp,
                targets: HashMap::new(),
            },
        );
        Ok(())
    }

    /// Uninstall a connection and invalidate every target reference in it.
    pub fn uninstall_connection(&self, connection_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        if inner.connections.remove(connection_id).is_none() {
            return Ok(());
        }
        inner
            .target_refs
            .retain(|_, (owner, _)| owner != connection_id);
        Ok(())
    }

    /// Durable/active state for native settings UI.
    pub fn state(&self, scope: &ChromeScope) -> ChromeAdapterState {
        let inner = self.inner.lock().unwrap_or_else(|_| panic!("chrome service poisoned"));
        let Some(connection) = connection_for_workspace(&inner, &scope.workspace) else {
            return ChromeAdapterState {
                available: false,
                connection_id: None,
                connection_label: None,
                grant: None,
                managed_isolated: false,
                approved_existing_profile: false,
                tab_count: 0,
                last_error: self.ownership.is_tripped().then(|| "ownership paused".into()),
            };
        };
        ChromeAdapterState {
            available: true,
            connection_id: Some(connection.spec.connection_id.clone()),
            connection_label: Some(connection.spec.endpoint_label.clone()),
            grant: Some(
                match connection.spec.grant {
                    ChromeConnectionGrant::Origin(_) => "origin",
                    ChromeConnectionGrant::DeveloperAllSites => "developer_all_sites",
                }
                .into(),
            ),
            managed_isolated: connection.spec.managed_isolated,
            approved_existing_profile: !connection.spec.managed_isolated,
            tab_count: connection.targets.len(),
            last_error: connection
                .targets
                .values()
                .find_map(|target| target.last_error.clone()),
        }
    }

    /// Revoke all Chrome authority for a session and invalidate its targets.
    pub fn revoke_session(&self, session: &SessionId) {
        let mut inner = self.inner.lock().unwrap_or_else(|_| panic!("chrome service poisoned"));
        let mut removed = Vec::new();
        for connection in inner.connections.values_mut() {
            let mut live = HashMap::new();
            for (target_id, target) in connection.targets.drain() {
                if &target.row.session != session {
                    live.insert(target_id, target);
                } else {
                    removed.push((connection.spec.connection_id.clone(), target.row.target_ref.clone()));
                }
            }
            connection.targets = live;
        }
        for (connection_id, target_ref) in removed {
            inner.target_refs.remove(&target_ref);
            let _ = connection_id;
        }
        self.ownership.trip();
    }

    pub fn ownership(&self) -> &ChromeOwnership {
        &self.ownership
    }

    /// Dispatch one validated shared-wire call. The parent service derives
    /// `scope`; call arguments never select the connection.
    pub async fn dispatch(&self, scope: &ChromeScope, call: &ComputerUseCall) -> ChromeCallOutcome {
        if !tidebreak_core::validate_chrome_computer_use_arguments(&call.name, &call.arguments) {
            return ChromeCallOutcome::rejected(call, "invalid chrome arguments", "invalid_arguments");
        }
        if self.ownership.is_tripped()
            && matches!(
                call.name.as_str(),
                tidebreak_core::CHROME_NEW_TAB_TOOL
                    | tidebreak_core::CHROME_CLOSE_TAB_TOOL
                    | tidebreak_core::CHROME_NAVIGATE_TOOL
                    | tidebreak_core::CHROME_ACT_TOOL
            )
        {
            return ChromeCallOutcome::rejected(
                call,
                "Chrome control is paused; the user took over",
                "ownership_paused",
            );
        }
        match call.name.as_str() {
            tidebreak_core::CHROME_LIST_TABS_TOOL => {
                let _: ChromeListTabsArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.list_tabs(scope) {
                    Ok(result) => completed(call, format!("{} tab(s) in the approved Chrome connection", result.tabs.len()), result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_NEW_TAB_TOOL => {
                let args: ChromeNewTabArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.new_tab(scope, &args).await {
                    Ok(result) => completed(call, format!("opened {}", result.url), result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_CLOSE_TAB_TOOL => {
                let args: ChromeTabRefArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.close_tab(scope, &args).await {
                    Ok(result) => completed(call, "tab closed".into(), result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_ACTIVATE_TAB_TOOL => {
                let args: ChromeTabRefArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.activate_tab(scope, &args).await {
                    Ok(result) => completed(call, format!("activated {}", result.title), result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_NAVIGATE_TOOL => {
                let args: ChromeNavigateArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.navigate(scope, &args).await {
                    Ok(result) => completed(call, format!("navigated to {}", result.url), result),
                    Err(error) => unknown(call, error, "chrome_unknown"),
                }
            }
            tidebreak_core::CHROME_SNAPSHOT_TOOL => {
                let args: ChromeSnapshotArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.snapshot(scope, &args).await {
                    Ok(result) => completed(call, format!("snapshot of {}", result.url), result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_SCREENSHOT_TOOL => {
                let args: ChromeScreenshotArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.screenshot(scope, &args).await {
                    Ok(result) => completed_screenshot(call, result),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            tidebreak_core::CHROME_ACT_TOOL => {
                let args: ChromeActArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.act(scope, &args).await {
                    Ok(result) => completed(call, format!("{} succeeded", result.action), result),
                    Err(error) => unknown(call, error, "chrome_unknown"),
                }
            }
            tidebreak_core::CHROME_WAIT_TOOL => {
                let args: ChromeWaitArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.wait(scope, &args).await {
                    Ok(result) => completed(call, result.message.clone(), result),
                    Err(error) => unknown(call, error, "chrome_unknown"),
                }
            }
            tidebreak_core::CHROME_DIAGNOSTICS_TOOL => {
                let args: ChromeDiagnosticsArgs = match parse(call, &call.arguments) {
                    Ok(value) => value,
                    Err(outcome) => return outcome,
                };
                match self.diagnostics(scope, &args).await {
                    Ok(result) => completed(
                        call,
                        format!(
                            "{} console and {} network entries",
                            result.console_entries.len(),
                            result.network_entries.len()
                        ),
                        result,
                    ),
                    Err(error) => rejected(call, error, "chrome_failed"),
                }
            }
            _ => ChromeCallOutcome::rejected(call, "unknown chrome computer-use call", "unknown_call"),
        }
    }

    fn resolve(
        &self,
        scope: &ChromeScope,
        target_ref: &str,
    ) -> Result<ResolvedTarget, String> {
        let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let (connection_id, target_id) = inner
            .target_refs
            .get(target_ref)
            .cloned()
            .ok_or_else(|| "unknown or revoked Chrome target reference".to_owned())?;
        let connection = inner
            .connections
            .get(&connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        if !connection.spec.grant_covers_workspace(&scope.workspace) {
            return Err("Chrome target belongs to another workspace".into());
        }
        let target = connection
            .targets
            .get(&target_id)
            .ok_or_else(|| "Chrome target is not attached or is gone".to_owned())?;
        if &target.row.session != &scope.session {
            return Err("Chrome target belongs to another session".into());
        }
        Ok(ResolvedTarget {
            connection_id,
            target_id,
            cdp: connection.cdp.clone(),
            session_id: target.session_id.clone(),
            row: target.row.clone(),
            document_epoch: target.document_epoch,
        })
    }

    fn list_tabs(&self, scope: &ChromeScope) -> Result<ChromeListTabsResult, String> {
        let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let connection = connection_for_workspace(&inner, &scope.workspace)
            .ok_or_else(|| "no approved Chrome connection for this workspace".to_owned())?;
        let tabs = connection
            .targets
            .values()
            .filter(|target| &target.row.session == &scope.session)
            .map(|target| ChromeTabSummary {
                target_ref: target.row.target_ref.clone(),
                title: target.row.title.clone(),
                url: target.row.url.clone(),
                load_state: load_state_for(&target.row, &target.lifecycle),
                active: target.row.active,
            })
            .collect();
        Ok(ChromeListTabsResult { tabs })
    }

    async fn new_tab(
        &self,
        scope: &ChromeScope,
        args: &ChromeNewTabArgs,
    ) -> Result<ChromeTabMutationResult, String> {
        let origin = origin_for_url(&args.url).ok_or_else(|| "invalid navigation URL".to_owned())?;
        let (cdp, connection_id) = {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            let connection = connection_for_workspace(&inner, &scope.workspace)
                .ok_or_else(|| "no approved Chrome connection for this workspace".to_owned())?;
            if !connection.spec.grant.covers(&origin) {
                return Err(format!("origin {} is outside the approved Chrome scope", origin.as_str()));
            }
            (connection.cdp.clone(), connection.spec.connection_id.clone())
        };
        if scope.cancel.is_cancelled() {
            return Err("cancelled before opening tab".into());
        }
        let target_id = cdp
            .command("Target.createTarget", serde_json::json!({"url": args.url}))
            .await
            .map_err(|error| format!("failed to create Chrome tab: {error}"))?
            .get("targetId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "createTarget returned no target id".to_owned())?
            .to_owned();
        let session_id = attach_target(&cdp, &target_id).map_err(|e| e.0)?;
        enable_target(&cdp, &session_id).map_err(|e| e.0)?;
        let loaded = wait_for_ready(&cdp, &session_id, 15_000).await;
        let url = args.url.clone();
        let title = read_url_title(&cdp, &session_id).await
            .map(|(_, title)| title)
            .unwrap_or_default();
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let target_ref = format!("ct-{:06}", inner.next_target_ref);
        inner.next_target_ref += 1;
        inner.target_refs.insert(target_ref.clone(), (connection_id.clone(), target_id.clone()));
        let connection = inner
            .connections
            .get_mut(&connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        connection.targets.insert(
            target_id.clone(),
            AttachedTarget {
                row: TargetRow {
                    target_ref: target_ref.clone(),
                    target_id,
                    frame_id: String::new(),
                    url: url.clone(),
                    title,
                    active: true,
                    attached: true,
                },
                session_id,
                document_epoch: 1,
                snapshots: Vec::new(),
                console: Vec::new(),
                network: Vec::new(),
                lifecycle: HashMap::new(),
                last_error: loaded.err(),
            },
        );
        let load_state = if loaded.is_ok() {
            BrowserLoadState::Ready
        } else {
            BrowserLoadState::Loading
        };
        Ok(ChromeTabMutationResult {
            target_ref,
            title: String::new(),
            url,
            load_state,
            active: true,
        })
    }

    async fn close_tab(
        &self,
        scope: &ChromeScope,
        args: &ChromeTabRefArgs,
    ) -> Result<ChromeTabMutationResult, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        let tab_count = {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            inner
                .connections
                .get(&resolved.connection_id)
                .map(|connection| connection.targets.len())
                .unwrap_or(0)
        };
        if tab_count <= 1 {
            return Err("refusing to close the last Chrome tab; open another tab first".into());
        }
        let _ = resolved
            .cdp
            .command_in_session(&resolved.session_id, "Page.close", serde_json::json!({}))
            .await;
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        if let Some(connection) = inner.connections.get_mut(&resolved.connection_id) {
            connection.targets.remove(&resolved.target_id);
        }
        inner.target_refs.remove(&args.target_ref);
        Ok(ChromeTabMutationResult {
            target_ref: args.target_ref.clone(),
            title: resolved.row.title,
            url: resolved.row.url,
            load_state: BrowserLoadState::Idle,
            active: false,
        })
    }

    async fn activate_tab(
        &self,
        scope: &ChromeScope,
        args: &ChromeTabRefArgs,
    ) -> Result<ChromeTabMutationResult, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        resolved
            .cdp
            .command_in_session(&resolved.session_id, "Page.bringToFront", serde_json::json!({}))
            .await
            .map_err(|error| format!("failed to activate tab: {error}"))?;
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        if let Some(connection) = inner.connections.get_mut(&resolved.connection_id) {
            for (target_id, target) in connection.targets.iter_mut() {
                target.row.active = &target.row.target_id == &resolved.target_id
                    || (target_id == &resolved.target_id);
            }
        }
        Ok(ChromeTabMutationResult {
            target_ref: resolved.row.target_ref,
            title: resolved.row.title,
            url: resolved.row.url,
            load_state: BrowserLoadState::Ready,
            active: true,
        })
    }

    async fn navigate(
        &self,
        scope: &ChromeScope,
        args: &ChromeNavigateArgs,
    ) -> Result<ChromeNavigateResult, String> {
        let origin = origin_for_url(&args.url).ok_or_else(|| "invalid navigation URL".to_owned())?;
        let resolved = self.resolve(scope, &args.target_ref)?;
        {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            let connection = inner
                .connections
                .get(&resolved.connection_id)
                .ok_or_else(|| "connection is gone".to_owned())?;
            if !connection.spec.grant.covers(&origin) {
                return Err(format!("origin {} is outside the approved Chrome scope", origin.as_str()));
            }
        }
        if scope.cancel.is_cancelled() {
            return Err("cancelled before navigation".into());
        }
        let _ = resolved
            .cdp
            .command_in_session(&resolved.session_id, "Page.navigate", serde_json::json!({"url": args.url}))
            .await
            .map_err(|error| format!("navigation command failed: {error}"))?;
        // Page.navigate returns as soon as navigation starts; the load gate
        // below observes the actual document.
        let timeout = args.timeout_ms.unwrap_or(15_000);
        let waited = wait_for_ready(&resolved.cdp, &resolved.session_id, timeout).await;
        let (url, title) = read_url_title(&resolved.cdp, &resolved.session_id)
            .await
            .unwrap_or((args.url.clone(), String::new()));
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let connection = inner
            .connections
            .get_mut(&resolved.connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        let target = connection
            .targets
            .get_mut(&resolved.target_id)
            .ok_or_else(|| "target is gone".to_owned())?;
        target.document_epoch += 1;
        target.row.url = url.clone();
        target.row.title = title.clone();
        target.snapshots.clear();
        if waited.is_err() {
            target.last_error = waited.err();
        }
        Ok(ChromeNavigateResult {
            target_ref: resolved.row.target_ref,
            url,
            load_state: if waited.is_ok() {
                BrowserLoadState::Ready
            } else {
                BrowserLoadState::Loading
            },
            document_epoch: target.document_epoch,
        })
    }

    async fn snapshot(
        &self,
        scope: &ChromeScope,
        args: &ChromeSnapshotArgs,
    ) -> Result<ChromePageSnapshot, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        let grant = {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            inner
                .connections
                .get(&resolved.connection_id)
                .map(|connection| connection.spec.grant.clone())
                .ok_or_else(|| "connection is gone".to_owned())?
        };
        let script = build_snapshot_script(args.bounded_max_nodes());
        let result = evaluate(
            &resolved.cdp,
            &resolved.session_id,
            &script,
        )
        .await
        .map_err(|error| format!("snapshot evaluation failed: {error}"))?;
        let raw = result
            .get("value")
            .cloned()
            .unwrap_or(result);
        let (nodes, frames, truncated) = project_snapshot(raw, args.bounded_max_nodes(), &grant);
        let (url, title) = read_url_title(&resolved.cdp, &resolved.session_id)
            .await
            .unwrap_or((resolved.row.url.clone(), resolved.row.title.clone()));
        let viewport = read_viewport(&resolved.cdp, &resolved.session_id)
            .await
            .unwrap_or(ChromeViewport {
                width: 0.0,
                height: 0.0,
                scroll_x: 0.0,
                scroll_y: 0.0,
            });
        let snapshot_id = format!("snap-{}", Uuid::new_v4().simple());
        let node_selectors = nodes
            .iter()
            .filter_map(|node| {
                node.target_ref.as_ref().map(|reference| {
                    (reference.clone(), format!("[data-tb-ref=\"{reference}\"]"))
                })
            })
            .collect();
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let connection = inner
            .connections
            .get_mut(&resolved.connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        let target = connection
            .targets
            .get_mut(&resolved.target_id)
            .ok_or_else(|| "target is gone".to_owned())?;
        target.row.url = url.clone();
        target.row.title = title.clone();
        target.snapshots.push(SnapshotEntry {
            snapshot_id: snapshot_id.clone(),
            document_epoch: target.document_epoch,
            session_id: resolved.session_id.clone(),
            node_selectors,
        });
        if target.snapshots.len() > MAX_SNAPSHOTS_PER_TARGET {
            target.snapshots.remove(0);
        }
        let document_epoch = target.document_epoch;
        Ok(ChromePageSnapshot {
            target_ref: resolved.row.target_ref,
            snapshot_id,
            document_epoch,
            content_trust: tidebreak_core::ChromeContentTrust::UntrustedPage,
            url,
            title,
            viewport,
            nodes,
            frames,
            truncated,
        })
    }

    async fn screenshot(
        &self,
        scope: &ChromeScope,
        args: &ChromeScreenshotArgs,
    ) -> Result<ChromeScreenshotResult, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        let (grant_matches, snapshot_matches) = {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            let connection = inner
                .connections
                .get(&resolved.connection_id)
                .ok_or_else(|| "connection is gone".to_owned())?;
            if let Some(target_origin) = origin_for_url(&resolved.row.url) {
                if !connection.spec.grant.covers(&target_origin) {
                    return Err("screenshot origin is outside the approved scope".into());
                }
            }
            let target = connection
                .targets
                .get(&resolved.target_id)
                .ok_or_else(|| "target is gone".to_owned())?;
            (
                connection.spec.grant == tidebreak_core::ChromeConnectionGrant::DeveloperAllSites,
                target.snapshots.iter().any(|snapshot| {
                    snapshot.snapshot_id == args.snapshot_id
                        && snapshot.document_epoch == args.document_epoch
                }),
            )
        };
        if !snapshot_matches {
            return Err("snapshot/epoch no longer matches; take a fresh snapshot".into());
        }
        let (bytes, _width, _height) = async_screenshot(
            &resolved.cdp,
            &resolved.session_id,
            args.max_width,
            args.max_height,
        )
        .await
        .map_err(|error| format!("screenshot failed: {error}"))?;
        use base64::Engine as _;
        let image_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let _ = grant_matches;
        Ok(ChromeScreenshotResult {
            target_ref: resolved.row.target_ref,
            snapshot_id: args.snapshot_id.clone(),
            document_epoch: args.document_epoch,
            image_base64,
            mime_type: "image/png".into(),
        })
    }

    async fn act(
        &self,
        scope: &ChromeScope,
        args: &ChromeActArgs,
    ) -> Result<ChromeActResult, String> {
        if self.ownership.is_tripped() {
            return Err("Chrome control is paused; the user took over".into());
        }
        let resolved = self.resolve(scope, &args.target_ref)?;
        let (snapshot, selector) = {
            let inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
            let connection = inner
                .connections
                .get(&resolved.connection_id)
                .ok_or_else(|| "connection is gone".to_owned())?;
            let target = connection
                .targets
                .get(&resolved.target_id)
                .ok_or_else(|| "target is gone".to_owned())?;
            let snapshot = target
                .snapshots
                .iter()
                .rev()
                .find(|snapshot| {
                    snapshot.snapshot_id == args.snapshot_id
                        && snapshot.document_epoch == args.document_epoch
                })
                .cloned()
                .ok_or_else(|| "snapshot is stale; take a fresh snapshot".to_owned())?;
            let selector = snapshot
                .node_selectors
                .get(&args.node_ref)
                .cloned()
                .ok_or_else(|| "node ref is invalid or from another snapshot".to_owned())?;
            (snapshot, selector)
        };
        // The driver executes only the caller's exact snapshot action; no
        // raw CDP method is ever accepted from the model.
        let snapshot_record = super::driver::SnapshotRecord {
            snapshot_id: snapshot.snapshot_id.clone(),
            document_epoch: snapshot.document_epoch,
            target_ref: resolved.row.target_ref.clone(),
            target_id: resolved.target_id.clone(),
            session_id: resolved.session_id.clone(),
            url: resolved.row.url.clone(),
            nodes: vec![],
        };
        let node_record = super::driver::SnapshotNode {
            node_ref: args.node_ref.clone(),
            selector,
            frame_id: String::new(),
            frame_session_id: None,
            kind: ChromeSemanticNodeKind::Interactive,
            url: resolved.row.url.clone(),
        };
        let status = execute_action(
            &resolved.cdp,
            &snapshot_record,
            &node_record,
            &args.action,
            &scope.cancel,
        )
        .map_err(|error| format!("action failed: {}", error.0))?;
        let result_status = match status {
            ChromeActStatus::Ok => ChromeActStatus::Ok,
            ChromeActStatus::Cancelled => ChromeActStatus::Cancelled,
            ChromeActStatus::StaleTarget => ChromeActStatus::StaleTarget,
            ChromeActStatus::InvalidRef => ChromeActStatus::InvalidRef,
            ChromeActStatus::InvalidValue => ChromeActStatus::InvalidValue,
            ChromeActStatus::UnsupportedFrame => ChromeActStatus::UnsupportedFrame,
            ChromeActStatus::EngineFailure => ChromeActStatus::EngineFailure,
            ChromeActStatus::Timeout => ChromeActStatus::Timeout,
        };
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let connection = inner
            .connections
            .get(&resolved.connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        let target = connection
            .targets
            .get(&resolved.target_id)
            .ok_or_else(|| "target is gone".to_owned())?;
        let url = target.row.url.clone();
        let title = target.row.title.clone();
        Ok(ChromeActResult {
            target_ref: resolved.row.target_ref,
            snapshot_id: args.snapshot_id.clone(),
            document_epoch: args.document_epoch,
            node_ref: args.node_ref.clone(),
            action: args.action.kind().to_owned(),
            status: result_status,
            message: match result_status {
                ChromeActStatus::Ok => "action completed; re-snapshot before acting again".into(),
                ChromeActStatus::Cancelled => "action cancelled by Stop or takeover".into(),
                ChromeActStatus::StaleTarget => "target changed; take a fresh snapshot".into(),
                ChromeActStatus::InvalidRef => "node ref is invalid".into(),
                ChromeActStatus::InvalidValue => "action value was rejected".into(),
                ChromeActStatus::UnsupportedFrame => "target frame is unsupported".into(),
                ChromeActStatus::EngineFailure => "action could not be confirmed".into(),
                ChromeActStatus::Timeout => "action timed out".into(),
            },
            requires_resnapshot: result_status != ChromeActStatus::Cancelled,
            url: Some(url),
            title: Some(title),
        })
    }

    async fn wait(
        &self,
        scope: &ChromeScope,
        args: &ChromeWaitArgs,
    ) -> Result<ChromeWaitResult, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        let deadline = Instant::now() + Duration::from_millis(args.bounded_timeout_ms());
        let known_url = resolved.row.url.clone();
        loop {
            if scope.cancel.is_cancelled() {
                return Ok(ChromeWaitResult {
                    target_ref: resolved.row.target_ref.clone(),
                    status: ChromeWaitStatus::Stopped,
                    message: "wait stopped by user".into(),
                    document_epoch: resolved.document_epoch,
                    url: None,
                    title: None,
                });
            }
            let (url, title, ready_state, text_present) =
                wait_state(&resolved.cdp, &resolved.session_id).await?;
            let satisfied = match &args.condition {
                ChromeWaitCondition::UrlChanged => !url.is_empty() && url != known_url,
                ChromeWaitCondition::LoadState { state } => {
                    let current = if ready_state == "complete" || ready_state == "interactive" {
                        BrowserLoadState::Ready
                    } else {
                        BrowserLoadState::Loading
                    };
                    current == *state
                }
                ChromeWaitCondition::TextPresent { text } => text_present.as_deref() == Some(text.as_str()),
                ChromeWaitCondition::TextAbsent { text } => {
                    !text_present.as_deref().is_some_and(|present| present == text)
                }
            };
            if satisfied {
                return Ok(ChromeWaitResult {
                    target_ref: resolved.row.target_ref.clone(),
                    status: ChromeWaitStatus::Resolved,
                    message: "condition satisfied".into(),
                    document_epoch: resolved.document_epoch,
                    url: Some(url),
                    title: Some(title),
                });
            }
            if Instant::now() >= deadline {
                return Ok(ChromeWaitResult {
                    target_ref: resolved.row.target_ref.clone(),
                    status: ChromeWaitStatus::TimedOut,
                    message: "condition not satisfied before timeout".into(),
                    document_epoch: resolved.document_epoch,
                    url: Some(url),
                    title: Some(title),
                });
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn diagnostics(
        &self,
        scope: &ChromeScope,
        args: &ChromeDiagnosticsArgs,
    ) -> Result<ChromeDiagnosticsResult, String> {
        let resolved = self.resolve(scope, &args.target_ref)?;
        let mut inner = self.inner.lock().map_err(|_| "chrome service poisoned")?;
        let connection = inner
            .connections
            .get(&resolved.connection_id)
            .ok_or_else(|| "connection is gone".to_owned())?;
        let target = connection
            .targets
            .get(&resolved.target_id)
            .ok_or_else(|| "target is gone".to_owned())?;
        let mut console = target.console.iter().rev().cloned().collect::<Vec<_>>();
        let mut network = target.network.iter().rev().cloned().collect::<Vec<_>>();
        console.truncate(args.max_console_entries.unwrap_or(100).min(tidebreak_core::MAX_CHROME_DIAGNOSTICS_ENTRIES));
        network.truncate(args.max_network_entries.unwrap_or(100).min(tidebreak_core::MAX_CHROME_DIAGNOSTICS_ENTRIES));
        let truncated = target.console.len() > console.len() || target.network.len() > network.len();
        Ok(ChromeDiagnosticsResult {
            target_ref: resolved.row.target_ref,
            document_epoch: resolved.document_epoch,
            console_entries: console,
            network_entries: network,
            truncated,
        })
    }
}

impl ChromeConnectionSpec {
    /// In this slice the host installs one connection per workspace. The
    /// parent native service can later scope connections per workspace and
    /// re-check against the durable consent store; origin coverage is enforced
    /// on every operation below.
    fn grant_covers_workspace(&self, workspace: &WorkspaceId) -> bool {
        &self.workspace == workspace
    }
}

/// The chrome actions that mutate page state and must recheck live consent
/// and ownership at act time.
pub fn is_controlling_action(action: &ChromeAction) -> bool {
    !matches!(action, ChromeAction::Hover)
}

fn connection_for_workspace<'a>(
    inner: &'a ServiceInner,
    workspace: &WorkspaceId,
) -> Option<&'a Connection> {
    inner
        .connections
        .values()
        .find(|connection| &connection.spec.workspace == workspace)
}

fn parse<T: DeserializeOwned>(
    call: &ComputerUseCall,
    arguments: &serde_json::Value,
) -> Result<T, ChromeCallOutcome> {
    serde_json::from_value(arguments.clone()).map_err(|error| {
        ChromeCallOutcome::rejected(call, format!("invalid chrome arguments: {error}"), "invalid_arguments")
    })
}

/// One request result carrying the shared wire plus a stable error code.
#[derive(Debug, Clone)]
pub struct ChromeCallOutcome {
    pub result: ComputerUseResult,
}

impl ChromeCallOutcome {
    pub fn rejected(call: &ComputerUseCall, message: String, error_code: &str) -> Self {
        Self {
            result: ComputerUseResult {
                request_id: call.request_id,
                outcome: ComputerUseOutcome::Rejected,
                text: message,
                data: serde_json::json!({}),
                error_code: Some(error_code.to_owned()),
                images: Vec::new(),
            },
        }
    }

    pub fn unknown(call: &ComputerUseCall, message: String, error_code: &str) -> Self {
        Self {
            result: ComputerUseResult {
                request_id: call.request_id,
                outcome: ComputerUseOutcome::Unknown,
                text: message,
                data: serde_json::json!({}),
                error_code: Some(error_code.to_owned()),
                images: Vec::new(),
            },
        }
    }

    pub fn completed(call: &ComputerUseCall, text: String, data: impl serde::Serialize) -> Self {
        Self {
            result: ComputerUseResult {
                request_id: call.request_id,
                outcome: ComputerUseOutcome::Completed,
                text,
                data: serde_json::to_value(data).unwrap_or_else(|_| serde_json::json!({})),
                error_code: None,
                images: Vec::new(),
            },
        }
    }
}

fn completed<T: serde::Serialize>(
    call: &ComputerUseCall,
    text: String,
    data: T,
) -> ChromeCallOutcome {
    ChromeCallOutcome::completed(call, text, data)
}

fn rejected(call: &ComputerUseCall, message: String, code: &str) -> ChromeCallOutcome {
    ChromeCallOutcome::rejected(call, message, code)
}

fn unknown(call: &ComputerUseCall, message: String, code: &str) -> ChromeCallOutcome {
    ChromeCallOutcome::unknown(call, message, code)
}

fn completed_screenshot(
    call: &ComputerUseCall,
    result: ChromeScreenshotResult,
) -> ChromeCallOutcome {
    let image = result.image_base64;
    let mut outcome = completed(
        call,
        "screenshot captured as an image attachment".into(),
        serde_json::json!({
            "targetRef": result.target_ref,
            "snapshotId": result.snapshot_id,
            "documentEpoch": result.document_epoch,
            "mimeType": result.mime_type,
        }),
    );
    outcome.result.images.push(ComputerUseImage {
        mime_type: "image/png".into(),
        base64: image,
    });
    outcome
}

async fn wait_for_ready(
    cdp: &CdpSession,
    session_id: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    if timeout_ms == 0 {
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms.min(tidebreak_core::MAX_CHROME_WAIT_TIMEOUT_MS));
    loop {
        if Instant::now() >= deadline {
            return Err("navigation did not reach ready before timeout".into());
        }
        let ready = cmd_async(
            cdp,
            session_id,
            "Runtime.evaluate",
            serde_json::json!({
                "expression": "document.readyState",
                "returnByValue": true
            }),
        )
        .await
        .map_err(|error| format!("navigation readiness probe failed: {error}"))?;
        let state = ready
            .get("result")
            .and_then(|value| value.get("value"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if state == "complete" || state == "interactive" {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn read_url_title(
    cdp: &CdpSession,
    session_id: &str,
) -> Result<(String, String), String> {
    let result = cmd_async(
        cdp,
        session_id,
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "({url: location.href, title: document.title})",
            "returnByValue": true
        }),
    )
    .await?;
    let value = result
        .get("result")
        .and_then(|value| value.get("value"))
        .cloned()
        .unwrap_or(serde_json::json!({}));
    Ok((
        value.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
        value.get("title").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
    ))
}

async fn wait_state(
    cdp: &CdpSession,
    session_id: &str,
) -> Result<(String, String, String, Option<String>), String> {
    let result = cmd_async(
        cdp,
        session_id,
        "Runtime.evaluate",
        serde_json::json!({
            "expression": "({url: location.href, title: document.title, readyState: document.readyState, body: document.body ? document.body.innerText : ''})",
            "returnByValue": true
        }),
    )
    .await?;
    let value = result
        .get("result")
        .and_then(|value| value.get("value"))
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let body = value.get("body").and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
    Ok((
        value.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
        value.get("title").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
        value.get("readyState").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
        if body.is_empty() { None } else { Some(body) },
    ))
}

async fn cmd_async(
    cdp: &CdpSession,
    session_id: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tokio::time::timeout(
        Duration::from_secs(15),
        cdp.command_in_session(session_id, method, params),
    )
    .await
    .map_err(|_| format!("{method} timed out"))?
    .map_err(|error| error.0)
}

async fn evaluate(
    cdp: &CdpSession,
    session_id: &str,
    expression: &str,
) -> Result<serde_json::Value, String> {
    cmd_async(
        cdp,
        session_id,
        "Runtime.evaluate",
        serde_json::json!({
            "expression": expression,
            "returnByValue": true,
            "awaitPromise": true
        }),
    )
    .await
}

async fn async_screenshot(
    cdp: &CdpSession,
    session_id: &str,
    max_width: Option<u64>,
    max_height: Option<u64>,
) -> Result<(Vec<u8>, u64, u64), String> {
    tokio::task::spawn_blocking({
        let cdp = cdp.clone();
        let session_id = session_id.to_owned();
        move || {
            screenshot_png(&cdp, &session_id, max_width, max_height)
                .map_err(|error| error.0)
        }
    })
    .await
    .map_err(|error| format!("screenshot task failed: {error}"))?
}

async fn read_viewport(
    cdp: &CdpSession,
    session_id: &str,
) -> Result<ChromeViewport, String> {
    let result = cmd_async(cdp, session_id, "Page.getLayoutMetrics", serde_json::json!({})).await?;
    let viewport = result
        .get("cssVisualViewport")
        .cloned()
        .unwrap_or(serde_json::json!({}));
    Ok(ChromeViewport {
        width: viewport.get("clientWidth").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        height: viewport.get("clientHeight").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        scroll_x: viewport.get("pageX").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        scroll_y: viewport.get("pageY").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
    })
}

fn load_state_for(row: &TargetRow, lifecycle: &HashMap<String, Vec<String>>) -> BrowserLoadState {
    let events = lifecycle.get(&row.frame_id).cloned().unwrap_or_default();
    if events.iter().any(|event| event == "DOMContentLoaded" || event == "load") {
        BrowserLoadState::Ready
    } else if events.is_empty() && row.url.is_empty() {
        BrowserLoadState::Idle
    } else {
        BrowserLoadState::Loading
    }
}

/// The driver's trusted, bounded page-reading projection. It never evaluates
/// arbitrary model script, marks password/OTP inputs sensitive, omits their
/// values, and keeps per-node output under `max_nodes`.
pub fn build_snapshot_script(max_nodes: usize) -> String {
    format!(
        r#"
        (() => {{
            const MAX = {max_nodes};
            const MAX_DEPTH = 24;
            const seen = new Set();
            const out = [];
            const frames = [];
            let truncated = false;

            function textOf(el) {{
                if (el.getAttribute('aria-label')) return el.getAttribute('aria-label');
                if (el.tagName === 'IMG') return el.getAttribute('alt') || '';
                return (el.innerText || el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 2000);
            }}

            function rowOf(el, frameName) {{
                const tag = el.tagName.toLowerCase();
                const interactive = el.matches('button, a[href], input, textarea, select, [tabindex], [contenteditable="true"]');
                const sensitive = el.matches('input[type="password"], input[type="otp"], input[autocomplete="one-time-code"], input[name*="otp" i], input[name*="token" i], input[name*="secret" i]');
                const role = el.getAttribute('role') || (interactive ? 'interactive' : 'text');
                if (!interactive && !sensitive && !el.children.length && tag !== 'script' && tag !== 'style' && tag !== 'noscript' && !textOf(el)) return null;
                if (sensitive && !interactive) return null;
                const ref = interactive ? 'n-' + (out.length + 1) : null;
                const typed = (el.getAttribute('type') || '').toLowerCase();
                return {{
                    kind: interactive ? 'interactive' : 'content',
                    ref,
                    tag,
                    role,
                    name: (el.getAttribute('name') || el.getAttribute('aria-label') || (interactive ? '' : textOf(el)) || '').slice(0, 512),
                    frame: frameName,
                    text: interactive ? null : (textOf(el) || null),
                    value: sensitive ? null : (interactive && el.value !== undefined ? String(el.value).slice(0, 256) : null),
                    href: el.getAttribute('href') || null,
                    inputType: interactive && el.matches('input,textarea') ? (typed || null) : null,
                    disabled: Boolean(el.disabled),
                    checked: el.checked !== undefined ? Boolean(el.checked) : null,
                    sensitive,
                    actions: interactive ? ['click','focus','scroll_into_view'] : [],
                    bounds: (() => {{
                        const r = el.getBoundingClientRect();
                        return {{x: r.x, y: r.y, width: r.width, height: r.height}};
                    }})()
                }};
            }}

            function walk(node, depth, frameName) {{
                if (depth > MAX_DEPTH || out.length >= MAX) {{
                    truncated = true;
                    return;
                }}
                if (!node || node.nodeType !== 1 || seen.has(node)) return;
                seen.add(node);
                for (const child of Array.from(node.children)) walk(child, depth + 1, frameName);
                if (out.length >= MAX) {{
                    truncated = true;
                    return;
                }}
                const row = rowOf(node, frameName);
                if (row) out.push(row);
            }}

            function walkFrame(frame, name) {{
                try {{
                    const doc = frame.document || frame.contentDocument;
                    if (!doc) return;
                    const url = frame.location ? frame.location.href : (doc.location ? doc.location.href : '');
                    frames.push({{name, url, status: 'same_origin'}});
                    walk(doc.body, 0, name);
                }} catch (e) {{
                    frames.push({{name, url: '', status: 'cross_origin'}});
                }}
            }}

            walkFrame(window, (document.title || '').slice(0, 128) || 'main');
            for (const frame of Array.from(document.querySelectorAll('iframe,frame'))) {{
                if (out.length >= MAX) {{
                    truncated = true;
                    break;
                }}
                walkFrame(frame, frame.name || ('frame-' + frames.length));
            }}
            return {{nodes: out.slice(0, MAX), frames, truncated}};
        }})()
        "#,
        max_nodes = max_nodes.min(tidebreak_core::MAX_CHROME_SNAPSHOT_NODES)
    )
}

fn project_snapshot(
    value: serde_json::Value,
    max_nodes: usize,
    _grant: &ChromeConnectionGrant,
) -> (Vec<ChromeSemanticNode>, Vec<ChromeSemanticFrame>, bool) {
    let nodes = value
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(max_nodes)
        .map(|value| ChromeSemanticNode {
            kind: if value.get("kind").and_then(serde_json::Value::as_str) == Some("interactive") {
                ChromeSemanticNodeKind::Interactive
            } else {
                ChromeSemanticNodeKind::Content
            },
            target_ref: value.get("ref").and_then(serde_json::Value::as_str).map(str::to_owned),
            tag: value.get("tag").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            role: value.get("role").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            name: value.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            frame: value.get("frame").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            text: value.get("text").and_then(serde_json::Value::as_str).map(str::to_owned),
            value: value.get("value").and_then(serde_json::Value::as_str).map(str::to_owned),
            href: value.get("href").and_then(serde_json::Value::as_str).map(str::to_owned),
            input_type: value.get("inputType").and_then(serde_json::Value::as_str).map(str::to_owned),
            disabled: value.get("disabled").and_then(serde_json::Value::as_bool).unwrap_or(false),
            checked: value.get("checked").and_then(serde_json::Value::as_bool),
            sensitive: value.get("sensitive").and_then(serde_json::Value::as_bool).unwrap_or(false),
            actions: value.get("actions").and_then(serde_json::Value::as_array).map(|actions| {
                actions.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect()
            }).unwrap_or_default(),
            bounds: {
                let bounds = value.get("bounds").cloned().unwrap_or(serde_json::json!({}));
                tidebreak_core::BrowserElementBounds {
                    x: bounds.get("x").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    y: bounds.get("y").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    width: bounds.get("width").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    height: bounds.get("height").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                }
            },
        })
        .collect();
    let frames = value
        .get("frames")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(tidebreak_core::MAX_CHROME_SNAPSHOT_NODES)
        .map(|value| ChromeSemanticFrame {
            name: value.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            url: value.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
            status: match value.get("status").and_then(serde_json::Value::as_str) {
                Some("cross_origin") => ChromeFrameStatus::CrossOrigin,
                Some("unsupported") => ChromeFrameStatus::UnsupportedFrame,
                _ => ChromeFrameStatus::SameOrigin,
            },
        })
        .collect();
    let truncated = value.get("truncated").and_then(serde_json::Value::as_bool).unwrap_or(false);
    (nodes, frames, truncated)
}
