//! One host-owned Chrome connection service. Native setup owns launch and consent.
//! Tool arguments never carry CDP endpoints, browser target ids, or profiles.
use super::cdp::{CdpEvent, CdpSession};
use super::driver::{PROBE_SCRIPT, SNAPSHOT_SCRIPT};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tidebreak_core::chrome_computer_use::ChromeContentTrust;
use tidebreak_core::computer_session::{
    ComputerUseCall, ComputerUseImage, ComputerUseOutcome, ComputerUseResult,
};
use tidebreak_core::*;
use uuid::Uuid;

#[derive(Clone)]
pub struct ChromeScope {
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub session: SessionId,
    pub cancel: CancelToken,
}
#[derive(Debug, Clone)]
pub struct ChromeConnectionSpec {
    pub connection_id: String,
    pub owner: OwnerId,
    pub workspace: WorkspaceId,
    pub endpoint_label: String,
    pub websocket_endpoint: String,
    pub grant: ChromeConnectionGrant,
    pub managed_isolated: bool,
}
#[derive(Debug, Clone, serde::Serialize)]
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
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChromeDiscoveredTab {
    pub target_id: String,
    pub title: String,
    pub url: String,
}
#[derive(Clone, Default)]
pub struct ChromeOwnership {
    paused: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}
impl ChromeOwnership {
    pub fn trip(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    pub fn resume(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
    }
    pub fn is_tripped(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
}
#[derive(Clone)]
struct Fence {
    connection: CancelToken,
    session: CancelToken,
    call: CancelToken,
    ownership: ChromeOwnership,
    generation: u64,
}
impl Fence {
    fn live(&self) -> bool {
        !self.connection.is_cancelled()
            && !self.session.is_cancelled()
            && !self.call.is_cancelled()
            && !self.ownership.is_tripped()
            && self.ownership.generation.load(Ordering::SeqCst) == self.generation
    }
    fn check(&self) -> Result<(), String> {
        if self.live() {
            Ok(())
        } else {
            Err("Chrome authority ended or control paused".into())
        }
    }
}
struct Connection {
    spec: ChromeConnectionSpec,
    cdp: CdpSession,
    cancel: CancelToken,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.cdp.close();
    }
}
#[derive(Clone)]
struct Frame {
    cdp_session: String,
    id: String,
    loader: String,
    url: String,
    name: String,
    /// Embedding frame id from the tree or CDP's parentId on a child-session
    /// root. A missing parent is valid only for the top frame.
    parent: Option<String>,
}
#[derive(Clone)]
struct Snapshot {
    id: String,
    epoch: u64,
    frames: Vec<Frame>,
    nodes: HashMap<String, (i64, String, String)>,
}
#[derive(Clone)]
struct Tab {
    connection: String,
    owner: OwnerId,
    workspace: WorkspaceId,
    session: SessionId,
    target_id: String,
    cdp_session: String,
    summary: ChromeTabSummary,
    document: String,
    epoch: u64,
    snapshot: Option<Snapshot>,
}
struct SessionState {
    owner: OwnerId,
    workspace: WorkspaceId,
    cancel: CancelToken,
}
struct Receipt {
    call: ComputerUseCall,
    result: ComputerUseResult,
}
#[derive(Default)]
struct Inner {
    connections: HashMap<String, Connection>,
    tabs: HashMap<String, Tab>,
    sessions: HashMap<SessionId, SessionState>,
    revoked: HashSet<SessionId>,
    receipts: HashMap<(SessionId, Uuid), Receipt>,
}
#[derive(Clone, Default)]
pub struct ChromeComputerUseService {
    inner: Arc<Mutex<Inner>>,
    serial: Arc<tokio::sync::Mutex<()>>,
    input_cleanup: Arc<tokio::sync::Mutex<()>>,
    ownership: ChromeOwnership,
}
#[derive(Clone)]
struct Access {
    cdp: CdpSession,
    grant: ChromeConnectionGrant,
    connection: String,
    fence: Fence,
}
/// Ceiling for one compensating release command after an error, cancel, or
/// dropped call, so cleanup stays bounded even when Chrome never answers.
const INPUT_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);
/// Default largest delivered screenshot dimension in CSS pixels. Explicit
/// bounds may still request up to the contract maximum; the byte budget
/// below governs either way.
const DEFAULT_CHROME_SCREENSHOT_DIMENSION: f64 = 1440.0;
/// Bounded number of capture attempts while fitting the shared transport's
/// per-image byte budget.
const SCREENSHOT_FIT_ATTEMPTS: usize = 4;
/// The cursor lives only for one action in a dedicated isolated world.
struct CursorDecoration {
    context: i64,
    frame: String,
    loader: String,
    id: String,
    width: f64,
    height: f64,
}

pub(super) const CHROME_CURSOR_SCRIPT: &str = r##"(payload => {
  const key = Symbol.for("io.brightwave.tidebreak.chrome.agent-cursor");
  const prior = globalThis[key];
  if (payload.clear) {
    if (prior && prior.id === payload.id) prior.clear();
    return "cleared";
  }
  if (String(location.href) !== payload.url || innerWidth !== payload.width
      || innerHeight !== payload.height || !Number.isFinite(payload.x)
      || !Number.isFinite(payload.y) || payload.x < 0 || payload.y < 0
      || payload.x >= innerWidth || payload.y >= innerHeight) return "stale";
  prior?.clear();
  if (!document.documentElement) return "unavailable";
  const host = document.createElement("div");
  host.setAttribute("aria-hidden", "true");
  host.setAttribute("data-tidebreak-ghost-cursor", "");
  host.style.cssText = "all:initial!important;position:fixed!important;inset:0!important;pointer-events:none!important;z-index:2147483647!important;contain:strict!important;user-select:none!important;";
  const shadow = host.attachShadow({mode:"closed"});
  const ns = "http://www.w3.org/2000/svg";
  const cursor = document.createElementNS(ns, "svg");
  cursor.setAttribute("viewBox", "0 0 24 24");
  cursor.setAttribute("width", "24");
  cursor.setAttribute("height", "24");
  cursor.style.cssText = "position:absolute;overflow:visible;pointer-events:none;";
  cursor.style.left = `${payload.x - 4}px`;
  cursor.style.top = `${payload.y - 3}px`;
  const ring = document.createElementNS(ns, "circle");
  for (const [name, value] of Object.entries({cx:"4",cy:"3",r:"8",fill:"none",stroke:"oklch(0.6 0.125 195)","stroke-width":"1.5",opacity:"0.55"})) ring.setAttribute(name, value);
  cursor.appendChild(ring);
  const outline = document.createElementNS(ns, "path");
  for (const [name, value] of Object.entries({d:"m4 3 7.07 17 2.51-7.39L21 10.07z",fill:"oklch(0.6 0.125 195)",stroke:"oklch(0.985 0.002 240)","stroke-width":"2","stroke-linejoin":"round"})) outline.setAttribute(name, value);
  cursor.appendChild(outline);
  shadow.appendChild(cursor);
  document.documentElement.appendChild(host);
  const state = {id:payload.id, timer:null, frame:null, clear:null};
  const events = ["pagehide", "popstate", "hashchange", "resize"];
  state.clear = () => {
    clearTimeout(state.timer);
    if (state.frame !== null) cancelAnimationFrame(state.frame);
    for (const event of events) removeEventListener(event, state.clear);
    host.remove();
    if (globalThis[key] === state) delete globalThis[key];
  };
  globalThis[key] = state;
  for (const event of events) addEventListener(event, state.clear, {once:true});
  const checkDocument = () => {
    if (String(location.href) !== payload.url || innerWidth !== payload.width
        || innerHeight !== payload.height) { state.clear(); return; }
    state.frame = requestAnimationFrame(checkDocument);
  };
  state.frame = requestAnimationFrame(checkDocument);
  // A lost debugger cannot run host cleanup. This expiry needs no connection.
  state.timer = setTimeout(state.clear, 1500);
  return "shown";
})"##;

fn cursor_clear_params(cursor: &CursorDecoration) -> Value {
    let payload = json!({"clear":true,"id":cursor.id});
    json!({
        "expression":format!("({CHROME_CURSOR_SCRIPT})({payload})"),
        "contextId":cursor.context,
        "returnByValue":true,
    })
}

/// Best-effort neutralizer for a pressed key or mouse button.
///
/// Armed before the down event is issued: once that command is queued, its
/// response can be cancelled — or the whole call future dropped by a caller's
/// `select!` — while Chrome has already applied the press. Dropping an armed
/// hold sends the bounded release commands straight on the transport.
/// Releasing held input restores the page to neutral rather than exercising
/// authority, so like the pre-existing drag cancel path it runs even after
/// the fence has ended. The transport's command queue keeps ordering sound: a
/// down that was discarded before dispatch never reaches Chrome after its
/// release, and a redundant release is harmless to Chrome.
struct InputHold {
    cdp: CdpSession,
    session: String,
    releases: Vec<(&'static str, Value)>,
    focus_sessions: Vec<String>,
    cursor: Option<CursorDecoration>,
    cleanup_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}
impl InputHold {
    fn new(access: &Access, tab: &Tab, cleanup_guard: tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self {
            cdp: access.cdp.clone(),
            session: tab.cdp_session.clone(),
            releases: Vec::new(),
            focus_sessions: Vec::new(),
            cursor: None,
            cleanup_guard: Some(cleanup_guard),
        }
    }
    /// Hidden Chrome tabs can acknowledge pointer input without delivering
    /// it. Page-level focus emulation keeps their input pipeline active; it
    /// never calls browser activation or requests native keyboard focus.
    async fn enable_page_focus(
        &mut self,
        access: &Access,
        snapshot: &Snapshot,
    ) -> Result<(), String> {
        for frame in &snapshot.frames {
            if !access.permits(&frame.url) || self.focus_sessions.contains(&frame.cdp_session) {
                continue;
            }
            // Arm restoration before the enable command can reach Chrome.
            self.focus_sessions.push(frame.cdp_session.clone());
            access
                .command(
                    Some(&frame.cdp_session),
                    "Emulation.setFocusEmulationEnabled",
                    json!({"enabled":true}),
                )
                .await?;
        }
        Ok(())
    }
    async fn show_cursor(
        &mut self,
        access: &Access,
        snapshot: &Snapshot,
        event: &Value,
    ) -> Result<(), String> {
        let x = event["x"]
            .as_f64()
            .filter(|x| x.is_finite())
            .ok_or("Chrome cursor position is invalid")?;
        let y = event["y"]
            .as_f64()
            .filter(|y| y.is_finite())
            .ok_or("Chrome cursor position is invalid")?;
        let root = snapshot
            .frames
            .first()
            .ok_or("Chrome root frame is unavailable")?;
        if root.cdp_session != self.session || !access.permits(&root.url) {
            return Err("Chrome cursor is outside the approved root frame".into());
        }
        if self.cursor.is_none() {
            let context = access.command(Some(&self.session), "Page.createIsolatedWorld", json!({
                "frameId":root.id,"worldName":"tidebreak-computer-use-cursor","grantUniveralAccess":false
            })).await?["executionContextId"].as_i64().ok_or("Chrome cursor world is unavailable")?;
            let viewport = access
                .eval(
                    &self.session,
                    Some(context),
                    "({width:innerWidth,height:innerHeight})".into(),
                )
                .await?;
            self.cursor = Some(CursorDecoration {
                context,
                frame: root.id.clone(),
                loader: root.loader.clone(),
                id: Uuid::new_v4().to_string(),
                width: viewport["width"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .ok_or("Chrome cursor viewport is unavailable")?,
                height: viewport["height"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0)
                    .ok_or("Chrome cursor viewport is unavailable")?,
            });
        }
        let cursor = self.cursor.as_ref().unwrap();
        if x < 0.0 || y < 0.0 || x >= cursor.width || y >= cursor.height {
            return Err("Chrome cursor position is outside its viewport".into());
        }
        let payload = json!({"id":cursor.id,"url":root.url,"x":x,"y":y,"width":cursor.width,"height":cursor.height});
        let shown = access
            .eval(
                &self.session,
                Some(cursor.context),
                format!("({CHROME_CURSOR_SCRIPT})({payload})"),
            )
            .await?;
        if shown != "shown" {
            return Err("Chrome cursor target changed; take a fresh snapshot".into());
        }
        Ok(())
    }

    fn arm(&mut self, method: &'static str, params: Value) {
        self.releases.push((method, params));
    }
    fn disarm(&mut self) {
        self.releases.clear();
    }
    async fn release(&mut self) -> Result<(), String> {
        let result = restore_page_input(
            &self.cdp,
            &self.session,
            &self.releases,
            &self.focus_sessions,
            self.cursor.as_ref(),
        )
        .await;
        self.releases.clear();
        self.focus_sessions.clear();
        self.cursor = None;
        result
    }
}

async fn restore_page_input(
    cdp: &CdpSession,
    session: &str,
    releases: &[(&'static str, Value)],
    focus_sessions: &[String],
    cursor: Option<&CursorDecoration>,
) -> Result<(), String> {
    let mut failed = false;
    for (method, params) in releases {
        let result = tokio::time::timeout(
            INPUT_RELEASE_TIMEOUT,
            cdp.command_in_session(session, method, params.clone()),
        )
        .await;
        failed |= !matches!(result, Ok(Ok(_)));
    }
    if let Some(cursor) = cursor {
        let cleared = tokio::time::timeout(
            INPUT_RELEASE_TIMEOUT,
            cdp.command_in_session(session, "Runtime.evaluate", cursor_clear_params(cursor)),
        )
        .await;
        let removed = matches!(cleared, Ok(Ok(ref value)) if value.get("exceptionDetails").is_none() && value["result"]["value"] == "cleared");
        // A completed click can replace the document before cleanup. Confirm
        // that the old document is gone before accepting a lost cursor world.
        let document_replaced = if removed {
            false
        } else {
            let current = tokio::time::timeout(
                INPUT_RELEASE_TIMEOUT,
                cdp.command_in_session(session, "Page.getFrameTree", json!({})),
            )
            .await;
            matches!(current, Ok(Ok(ref value)) if {
                let frame = &value["frameTree"]["frame"];
                let id = frame["id"].as_str().unwrap_or("");
                let loader = frame["loaderId"].as_str().unwrap_or("");
                !id.is_empty() && !loader.is_empty() && (id != cursor.frame || loader != cursor.loader)
            })
        };
        failed |= !removed && !document_replaced;
    }
    // Queue all resets before waiting. The total cleanup ceiling does not
    // grow with the page's number of out-of-process frame sessions.
    let resets = futures_util::future::join_all(focus_sessions.iter().map(|session| {
        cdp.command_in_session(
            session,
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled":false}),
        )
    }));
    failed |= match tokio::time::timeout(INPUT_RELEASE_TIMEOUT, resets).await {
        Ok(results) => results.iter().any(Result::is_err),
        Err(_) => true,
    };
    if failed {
        // Detaching the DevTools connection clears page emulation and refuses
        // new input if Chrome did not acknowledge its restoration.
        cdp.close();
        return Err("Chrome input cleanup failed. Reconnect before another action.".into());
    }
    Ok(())
}

impl Drop for InputHold {
    fn drop(&mut self) {
        if self.releases.is_empty() && self.focus_sessions.is_empty() && self.cursor.is_none() {
            return;
        }
        let cdp = self.cdp.clone();
        let session = std::mem::take(&mut self.session);
        let releases = std::mem::take(&mut self.releases);
        let focus_sessions = std::mem::take(&mut self.focus_sessions);
        let cursor = self.cursor.take();
        let cleanup_guard = self.cleanup_guard.take();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            cdp.close();
            return;
        };
        runtime.spawn(async move {
            let _ = restore_page_input(&cdp, &session, &releases, &focus_sessions, cursor.as_ref())
                .await;
            drop(cleanup_guard);
        });
    }
}

impl Access {
    async fn command(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.fence.check()?;
        let fence = self.fence.clone();
        let command =
            self.cdp
                .command_guarded(session, method, params, Arc::new(move || fence.live()));
        tokio::pin!(command);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let result = tokio::select! {
            result = tokio::time::timeout_at(deadline, &mut command) => {
                result.map_err(|_| format!("{method} timed out"))?.map_err(|error| error.0)
            },
            reason = async {
                tokio::select! {
                    _ = self.fence.connection.cancelled() => "Chrome connection revoked",
                    _ = self.fence.session.cancelled() => "Chrome session revoked",
                    _ = self.fence.call.cancelled() => "Chrome call cancelled",
                    _ = async {
                        while self.fence.live() {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    } => "Chrome control stopped",
                }
            } => {
                Err(reason.into())
            },
        };
        self.fence.check()?;
        result
    }
    async fn eval(
        &self,
        session: &str,
        context: Option<i64>,
        expression: String,
    ) -> Result<Value, String> {
        let mut params = json!({"expression":expression,"returnByValue":true,"awaitPromise":true});
        if let Some(context) = context {
            params["contextId"] = json!(context);
        }
        let result = self
            .command(Some(session), "Runtime.evaluate", params)
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("Chrome page evaluation failed; take a fresh snapshot".into());
        }
        result
            .get("result")
            .and_then(|v| v.get("value"))
            .cloned()
            .ok_or_else(|| "Chrome evaluation returned no value".into())
    }
    fn permits(&self, url: &str) -> bool {
        BrowserOrigin::from_url(url).is_some_and(|origin| self.grant.covers(&origin))
    }
    async fn frames(&self, session: &str) -> Result<Vec<Frame>, String> {
        fn walk(
            value: &Value,
            out: &mut Vec<Frame>,
            parent_url: &str,
            parent_id: Option<&str>,
            cdp_session: &str,
        ) -> Result<(), String> {
            if out.len() >= 128 {
                return Err("Chrome page has too many frames".into());
            }
            let frame = &value["frame"];
            let url = frame["url"].as_str().unwrap_or("");
            let url = if url == "about:blank" || url == "about:srcdoc" {
                parent_url
            } else {
                url
            };
            let id = frame["id"].as_str().unwrap_or("").to_owned();
            out.push(Frame {
                cdp_session: cdp_session.into(),
                id: id.clone(),
                loader: frame["loaderId"].as_str().unwrap_or("").into(),
                url: url.into(),
                name: frame["name"].as_str().unwrap_or("").into(),
                parent: parent_id
                    .or_else(|| frame["parentId"].as_str())
                    .map(str::to_owned),
            });
            if let Some(children) = value["childFrames"].as_array() {
                for child in children {
                    walk(child, out, url, Some(&id), cdp_session)?;
                }
            }
            Ok(())
        }
        let value = self
            .command(Some(session), "Page.getFrameTree", json!({}))
            .await?;
        let mut frames = Vec::new();
        walk(&value["frameTree"], &mut frames, "", None, session)?;
        if frames.first().is_none_or(|frame| !self.permits(&frame.url)) {
            return Err("Chrome page origin is outside the approved scope".into());
        }
        let mut seen = HashSet::from([session.to_owned()]);
        loop {
            let children = self
                .cdp
                .attached_sessions()
                .into_iter()
                .filter(|child| {
                    child
                        .parent_session
                        .as_ref()
                        .is_some_and(|parent| seen.contains(parent))
                        && !seen.contains(&child.session_id)
                })
                .collect::<Vec<_>>();
            if children.is_empty() {
                break;
            }
            for child in children {
                if seen.len() >= 128 {
                    return Err("Chrome page has too many frame sessions".into());
                }
                seen.insert(child.session_id.clone());
                self.command(
                    Some(&child.session_id),
                    "Target.setAutoAttach",
                    json!({"autoAttach":true,"waitForDebuggerOnStart":false,"flatten":true,"filter":[{"type":"iframe","exclude":false},{"exclude":true}]}),
                )
                .await?;
                let tree = self
                    .command(Some(&child.session_id), "Page.getFrameTree", json!({}))
                    .await?;
                let mut child_frames = Vec::new();
                walk(
                    &tree["frameTree"],
                    &mut child_frames,
                    "",
                    None,
                    &child.session_id,
                )?;
                for mut frame in child_frames {
                    if let Some(old) = frames.iter_mut().find(|old| old.id == frame.id) {
                        // The embedding tree already recorded who owns this
                        // frame; the child-session walk cannot know that.
                        if frame.parent.is_none() {
                            frame.parent = old.parent.clone();
                        }
                        *old = frame;
                    } else {
                        frames.push(frame);
                    }
                }
            }
        }
        frames[1..].sort_by(|a, b| a.id.cmp(&b.id));
        Ok(frames)
    }
}
impl ChromeComputerUseService {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn ownership(&self) -> &ChromeOwnership {
        &self.ownership
    }
    pub fn install_connection(
        &self,
        spec: ChromeConnectionSpec,
        cdp: CdpSession,
    ) -> Result<(), String> {
        let endpoint =
            url::Url::parse(&spec.websocket_endpoint).map_err(|_| "invalid Chrome endpoint")?;
        if endpoint.scheme() != "ws"
            || !matches!(
                endpoint.host_str(),
                Some("127.0.0.1" | "localhost" | "[::1]")
            )
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.port().is_none()
        {
            return Err("Chrome requires a host-derived loopback WebSocket endpoint".into());
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.connections.contains_key(&spec.connection_id)
            || inner
                .connections
                .values()
                .any(|c| c.spec.owner == spec.owner && c.spec.workspace == spec.workspace)
        {
            return Err(
                "An approved Chrome connection already exists for this owner and workspace".into(),
            );
        }
        inner.connections.insert(
            spec.connection_id.clone(),
            Connection {
                spec,
                cdp,
                cancel: CancelToken::new(),
            },
        );
        Ok(())
    }
    pub fn uninstall_connection(&self, id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(connection) = inner.connections.remove(id) {
            connection.cancel.cancel();
            connection.cdp.close();
        }
        inner.tabs.retain(|_, tab| tab.connection != id);
        Ok(())
    }
    pub fn revoke_session(&self, id: &SessionId) {
        let mut inner = self.inner.lock().unwrap();
        inner.revoked.insert(*id);
        if let Some(state) = inner.sessions.remove(id) {
            state.cancel.cancel();
        }
        inner.tabs.retain(|_, tab| &tab.session != id);
        inner.receipts.retain(|(session, _), _| session != id);
    }
    pub fn state(&self, scope: &ChromeScope) -> ChromeAdapterState {
        let inner = self.inner.lock().unwrap();
        let connection = inner
            .connections
            .values()
            .find(|c| c.spec.owner == scope.owner && c.spec.workspace == scope.workspace);
        ChromeAdapterState {
            available: connection.is_some_and(|connection| connection.cdp.is_connected())
                && !inner.revoked.contains(&scope.session)
                && !scope.cancel.is_cancelled()
                && !self.ownership.is_tripped(),
            connection_id: connection.map(|c| c.spec.connection_id.clone()),
            connection_label: connection.map(|c| c.spec.endpoint_label.clone()),
            grant: connection.map(|c| match c.spec.grant {
                ChromeConnectionGrant::Origin(_) => "origin".into(),
                ChromeConnectionGrant::DeveloperAllSites => "developer_all_sites".into(),
            }),
            managed_isolated: connection.is_some_and(|c| c.spec.managed_isolated),
            approved_existing_profile: connection.is_some_and(|c| !c.spec.managed_isolated),
            tab_count: inner
                .tabs
                .values()
                .filter(|t| {
                    t.owner == scope.owner
                        && t.workspace == scope.workspace
                        && t.session == scope.session
                })
                .count(),
            last_error: self
                .ownership
                .is_tripped()
                .then(|| "Chrome control is paused".into()),
        }
    }
    fn access(&self, scope: &ChromeScope, connection: Option<&str>) -> Result<Access, String> {
        let mut inner = self.inner.lock().unwrap();
        if inner.revoked.contains(&scope.session) {
            return Err("Chrome session is revoked".into());
        }
        let c = inner
            .connections
            .values()
            .find(|c| {
                c.spec.owner == scope.owner
                    && c.spec.workspace == scope.workspace
                    && connection.is_none_or(|id| c.spec.connection_id == id)
            })
            .ok_or("No approved Chrome connection for this owner and workspace")?;
        if !c.cdp.is_connected() {
            return Err("Chrome connection is closed; reconnect Chrome".into());
        }
        let cdp = c.cdp.clone();
        let grant = c.spec.grant.clone();
        let connection_id = c.spec.connection_id.clone();
        let connection_cancel = c.cancel.clone();
        let session = inner
            .sessions
            .entry(scope.session)
            .or_insert_with(|| SessionState {
                owner: scope.owner.clone(),
                workspace: scope.workspace,
                cancel: CancelToken::new(),
            });
        if session.owner != scope.owner || session.workspace != scope.workspace {
            return Err("Chrome session belongs to another owner or workspace".into());
        }
        let cancel = session.cancel.clone();
        let fence = Fence {
            connection: connection_cancel,
            session: cancel,
            call: scope.cancel.clone(),
            ownership: self.ownership.clone(),
            generation: self.ownership.generation.load(Ordering::SeqCst),
        };
        fence.check()?;
        Ok(Access {
            cdp,
            grant,
            connection: connection_id,
            fence,
        })
    }
    fn tab(&self, scope: &ChromeScope, reference: &str) -> Result<(Access, Tab), String> {
        let tab = self
            .inner
            .lock()
            .unwrap()
            .tabs
            .get(reference)
            .cloned()
            .ok_or("Unknown or revoked Chrome tab")?;
        if tab.owner != scope.owner
            || tab.workspace != scope.workspace
            || tab.session != scope.session
        {
            return Err("Chrome tab belongs to another owner, workspace, or session".into());
        }
        Ok((self.access(scope, Some(&tab.connection))?, tab))
    }
    pub async fn discover_tabs(&self, id: &str) -> Result<Vec<ChromeDiscoveredTab>, String> {
        let cdp = self
            .inner
            .lock()
            .unwrap()
            .connections
            .get(id)
            .map(|c| c.cdp.clone())
            .ok_or("Chrome connection is gone")?;
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            cdp.command("Target.getTargets", json!({})),
        )
        .await
        .map_err(|_| "Chrome discovery timed out")?
        .map_err(|e| e.0)?;
        Ok(result["targetInfos"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == "page")
            .take(128)
            .filter_map(|t| {
                Some(ChromeDiscoveredTab {
                    target_id: t["targetId"].as_str()?.into(),
                    title: t["title"].as_str().unwrap_or("").into(),
                    url: t["url"].as_str().unwrap_or("").into(),
                })
            })
            .collect())
    }
    pub async fn attach_existing_tab(
        &self,
        scope: &ChromeScope,
        id: &str,
        target: &str,
    ) -> Result<ChromeTabSummary, String> {
        let _serial = self.serial.lock().await;
        let access = self.access(scope, Some(id))?;
        self.attach(scope, &access, target).await
    }
    async fn attach(
        &self,
        scope: &ChromeScope,
        access: &Access,
        target: &str,
    ) -> Result<ChromeTabSummary, String> {
        if self
            .inner
            .lock()
            .unwrap()
            .tabs
            .values()
            .any(|t| t.connection == access.connection && t.target_id == target)
        {
            return Err("Chrome tab is already controlled by a session".into());
        }
        let info = access
            .command(None, "Target.getTargetInfo", json!({"targetId":target}))
            .await?;
        if info["targetInfo"]["type"] != "page"
            || !access.permits(info["targetInfo"]["url"].as_str().unwrap_or(""))
        {
            return Err("Chrome tab origin is outside the approved scope".into());
        }
        let result = access
            .command(
                None,
                "Target.attachToTarget",
                json!({"targetId":target,"flatten":true}),
            )
            .await?;
        let session = result["sessionId"]
            .as_str()
            .ok_or("Chrome attach returned no session")?
            .to_owned();
        for method in ["Page.enable", "Runtime.enable", "Network.enable"] {
            access.command(Some(&session), method, json!({})).await?;
        }
        access
            .command(
                Some(&session),
                "Target.setAutoAttach",
                json!({"autoAttach":true,"waitForDebuggerOnStart":false,"flatten":true,"filter":[{"type":"iframe","exclude":false},{"exclude":true}]}),
            )
            .await?;
        let frames = access.frames(&session).await?;
        let frame = &frames[0];
        let summary = ChromeTabSummary {
            target_ref: format!("ct-{}", Uuid::new_v4().simple()),
            title: info["targetInfo"]["title"].as_str().unwrap_or("").into(),
            url: frame.url.clone(),
            load_state: BrowserLoadState::Ready,
            active: false,
        };
        access.fence.check()?;
        self.inner.lock().unwrap().tabs.insert(
            summary.target_ref.clone(),
            Tab {
                connection: access.connection.clone(),
                owner: scope.owner.clone(),
                workspace: scope.workspace,
                session: scope.session,
                target_id: target.into(),
                cdp_session: session,
                summary: summary.clone(),
                document: frame.loader.clone(),
                epoch: 1,
                snapshot: None,
            },
        );
        Ok(summary)
    }
    fn refresh(&self, tab: &mut Tab, frames: &[Frame]) -> Result<(), String> {
        let frame = frames.first().ok_or("Chrome page has no frame")?;
        if tab.document != frame.loader || tab.summary.url != frame.url {
            tab.epoch += 1;
            tab.document = frame.loader.clone();
            tab.snapshot = None;
            tab.summary.url = frame.url.clone();
        }
        let mut inner = self.inner.lock().unwrap();
        let stored = inner
            .tabs
            .get_mut(&tab.summary.target_ref)
            .ok_or("Chrome tab was revoked")?;
        *stored = tab.clone();
        Ok(())
    }
    pub async fn dispatch(&self, scope: &ChromeScope, call: &ComputerUseCall) -> ChromeCallOutcome {
        if call.name == CHROME_ACTIVATE_TAB_TOOL {
            let mut rejected = outcome(
                call,
                ComputerUseOutcome::Rejected,
                "Chrome tab activation is unavailable because it changes your active tab. Use independent actions on the target tab; do not retry through foreground control.",
                Value::Null,
            );
            rejected.result.error_code = Some("independent_input_unavailable".into());
            return rejected;
        }
        let _serial = self.serial.lock().await;
        if !validate_chrome_computer_use_arguments(&call.name, &call.arguments) {
            return outcome(
                call,
                ComputerUseOutcome::Rejected,
                "Invalid Chrome tool arguments",
                Value::Null,
            );
        }
        if let Err(error) = self.access(scope, None) {
            return outcome(call, ComputerUseOutcome::Rejected, &error, Value::Null);
        }
        // A dropped action can still own asynchronous cleanup after releasing
        // the serial gate. Drain it before snapshots, captures, or navigation.
        let _prior_input_cleanup = if call.name == CHROME_ACT_TOOL {
            None
        } else {
            Some(self.input_cleanup.lock().await)
        };
        let mutating = matches!(
            call.name.as_str(),
            CHROME_NEW_TAB_TOOL | CHROME_CLOSE_TAB_TOOL | CHROME_NAVIGATE_TOOL | CHROME_ACT_TOOL
        );
        if mutating {
            let mut inner = self.inner.lock().unwrap();
            let key = (scope.session, call.request_id);
            if let Some(receipt) = inner.receipts.get(&key) {
                return if receipt.call == *call {
                    ChromeCallOutcome {
                        result: receipt.result.clone(),
                    }
                } else {
                    outcome(
                        call,
                        ComputerUseOutcome::Rejected,
                        "Request id was already used for a different call",
                        Value::Null,
                    )
                };
            }
            if inner
                .receipts
                .keys()
                .filter(|(id, _)| id == &scope.session)
                .count()
                >= 2048
            {
                return outcome(
                    call,
                    ComputerUseOutcome::Rejected,
                    "Chrome session operation limit reached; start a new session",
                    Value::Null,
                );
            }
            inner.receipts.insert(
                key,
                Receipt {
                    call: call.clone(),
                    result: outcome(
                        call,
                        ComputerUseOutcome::Unknown,
                        "Operation may have started; inspect before issuing a new action",
                        Value::Null,
                    )
                    .result,
                },
            );
        }
        let result = self.execute(scope, call).await;
        let mut response = match result {
            Ok((data, images)) => {
                let mut result = outcome(
                    call,
                    ComputerUseOutcome::Completed,
                    "Chrome operation completed",
                    data,
                );
                result.result.images = images;
                result
            }
            Err(error) => outcome(
                call,
                if mutating {
                    ComputerUseOutcome::Unknown
                } else {
                    ComputerUseOutcome::Rejected
                },
                &error,
                Value::Null,
            ),
        };
        if self.access(scope, None).is_err() {
            response = outcome(
                call,
                if mutating {
                    ComputerUseOutcome::Unknown
                } else {
                    ComputerUseOutcome::Rejected
                },
                "Chrome authority ended before result delivery",
                Value::Null,
            );
        }
        if mutating {
            if let Some(receipt) = self
                .inner
                .lock()
                .unwrap()
                .receipts
                .get_mut(&(scope.session, call.request_id))
            {
                receipt.result = response.result.clone();
            }
        }
        response
    }
    async fn execute(
        &self,
        scope: &ChromeScope,
        call: &ComputerUseCall,
    ) -> Result<(Value, Vec<ComputerUseImage>), String> {
        match call.name.as_str() {
            CHROME_LIST_TABS_TOOL => {
                let refs = self
                    .inner
                    .lock()
                    .unwrap()
                    .tabs
                    .values()
                    .filter(|t| {
                        t.owner == scope.owner
                            && t.workspace == scope.workspace
                            && t.session == scope.session
                    })
                    .map(|t| t.summary.target_ref.clone())
                    .collect::<Vec<_>>();
                let mut tabs = Vec::new();
                for reference in refs {
                    let (access, mut tab) = self.tab(scope, &reference)?;
                    if let Ok(frames) = access.frames(&tab.cdp_session).await {
                        self.refresh(&mut tab, &frames)?;
                        tabs.push(tab.summary)
                    }
                }
                data(ChromeListTabsResult { tabs })
            }
            CHROME_NEW_TAB_TOOL => {
                let args: ChromeNewTabArgs = parse(call)?;
                let access = self.access(scope, None)?;
                if !access.permits(&args.url) {
                    return Err("Chrome URL is outside the approved scope".into());
                }
                let result = access
                    .command(
                        None,
                        "Target.createTarget",
                        json!({"url":args.url,"background":true}),
                    )
                    .await?;
                let target = result["targetId"]
                    .as_str()
                    .ok_or("Chrome returned no target")?;
                match self.attach(scope, &access, target).await {
                    Ok(summary) => data(summary),
                    Err(error) => {
                        let _ = tokio::time::timeout(
                            Duration::from_secs(3),
                            access
                                .cdp
                                .command("Target.closeTarget", json!({"targetId":target})),
                        )
                        .await;
                        Err(error)
                    }
                }
            }
            CHROME_CLOSE_TAB_TOOL => {
                let args: ChromeTabRefArgs = parse(call)?;
                let (access, mut tab) = self.tab(scope, &args.target_ref)?;
                let frames = access.frames(&tab.cdp_session).await?;
                self.refresh(&mut tab, &frames)?;
                access
                    .command(
                        None,
                        "Target.closeTarget",
                        json!({"targetId":tab.target_id}),
                    )
                    .await?;
                self.inner.lock().unwrap().tabs.remove(&args.target_ref);
                tab.summary.active = false;
                tab.summary.load_state = BrowserLoadState::Idle;
                data(tab.summary)
            }
            CHROME_NAVIGATE_TOOL => {
                let args: ChromeNavigateArgs = parse(call)?;
                let (access, mut tab) = self.tab(scope, &args.target_ref)?;
                if !access.permits(&args.url) {
                    return Err("Chrome URL is outside the approved scope".into());
                }
                access.frames(&tab.cdp_session).await?;
                let result = access
                    .command(
                        Some(&tab.cdp_session),
                        "Page.navigate",
                        json!({"url":args.url}),
                    )
                    .await?;
                if result.get("errorText").is_some() {
                    return Err(format!("Chrome navigation failed: {}", result["errorText"]));
                }
                tab.snapshot = None;
                self.inner
                    .lock()
                    .unwrap()
                    .tabs
                    .insert(args.target_ref.clone(), tab.clone());
                let deadline = tokio::time::Instant::now()
                    + Duration::from_millis(
                        args.timeout_ms
                            .unwrap_or(15000)
                            .min(MAX_CHROME_WAIT_TIMEOUT_MS),
                    );
                let mut ready = false;
                loop {
                    let frames = access.frames(&tab.cdp_session).await?;
                    self.refresh(&mut tab, &frames)?;
                    let state = access
                        .eval(&tab.cdp_session, None, "document.readyState".into())
                        .await;
                    if state.as_ref().is_ok_and(|v| v == "complete") {
                        ready = true;
                        break;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                data(ChromeNavigateResult {
                    target_ref: args.target_ref,
                    url: tab.summary.url,
                    load_state: if ready {
                        BrowserLoadState::Ready
                    } else {
                        BrowserLoadState::Loading
                    },
                    document_epoch: tab.epoch,
                })
            }
            CHROME_SNAPSHOT_TOOL => data(
                self.snapshot(scope, &parse::<ChromeSnapshotArgs>(call)?)
                    .await?,
            ),
            CHROME_SCREENSHOT_TOOL => {
                self.screenshot(scope, &parse::<ChromeScreenshotArgs>(call)?)
                    .await
            }
            CHROME_ACT_TOOL => data(self.act(scope, &parse::<ChromeActArgs>(call)?).await?),
            CHROME_WAIT_TOOL => data(self.wait(scope, &parse::<ChromeWaitArgs>(call)?).await?),
            CHROME_DIAGNOSTICS_TOOL => data(
                self.diagnostics(scope, &parse::<ChromeDiagnosticsArgs>(call)?)
                    .await?,
            ),
            _ => Err("Unknown Chrome tool".into()),
        }
    }
    async fn snapshot(
        &self,
        scope: &ChromeScope,
        args: &ChromeSnapshotArgs,
    ) -> Result<ChromePageSnapshot, String> {
        let (access, mut tab) = self.tab(scope, &args.target_ref)?;
        let before = access.frames(&tab.cdp_session).await?;
        self.refresh(&mut tab, &before)?;
        let snapshot_id = format!("snap-{}", Uuid::new_v4().simple());
        let mut nodes = Vec::new();
        let mut references = HashMap::new();
        let mut frames = Vec::new();
        let mut truncated = false;
        for (index, frame) in before.iter().enumerate() {
            if !access.permits(&frame.url) {
                frames.push(ChromeSemanticFrame {
                    name: frame.name.clone(),
                    url: frame.url.clone(),
                    status: ChromeFrameStatus::UnsupportedFrame,
                });
                continue;
            }
            let context=access.command(Some(&frame.cdp_session),"Page.createIsolatedWorld",json!({"frameId":frame.id,"worldName":"tidebreak-computer-use","grantUniveralAccess":false})).await;
            let Ok(context) = context else {
                frames.push(ChromeSemanticFrame {
                    name: frame.name.clone(),
                    url: frame.url.clone(),
                    status: ChromeFrameStatus::UnsupportedFrame,
                });
                continue;
            };
            let context = context["executionContextId"]
                .as_i64()
                .ok_or("Chrome isolated world returned no context")?;
            let options = json!({"max":args.bounded_max_nodes().saturating_sub(nodes.len()),"prefix":format!("n-{index}"),"snapshot":snapshot_id,"frame":frame.id});
            let result = access
                .eval(
                    &frame.cdp_session,
                    Some(context),
                    format!("({SNAPSHOT_SCRIPT})({options})"),
                )
                .await?;
            truncated |= result["truncated"].as_bool().unwrap_or(false);
            let projected: Vec<ChromeSemanticNode> =
                serde_json::from_value(result["nodes"].clone())
                    .map_err(|e| format!("Chrome snapshot is invalid: {e}"))?;
            for node in projected {
                if let Some(reference) = &node.target_ref {
                    references.insert(
                        reference.clone(),
                        (context, frame.id.clone(), frame.cdp_session.clone()),
                    );
                }
                nodes.push(node);
            }
            frames.push(ChromeSemanticFrame {
                name: frame.name.clone(),
                url: frame.url.clone(),
                status: if index == 0
                    || BrowserOrigin::from_url(&frame.url)
                        == BrowserOrigin::from_url(&before[0].url)
                {
                    ChromeFrameStatus::SameOrigin
                } else {
                    ChromeFrameStatus::CrossOrigin
                },
            });
        }
        let after = access.frames(&tab.cdp_session).await?;
        ensure_same_document(&before, &after)?;
        let info = access
            .eval(
                &tab.cdp_session,
                None,
                "({title:document.title,width:innerWidth,height:innerHeight,scrollX,scrollY})"
                    .into(),
            )
            .await?;
        tab.summary.title = info["title"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(512)
            .collect();
        tab.snapshot = Some(Snapshot {
            id: snapshot_id.clone(),
            epoch: tab.epoch,
            frames: after,
            nodes: references,
        });
        access.fence.check()?;
        self.inner
            .lock()
            .unwrap()
            .tabs
            .insert(args.target_ref.clone(), tab.clone());
        Ok(ChromePageSnapshot {
            target_ref: args.target_ref.clone(),
            snapshot_id,
            document_epoch: tab.epoch,
            content_trust: ChromeContentTrust::UntrustedPage,
            url: tab.summary.url,
            title: tab.summary.title,
            viewport: ChromeViewport {
                width: info["width"].as_f64().unwrap_or(0.0),
                height: info["height"].as_f64().unwrap_or(0.0),
                scroll_x: info["scrollX"].as_f64().unwrap_or(0.0),
                scroll_y: info["scrollY"].as_f64().unwrap_or(0.0),
            },
            nodes,
            frames,
            truncated,
        })
    }
    async fn checked_snapshot(
        &self,
        access: &Access,
        tab: &Tab,
        id: &str,
        epoch: u64,
    ) -> Result<Snapshot, String> {
        let snapshot = tab
            .snapshot
            .clone()
            .filter(|s| s.id == id && s.epoch == epoch)
            .ok_or("Chrome snapshot is stale; take a fresh snapshot")?;
        let frames = access.frames(&tab.cdp_session).await?;
        ensure_same_document(&snapshot.frames, &frames)?;
        Ok(snapshot)
    }
    async fn screenshot(
        &self,
        scope: &ChromeScope,
        args: &ChromeScreenshotArgs,
    ) -> Result<(Value, Vec<ComputerUseImage>), String> {
        let (access, tab) = self.tab(scope, &args.target_ref)?;
        let snapshot = self
            .checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
            .await?;
        if snapshot.frames.iter().any(|f| !access.permits(&f.url)) {
            return Err("Screenshot includes a frame outside the approved Chrome scope".into());
        }
        let metrics = access
            .command(Some(&tab.cdp_session), "Page.getLayoutMetrics", json!({}))
            .await?;
        let viewport = &metrics["cssVisualViewport"];
        let width = viewport["clientWidth"].as_f64().unwrap_or(0.0);
        let height = viewport["clientHeight"].as_f64().unwrap_or(0.0);
        if width <= 0.0 || height <= 0.0 {
            return Err("Chrome viewport is unavailable".into());
        }
        let mut scale = (args
            .max_width
            .map(|value| value as f64)
            .unwrap_or(DEFAULT_CHROME_SCREENSHOT_DIMENSION)
            / width)
            .min(
                args.max_height
                    .filter(|value| *value > 0)
                    .map(|value| value as f64)
                    .unwrap_or(DEFAULT_CHROME_SCREENSHOT_DIMENSION)
                    / height,
            )
            .min(1.0);
        // The shared transport carries at most one 1 MiB image inside one
        // 2 MiB frame. Fit here in the producer with a bounded number of
        // re-captures at a smaller clip scale; the native wrapper still
        // enforces the same ceiling as defense in depth.
        use base64::Engine;
        for _ in 0..SCREENSHOT_FIT_ATTEMPTS {
            let image=access.command(Some(&tab.cdp_session),"Page.captureScreenshot",json!({"format":"png","captureBeyondViewport":false,"clip":{"x":viewport["pageX"].as_f64().unwrap_or(0.0),"y":viewport["pageY"].as_f64().unwrap_or(0.0),"width":width,"height":height,"scale":scale}})).await?;
            self.checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
                .await?;
            let encoded = image["data"]
                .as_str()
                .ok_or("Chrome screenshot has no image")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "Chrome screenshot has invalid encoding")?;
            if bytes.len() > MAX_CHROME_SCREENSHOT_PNG_BYTES
                || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
            {
                return Err("Chrome screenshot is invalid or exceeds its size limit".into());
            }
            if bytes.len() <= MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES
                && encoded.len() <= MAX_BROWSER_SCREENSHOT_FRAME_BYTES
            {
                let (delivered_width, delivered_height) = ::image::ImageReader::with_format(
                    std::io::Cursor::new(&bytes),
                    ::image::ImageFormat::Png,
                )
                .into_dimensions()
                .map_err(|_| "Chrome screenshot has invalid PNG dimensions")?;
                if delivered_width == 0 || delivered_height == 0 {
                    return Err("Chrome screenshot has empty dimensions".into());
                }
                return Ok((
                    json!({"targetRef":args.target_ref,"snapshotId":args.snapshot_id,"documentEpoch":args.document_epoch,"mimeType":"image/png","width":delivered_width,"height":delivered_height}),
                    vec![ComputerUseImage {
                        mime_type: "image/png".into(),
                        base64: encoded.into(),
                    }],
                ));
            }
            // PNG size tracks pixel area, so shrink toward the byte budget
            // with a margin; the floor keeps one attempt from collapsing.
            let ratio =
                (MAX_BROWSER_SCREENSHOT_IMAGE_BLOCK_BYTES as f64 / bytes.len() as f64).sqrt();
            scale = (scale * ratio * 0.9).max(scale * 0.25);
            if width * scale < 16.0 || height * scale < 16.0 {
                break;
            }
        }
        Err("Chrome screenshot could not be reduced to the transport image budget".into())
    }
    async fn probe(
        &self,
        access: &Access,
        tab: &Tab,
        snapshot: &Snapshot,
        reference: &str,
        mut options: Value,
    ) -> Result<(f64, f64), String> {
        let (context, frame, frame_session) = snapshot
            .nodes
            .get(reference)
            .ok_or("Chrome node ref is absent from this snapshot")?;
        self.checked_snapshot(access, tab, &snapshot.id, snapshot.epoch)
            .await?;
        let offsets = frame_offset_chain(snapshot, frame)?;
        options["snapshot"] = json!(snapshot.id);
        options["ref"] = json!(reference);
        let result = access
            .eval(
                frame_session,
                Some(*context),
                format!("({PROBE_SCRIPT})({options})"),
            )
            .await?;
        if result["ok"] != true {
            return Err(format!(
                "Chrome target refused: {}",
                result["reason"].as_str().unwrap_or("stale_target")
            ));
        }
        let mut x = result["x"]
            .as_f64()
            .ok_or("Chrome target has no x coordinate")?;
        let mut y = result["y"]
            .as_f64()
            .ok_or("Chrome target has no y coordinate")?;
        // Each owner quad maps its child viewport into the queried CDP
        // session's root viewport. Same-session ancestors are already part
        // of that quad; map again only at a session boundary.
        let mut viewport = result["viewport"].clone();
        for (index, (owner_frame, owner_session)) in offsets.iter().enumerate() {
            let owner = access
                .command(
                    Some(owner_session),
                    "DOM.getFrameOwner",
                    json!({"frameId":owner_frame}),
                )
                .await?;
            let bounds = access
                .command(
                    Some(owner_session),
                    "DOM.getBoxModel",
                    json!({"backendNodeId":owner["backendNodeId"]}),
                )
                .await?;
            (x, y) = map_frame_point(&bounds["model"]["content"], &viewport, x, y)?;
            if index + 1 < offsets.len() {
                viewport = access
                    .eval(
                        owner_session,
                        None,
                        "({width:innerWidth,height:innerHeight})".into(),
                    )
                    .await?;
            }
        }
        Ok((x, y))
    }
    async fn mouse(
        &self,
        access: &Access,
        tab: &Tab,
        snapshot: &Snapshot,
        hold: &mut InputHold,
        event: Value,
    ) -> Result<(), String> {
        self.checked_snapshot(access, tab, &snapshot.id, snapshot.epoch)
            .await?;
        hold.show_cursor(access, snapshot, &event).await?;
        self.checked_snapshot(access, tab, &snapshot.id, snapshot.epoch)
            .await?;
        access
            .command(Some(&tab.cdp_session), "Input.dispatchMouseEvent", event)
            .await?;
        Ok(())
    }
    async fn act(
        &self,
        scope: &ChromeScope,
        args: &ChromeActArgs,
    ) -> Result<ChromeActResult, String> {
        let (access, tab) = self.tab(scope, &args.target_ref)?;
        let snapshot = self
            .checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
            .await?;
        // A previous snapshot cannot authorize a second action after any
        // attempt. Consume it before the first side effect so cancellation or
        // a dropped call future cannot leave the authority behind.
        if let Some(stored) = self.inner.lock().unwrap().tabs.get_mut(&args.target_ref) {
            stored.snapshot = None;
        }
        let cleanup_guard = self.input_cleanup.clone().lock_owned().await;
        access.fence.check()?;
        let mut hold = InputHold::new(&access, &tab, cleanup_guard);
        let operation=async {
   hold.enable_page_focus(&access, &snapshot).await?;
   match &args.action {
    ChromeAction::Fill{value}|ChromeAction::Select{value}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true,"operation":args.action.kind(),"value":value})).await?;}
    ChromeAction::Check{checked}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"operation":"check","value":checked})).await?;}
    ChromeAction::Type{text}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true})).await?;self.checked_snapshot(&access,&tab,&args.snapshot_id,args.document_epoch).await?;access.command(Some(&tab.cdp_session),"Input.insertText",json!({"text":text})).await?;}
    ChromeAction::Press{key}=>{
     self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true})).await?;
     let(key,modifiers,code)=key_chord(key)?;
     self.checked_snapshot(&access,&tab,&args.snapshot_id,args.document_epoch).await?;
     hold.arm("Input.dispatchKeyEvent",json!({"type":"keyUp","key":key,"modifiers":modifiers,"windowsVirtualKeyCode":code}));
     access.command(Some(&tab.cdp_session),"Input.dispatchKeyEvent",json!({"type":"keyDown","key":key,"modifiers":modifiers,"windowsVirtualKeyCode":code})).await?;
     access.command(Some(&tab.cdp_session),"Input.dispatchKeyEvent",json!({"type":"keyUp","key":key,"modifiers":modifiers,"windowsVirtualKeyCode":code})).await?;
     hold.disarm();
    }
    ChromeAction::Drag{to_ref}=>{
     self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true})).await?;
     let(dx,dy)=self.probe(&access,&tab,&snapshot,to_ref,json!({"scroll":false})).await?;
     let(x,y)=self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":false})).await?;
     self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mouseMoved","x":x,"y":y})).await?;
     // Cleanup releases back at the origin so it can never complete the drop.
     hold.arm("Input.cancelDragging",json!({}));
     hold.arm("Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":1}));
     self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await?;
     for step in 1..=12 {let t=f64::from(step)/12.0;self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mouseMoved","x":x+(dx-x)*t,"y":y+(dy-y)*t,"button":"left","buttons":1})).await?;tokio::time::sleep(Duration::from_millis(16)).await;}
     self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mouseReleased","x":dx,"y":dy,"button":"left","buttons":0,"clickCount":1})).await?;
     hold.disarm();
    }
    ChromeAction::Click|ChromeAction::DoubleClick|ChromeAction::Hover|ChromeAction::Scroll{..}=>{
     let(x,y)=self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true})).await?;
     self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mouseMoved","x":x,"y":y})).await?;
     match &args.action {
      ChromeAction::Hover=>{},
      ChromeAction::Scroll{x:dx,y:dy}=>{self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mouseWheel","x":x,"y":y,"deltaX":dx.unwrap_or(0.0),"deltaY":dy.unwrap_or(0.0)})).await?;},
      _=>{let count=if args.action==ChromeAction::DoubleClick{2}else{1};for click in 1..=count{
       self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":false})).await?;
       hold.arm("Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":click}));
       self.mouse(&access,&tab,&snapshot,&mut hold,json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":click})).await?;
       access.command(Some(&tab.cdp_session),"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":click})).await?;
       hold.disarm();
      }}
     }
    }
   }
   Ok::<(),String>(())
  }.await;
        // Keep the service serial gate until every compensating release has
        // finished. Drop is only a fallback for an unexpectedly aborted task.
        hold.release().await?;
        operation?;
        Ok(ChromeActResult {
            target_ref: args.target_ref.clone(),
            snapshot_id: args.snapshot_id.clone(),
            document_epoch: args.document_epoch,
            node_ref: args.node_ref.clone(),
            action: args.action.kind().into(),
            status: ChromeActStatus::Ok,
            message: "Action completed; take a fresh snapshot".into(),
            requires_resnapshot: true,
            url: None,
            title: None,
        })
    }
    async fn wait(
        &self,
        scope: &ChromeScope,
        args: &ChromeWaitArgs,
    ) -> Result<ChromeWaitResult, String> {
        let (access, mut tab) = self.tab(scope, &args.target_ref)?;
        self.checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
            .await?;
        let start = tab.summary.url.clone();
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(args.bounded_timeout_ms());
        loop {
            let frames = access.frames(&tab.cdp_session).await?;
            self.refresh(&mut tab, &frames)?;
            let expression = match &args.condition {
                ChromeWaitCondition::TextPresent { text } => format!(
                    "Boolean(document.body?.innerText.includes({}))",
                    json!(text)
                ),
                ChromeWaitCondition::TextAbsent { text } => format!(
                    "!Boolean(document.body?.innerText.includes({}))",
                    json!(text)
                ),
                ChromeWaitCondition::UrlChanged => format!("location.href!=={}", json!(start)),
                ChromeWaitCondition::LoadState { state } => match state {
                    BrowserLoadState::Ready => "document.readyState==='complete'".into(),
                    BrowserLoadState::Loading => "document.readyState==='loading'".into(),
                    _ => "false".into(),
                },
            };
            let matched = access.eval(&tab.cdp_session, None, expression).await? == true;
            access.frames(&tab.cdp_session).await?;
            if matched || tokio::time::Instant::now() >= deadline {
                return Ok(ChromeWaitResult {
                    target_ref: args.target_ref.clone(),
                    status: if matched {
                        ChromeWaitStatus::Resolved
                    } else {
                        ChromeWaitStatus::TimedOut
                    },
                    message: if matched {
                        "Condition satisfied".into()
                    } else {
                        "Condition did not match before timeout".into()
                    },
                    document_epoch: tab.epoch,
                    url: Some(tab.summary.url),
                    title: None,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn diagnostics(
        &self,
        scope: &ChromeScope,
        args: &ChromeDiagnosticsArgs,
    ) -> Result<ChromeDiagnosticsResult, String> {
        let (access, mut tab) = self.tab(scope, &args.target_ref)?;
        self.checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
            .await?;
        let frames = access.frames(&tab.cdp_session).await?;
        self.refresh(&mut tab, &frames)?;
        // A narrow page grant cannot attribute console messages from other frames.
        if !matches!(access.grant, ChromeConnectionGrant::DeveloperAllSites) {
            return Err("Chrome diagnostics require the explicit developer grant".into());
        }
        let mut console = Vec::new();
        let mut network: HashMap<String, ChromeNetworkEntry> = HashMap::new();
        for event in access.cdp.recent_events() {
            match event {
                CdpEvent::Console {
                    session_id,
                    level,
                    text,
                    url,
                    line,
                    column,
                    timestamp_ms,
                } if session_id.as_deref() == Some(&tab.cdp_session) => {
                    console.push(ChromeConsoleEntry {
                        level,
                        text,
                        url,
                        line,
                        column,
                        timestamp_ms,
                    })
                }
                CdpEvent::Exception {
                    session_id,
                    text,
                    url,
                    line,
                    column,
                    timestamp_ms,
                } if session_id.as_deref() == Some(&tab.cdp_session) => {
                    console.push(ChromeConsoleEntry {
                        level: "error".into(),
                        text,
                        url,
                        line,
                        column,
                        timestamp_ms,
                    })
                }
                CdpEvent::RequestWillBeSent {
                    session_id,
                    request_id,
                    method,
                    url,
                    resource_type,
                    timestamp_ms,
                } if session_id.as_deref() == Some(&tab.cdp_session) => {
                    network.insert(
                        request_id.clone(),
                        ChromeNetworkEntry {
                            kind: "request".into(),
                            method,
                            url,
                            status: None,
                            error_text: String::new(),
                            request_id,
                            resource_type,
                            mime_type: String::new(),
                            from_cache: false,
                            timestamp_ms,
                        },
                    );
                }
                CdpEvent::ResponseReceived {
                    session_id,
                    request_id,
                    status,
                    mime_type,
                    from_cache,
                    ..
                } if session_id.as_deref() == Some(&tab.cdp_session) => {
                    if let Some(entry) = network.get_mut(&request_id) {
                        entry.kind = "response".into();
                        entry.status = Some(status);
                        entry.mime_type = mime_type;
                        entry.from_cache = from_cache;
                    }
                }
                CdpEvent::LoadingFailed {
                    session_id,
                    request_id,
                    error_text,
                    ..
                } if session_id.as_deref() == Some(&tab.cdp_session) => {
                    if let Some(entry) = network.get_mut(&request_id) {
                        entry.kind = "failure".into();
                        entry.error_text = error_text;
                    }
                }
                _ => {}
            }
        }
        let mut network = network.into_values().collect::<Vec<_>>();
        network.sort_by_key(|entry| entry.timestamp_ms);
        console.reverse();
        network.reverse();
        let c = args
            .max_console_entries
            .unwrap_or(DEFAULT_CHROME_DIAGNOSTICS_ENTRIES)
            .min(MAX_CHROME_DIAGNOSTICS_ENTRIES);
        let n = args
            .max_network_entries
            .unwrap_or(DEFAULT_CHROME_DIAGNOSTICS_ENTRIES)
            .min(MAX_CHROME_DIAGNOSTICS_ENTRIES);
        let truncated = console.len() > c || network.len() > n;
        console.truncate(c);
        network.truncate(n);
        Ok(ChromeDiagnosticsResult {
            target_ref: args.target_ref.clone(),
            document_epoch: tab.epoch,
            console_entries: console,
            network_entries: network,
            truncated,
        })
    }
}
/// Map a child viewport point through its content quad. Rotation, scaling,
/// and skew are affine; perspective needs a different projection and refuses.
fn map_frame_point(quad: &Value, viewport: &Value, x: f64, y: f64) -> Result<(f64, f64), String> {
    const UNAVAILABLE: &str = "Chrome frame geometry is unavailable";
    let width = viewport["width"].as_f64().ok_or(UNAVAILABLE)?;
    let height = viewport["height"].as_f64().ok_or(UNAVAILABLE)?;
    if width <= 0.0 || height <= 0.0 || !width.is_finite() || !height.is_finite() {
        return Err(UNAVAILABLE.into());
    }
    let values = quad
        .as_array()
        .filter(|values| values.len() == 8)
        .ok_or(UNAVAILABLE)?;
    let mut points = [0.0; 8];
    for (point, value) in points.iter_mut().zip(values) {
        *point = value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or(UNAVAILABLE)?;
    }
    if (points[0] + points[4] - points[2] - points[6]).abs() > 0.01
        || (points[1] + points[5] - points[3] - points[7]).abs() > 0.01
    {
        return Err("Chrome input does not support perspective-transformed frames".into());
    }
    let horizontal = (points[2] - points[0], points[3] - points[1]);
    let vertical = (points[6] - points[0], points[7] - points[1]);
    if (horizontal.0 * vertical.1 - horizontal.1 * vertical.0).abs() < 0.01 {
        return Err(UNAVAILABLE.into());
    }
    Ok((
        points[0] + horizontal.0 * x / width + vertical.0 * y / height,
        points[1] + horizontal.1 * x / width + vertical.1 * y / height,
    ))
}

/// Identify owner boxes whose session-relative offsets reach the top viewport.
/// The whole chain is validated before a caller sends any geometry commands.
fn frame_offset_chain<'a>(
    snapshot: &'a Snapshot,
    frame_id: &str,
) -> Result<Vec<(&'a str, &'a str)>, String> {
    const UNRESOLVED: &str =
        "Chrome target frame position cannot be resolved; take a fresh snapshot";
    let top = snapshot.frames.first().ok_or(UNRESOLVED)?;
    if top.parent.is_some() {
        return Err(UNRESOLVED.into());
    }
    let frames = snapshot
        .frames
        .iter()
        .map(|frame| (frame.id.as_str(), frame))
        .collect::<HashMap<_, _>>();
    if frames.len() != snapshot.frames.len() {
        return Err(UNRESOLVED.into());
    }
    let mut current = *frames.get(frame_id).ok_or(UNRESOLVED)?;
    let mut seen = HashSet::new();
    let mut offsets = Vec::new();
    // No coordinate system owns the local target point until its first
    // owner box translates it into the queried session's root viewport.
    let mut coordinate_session = None;
    while current.id != top.id {
        if !seen.insert(current.id.as_str()) {
            return Err(UNRESOLVED.into());
        }
        let parent = current
            .parent
            .as_deref()
            .and_then(|id| frames.get(id).copied())
            .ok_or(UNRESOLVED)?;
        if coordinate_session != Some(parent.cdp_session.as_str()) {
            offsets.push((current.id.as_str(), parent.cdp_session.as_str()));
            coordinate_session = Some(parent.cdp_session.as_str());
        }
        current = parent;
    }
    Ok(offsets)
}

fn ensure_same_document(before: &[Frame], after: &[Frame]) -> Result<(), String> {
    if before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            a.id != b.id
                || a.loader != b.loader
                || a.url != b.url
                || a.cdp_session != b.cdp_session
                || a.parent != b.parent
        })
    {
        Err("Chrome document or frame changed; take a fresh snapshot".into())
    } else {
        Ok(())
    }
}
fn key_chord(value: &str) -> Result<(String, u32, u32), String> {
    let mut parts = value.split('+').collect::<Vec<_>>();
    let key = parts.pop().ok_or("Key is empty")?;
    let mut modifiers = 0;
    for modifier in parts {
        modifiers |= match modifier.to_ascii_lowercase().as_str() {
            "alt" | "option" => 1,
            "control" | "ctrl" => 2,
            "meta" | "command" | "cmd" => 4,
            "shift" => 8,
            _ => return Err("Unsupported key modifier".into()),
        };
    }
    let code = match key {
        "Enter" => 13,
        "Tab" => 9,
        "Escape" => 27,
        "Backspace" => 8,
        "Delete" => 46,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Home" => 36,
        "End" => 35,
        "PageUp" => 33,
        "PageDown" => 34,
        " " | "Space" => 32,
        key if key.len() == 1 => u32::from(key.as_bytes()[0].to_ascii_uppercase()),
        _ => return Err("Unsupported Chrome key".into()),
    };
    Ok((
        if key == "Space" {
            " ".into()
        } else {
            key.into()
        },
        modifiers,
        code,
    ))
}
fn parse<T: DeserializeOwned>(call: &ComputerUseCall) -> Result<T, String> {
    serde_json::from_value(call.arguments.clone())
        .map_err(|e| format!("Invalid Chrome arguments: {e}"))
}
fn data(value: impl serde::Serialize) -> Result<(Value, Vec<ComputerUseImage>), String> {
    serde_json::to_value(value)
        .map(|v| (v, Vec::new()))
        .map_err(|e| e.to_string())
}
pub struct ChromeCallOutcome {
    pub result: ComputerUseResult,
}
fn outcome(
    call: &ComputerUseCall,
    status: ComputerUseOutcome,
    text: &str,
    data: Value,
) -> ChromeCallOutcome {
    ChromeCallOutcome {
        result: ComputerUseResult {
            request_id: call.request_id,
            outcome: status,
            text: text.into(),
            data,
            error_code: if status == ComputerUseOutcome::Completed {
                None
            } else {
                Some("chrome_failed".into())
            },
            images: Vec::new(),
        },
    }
}

#[cfg(test)]
mod frame_geometry_tests {
    use super::*;

    #[test]
    fn scaled_and_rotated_frames_map_viewport_points_through_the_content_quad() {
        let viewport = json!({"width":200,"height":200});
        let scaled = json!([100, 120, 460, 120, 460, 480, 100, 480]);
        assert_eq!(
            map_frame_point(&scaled, &viewport, 50.0, 75.0).unwrap(),
            (190.0, 255.0)
        );
        let rotated = json!([300, 100, 300, 300, 100, 300, 100, 100]);
        assert_eq!(
            map_frame_point(&rotated, &viewport, 50.0, 75.0).unwrap(),
            (225.0, 150.0)
        );
    }

    #[test]
    fn perspective_or_degenerate_frame_geometry_refuses() {
        let viewport = json!({"width":200,"height":200});
        for quad in [
            json!([0, 0, 100, 0, 80, 100, 20, 100]),
            json!([0, 0, 0, 0, 0, 0, 0, 0]),
            json!([0, 0]),
        ] {
            assert!(map_frame_point(&quad, &viewport, 50.0, 75.0).is_err());
        }
        assert!(map_frame_point(
            &json!([0, 0, 100, 0, 100, 100, 0, 100]),
            &json!({"width":0,"height":200}),
            1.0,
            1.0
        )
        .is_err());
    }
}
