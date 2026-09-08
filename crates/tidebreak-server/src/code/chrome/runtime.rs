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
    ownership: ChromeOwnership,
}
#[derive(Clone)]
struct Access {
    cdp: CdpSession,
    grant: ChromeConnectionGrant,
    connection: String,
    fence: Fence,
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
        let result = tokio::select! {
         result=tokio::time::timeout(Duration::from_secs(15),&mut command)=>result.map_err(|_|format!("{method} timed out"))?.map_err(|error|error.0),
         _=self.fence.connection.cancelled()=>Err("Chrome connection revoked".into()),
         _=self.fence.session.cancelled()=>Err("Chrome session revoked".into()),
         _=self.fence.call.cancelled()=>Err("Chrome call cancelled".into()),
         _=async {while self.fence.live(){tokio::time::sleep(Duration::from_millis(10)).await;}}=>Err("Chrome control stopped".into()),
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
            out.push(Frame {
                cdp_session: cdp_session.into(),
                id: frame["id"].as_str().unwrap_or("").into(),
                loader: frame["loaderId"].as_str().unwrap_or("").into(),
                url: url.into(),
                name: frame["name"].as_str().unwrap_or("").into(),
            });
            if let Some(children) = value["childFrames"].as_array() {
                for child in children {
                    walk(child, out, url, cdp_session)?;
                }
            }
            Ok(())
        }
        let value = self
            .command(Some(session), "Page.getFrameTree", json!({}))
            .await?;
        let mut frames = Vec::new();
        walk(&value["frameTree"], &mut frames, "", session)?;
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
                walk(&tree["frameTree"], &mut child_frames, "", &child.session_id)?;
                for frame in child_frames {
                    if let Some(old) = frames.iter_mut().find(|old| old.id == frame.id) {
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
        let mutating = matches!(
            call.name.as_str(),
            CHROME_NEW_TAB_TOOL
                | CHROME_CLOSE_TAB_TOOL
                | CHROME_ACTIVATE_TAB_TOOL
                | CHROME_NAVIGATE_TOOL
                | CHROME_ACT_TOOL
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
            CHROME_CLOSE_TAB_TOOL | CHROME_ACTIVATE_TAB_TOOL => {
                let args: ChromeTabRefArgs = parse(call)?;
                let (access, mut tab) = self.tab(scope, &args.target_ref)?;
                let frames = access.frames(&tab.cdp_session).await?;
                self.refresh(&mut tab, &frames)?;
                if call.name == CHROME_CLOSE_TAB_TOOL {
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
                } else {
                    access
                        .command(Some(&tab.cdp_session), "Page.bringToFront", json!({}))
                        .await?;
                    tab.summary.active = true;
                    self.inner
                        .lock()
                        .unwrap()
                        .tabs
                        .insert(args.target_ref, tab.clone());
                }
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
        let scale = (args.max_width.unwrap_or(MAX_CHROME_SCREENSHOT_DIMENSION) as f64 / width)
            .min(
                args.max_height
                    .filter(|value| *value > 0)
                    .unwrap_or(MAX_CHROME_SCREENSHOT_DIMENSION) as f64
                    / height,
            )
            .min(1.0);
        let image=access.command(Some(&tab.cdp_session),"Page.captureScreenshot",json!({"format":"png","captureBeyondViewport":false,"clip":{"x":viewport["pageX"].as_f64().unwrap_or(0.0),"y":viewport["pageY"].as_f64().unwrap_or(0.0),"width":width,"height":height,"scale":scale}})).await?;
        self.checked_snapshot(&access, &tab, &args.snapshot_id, args.document_epoch)
            .await?;
        let encoded = image["data"]
            .as_str()
            .ok_or("Chrome screenshot has no image")?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "Chrome screenshot has invalid encoding")?;
        if bytes.len() > MAX_CHROME_SCREENSHOT_PNG_BYTES || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err("Chrome screenshot is invalid or exceeds its size limit".into());
        }
        Ok((
            json!({"targetRef":args.target_ref,"snapshotId":args.snapshot_id,"documentEpoch":args.document_epoch,"mimeType":"image/png"}),
            vec![ComputerUseImage {
                mime_type: "image/png".into(),
                base64: encoded.into(),
            }],
        ))
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
        if frame != &snapshot.frames[0].id {
            let owner = access
                .command(
                    Some(&tab.cdp_session),
                    "DOM.getFrameOwner",
                    json!({"frameId":frame}),
                )
                .await?;
            let bounds = access
                .command(
                    Some(&tab.cdp_session),
                    "DOM.getBoxModel",
                    json!({"backendNodeId":owner["backendNodeId"]}),
                )
                .await?;
            x += bounds["model"]["content"][0]
                .as_f64()
                .ok_or("Chrome frame offset is unavailable")?;
            y += bounds["model"]["content"][1]
                .as_f64()
                .ok_or("Chrome frame offset is unavailable")?;
        }
        Ok((x, y))
    }
    async fn mouse(
        &self,
        access: &Access,
        tab: &Tab,
        snapshot: &Snapshot,
        event: Value,
    ) -> Result<(), String> {
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
        let operation=async {
   match &args.action {
    ChromeAction::Fill{value}|ChromeAction::Select{value}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true,"operation":args.action.kind(),"value":value})).await?;}
    ChromeAction::Check{checked}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"operation":"check","value":checked})).await?;}
    ChromeAction::Type{text}=>{self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true})).await?;self.checked_snapshot(&access,&tab,&args.snapshot_id,args.document_epoch).await?;access.command(Some(&tab.cdp_session),"Input.insertText",json!({"text":text})).await?;}
    ChromeAction::Press{key}=>{
     self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true,"focus":true})).await?;
     let(key,modifiers,code)=key_chord(key)?;
     self.checked_snapshot(&access,&tab,&args.snapshot_id,args.document_epoch).await?;
     access.command(Some(&tab.cdp_session),"Input.dispatchKeyEvent",json!({"type":"keyDown","key":key,"modifiers":modifiers,"windowsVirtualKeyCode":code})).await?;
     access.command(Some(&tab.cdp_session),"Input.dispatchKeyEvent",json!({"type":"keyUp","key":key,"modifiers":modifiers,"windowsVirtualKeyCode":code})).await?;
    }
    ChromeAction::Drag{to_ref}=>{
     self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true})).await?;
     let(dx,dy)=self.probe(&access,&tab,&snapshot,to_ref,json!({"scroll":false})).await?;
     let(x,y)=self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":false})).await?;
     self.mouse(&access,&tab,&snapshot,json!({"type":"mouseMoved","x":x,"y":y})).await?;
     self.mouse(&access,&tab,&snapshot,json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await?;
     let drag=async {for step in 1..=12 {let t=f64::from(step)/12.0;self.mouse(&access,&tab,&snapshot,json!({"type":"mouseMoved","x":x+(dx-x)*t,"y":y+(dy-y)*t,"button":"left","buttons":1})).await?;tokio::time::sleep(Duration::from_millis(16)).await;}Ok::<(),String>(())}.await;
     if let Err(error)=drag {
       let _=tokio::time::timeout(Duration::from_secs(2),access.cdp.command_in_session(&tab.cdp_session,"Input.cancelDragging",json!({}))).await;
       return Err(error);
     }
     self.mouse(&access,&tab,&snapshot,json!({"type":"mouseReleased","x":dx,"y":dy,"button":"left","buttons":0,"clickCount":1})).await?;
    }
    ChromeAction::Click|ChromeAction::DoubleClick|ChromeAction::Hover|ChromeAction::Scroll{..}=>{
     let(x,y)=self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":true})).await?;
     self.mouse(&access,&tab,&snapshot,json!({"type":"mouseMoved","x":x,"y":y})).await?;
     match &args.action {
      ChromeAction::Hover=>{},
      ChromeAction::Scroll{x:dx,y:dy}=>{self.mouse(&access,&tab,&snapshot,json!({"type":"mouseWheel","x":x,"y":y,"deltaX":dx.unwrap_or(0.0),"deltaY":dy.unwrap_or(0.0)})).await?;},
      _=>{let count=if args.action==ChromeAction::DoubleClick{2}else{1};for click in 1..=count{
       self.probe(&access,&tab,&snapshot,&args.node_ref,json!({"scroll":false})).await?;
       self.mouse(&access,&tab,&snapshot,json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":click})).await?;
       access.command(Some(&tab.cdp_session),"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":x,"y":y,"button":"left","buttons":0,"clickCount":click})).await?;
      }}
     }
    }
   }
   Ok::<(),String>(())
  }.await;
        // A previous snapshot cannot authorize a second action after any attempt.
        if let Some(stored) = self.inner.lock().unwrap().tabs.get_mut(&args.target_ref) {
            stored.snapshot = None;
        }
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
fn ensure_same_document(before: &[Frame], after: &[Frame]) -> Result<(), String> {
    if before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            a.id != b.id || a.loader != b.loader || a.url != b.url || a.cdp_session != b.cdp_session
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
