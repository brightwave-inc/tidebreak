//! Host-owned browser identity and state.
//!
//! The renderer and remote page are projections of this registry. They may ask
//! for browser work, but they cannot create ownership, move a browser between
//! workspaces, or advance its document epoch.

use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

use serde::Serialize;
pub(crate) use tidebreak_core::{
    BrowserControllerKind, BrowserControllerState as BrowserController, BrowserEngineCapabilities,
    BrowserEngineDescriptor, BrowserLoadState, BrowserSessionSummary,
};
use tidebreak_core::{
    BrowserEngineName, BrowserGrantCapability, BrowserOrigin, BrowserOriginScope, OwnerId,
};
#[cfg(any(target_os = "macos", test))]
use tokio::sync::OwnedMutexGuard;
use tokio::sync::{watch, Mutex as AsyncMutex};
use uuid::Uuid;

use crate::browser_recovery::{
    BrowserSessionStore, LegacyBrowserImportResult, LegacyBrowserSession, RecoveredBrowserSession,
};

const BROWSER_AUDIT_FILE: &str = "browser-audit.jsonl";
const AGENT_CAPABILITY_TTL: Duration = Duration::from_secs(90);
const AGENT_CONFIRMATION_TTL: Duration = Duration::from_secs(30);
const MAX_AUDIT_ACTION_CHARS: usize = 64;
const MAX_AUDIT_TARGET_CHARS: usize = 160;

fn platform_default_engine() -> BrowserEngineDescriptor {
    #[cfg(target_os = "macos")]
    let name = BrowserEngineName::WkWebView;
    #[cfg(target_os = "windows")]
    let name = BrowserEngineName::WebView2;
    #[cfg(target_os = "linux")]
    let name = BrowserEngineName::WebKitGtk;
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let name = BrowserEngineName::Unsupported;

    BrowserEngineDescriptor {
        name,
        capabilities: BrowserEngineCapabilities {
            lifecycle: true,
            persistent_profile: true,
            // These become true only when the platform adapter is wired to
            // the engine-neutral semantic command contract.
            semantic_snapshot: cfg!(target_os = "macos"),
            semantic_actions: cfg!(target_os = "macos"),
            // WKWebView can consume parser-created closed shadow roots before
            // Tidebreak can prove that their rendered content is safe.
            screenshot: false,
            cross_origin_frames: false,
            profile_reset: cfg!(target_os = "macos"),
        },
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrowserAgentAccessScope {
    Origin,
    LoopbackWorkspace,
}

/// Renderer-safe projection of the native browser grant state.
///
/// The renderer receives only the normalized origin being discussed and the
/// effective capability booleans. Grant ids, capability ids, expiry, and
/// pending confirmation records never cross the native boundary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserAgentAccess {
    pub(crate) shared: bool,
    pub(crate) paused: bool,
    pub(crate) halted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<BrowserAgentAccessScope>,
    pub(crate) can_observe: bool,
    pub(crate) can_control: bool,
    pub(crate) can_transfer_files: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserSnapshot {
    pub(crate) exists: bool,
    pub(crate) browser_id: String,
    pub(crate) workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) load_state: Option<BrowserLoadState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) document_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) visible: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) engine: Option<BrowserEngineDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) controller: Option<BrowserController>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) agent_access: Option<BrowserAgentAccess>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) inspect_enabled: Option<bool>,
}

impl BrowserSnapshot {
    pub(crate) fn missing(browser_id: &str, workspace_id: &str) -> Self {
        Self {
            exists: false,
            browser_id: browser_id.to_owned(),
            workspace_id: workspace_id.to_owned(),
            profile_id: None,
            url: None,
            title: None,
            load_state: None,
            document_epoch: None,
            visible: None,
            engine: None,
            controller: None,
            agent_access: None,
            inspect_enabled: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserDispatchEffect {
    Observe,
    Mutate,
    Consequential,
}

impl BrowserDispatchEffect {
    fn confirmation_state(self) -> BrowserAuditConfirmation {
        match self {
            Self::Observe | Self::Mutate => BrowserAuditConfirmation::NotRequired,
            Self::Consequential => BrowserAuditConfirmation::Approved,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserAuditConfirmation {
    NotRequired,
    Approved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserAuditPhase {
    Intent,
    Outcome,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserAuditOutcome {
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserAuditEvent<'a> {
    version: u32,
    event_id: Uuid,
    timestamp: chrono::DateTime<chrono::Utc>,
    phase: BrowserAuditPhase,
    outcome: BrowserAuditOutcome,
    capability_id: Uuid,
    controller_label: &'a str,
    browser_id: &'a str,
    workspace_id: &'a str,
    origin: &'a str,
    action_type: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_target_label: Option<&'a str>,
    confirmation: BrowserAuditConfirmation,
}

#[derive(Clone, Default)]
struct BrowserAuditLog {
    path: Arc<OnceLock<PathBuf>>,
    writer: Arc<Mutex<()>>,
}

impl BrowserAuditLog {
    fn initialize(&self, data_dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|_| "could not create private browser state".to_owned())?;
        let path = data_dir.join(BROWSER_AUDIT_FILE);
        match self.path.set(path.clone()) {
            Ok(()) => Ok(()),
            Err(_) if self.path.get() == Some(&path) => Ok(()),
            Err(_) => Err("browser audit storage was initialized more than once".to_owned()),
        }
    }

    fn append(&self, event: &BrowserAuditEvent<'_>) -> Result<(), String> {
        let path = self
            .path
            .get()
            .ok_or_else(|| "browser audit storage is unavailable".to_owned())?;
        let _writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|_| "browser audit storage is unavailable".to_owned())?;
        serde_json::to_writer(&mut file, event)
            .map_err(|_| "browser audit event could not be encoded".to_owned())?;
        file.write_all(b"\n")
            .and_then(|()| file.sync_data())
            .map_err(|_| "browser audit event could not be persisted".to_owned())
    }
}

#[derive(Clone)]
struct BrowserRecord {
    instance_id: u64,
    owner_id: OwnerId,
    workspace_id: String,
    profile_id: String,
    url: Option<String>,
    title: Option<String>,
    load_state: BrowserLoadState,
    document_epoch: u64,
    visible: bool,
    engine: BrowserEngineDescriptor,
    controller: BrowserController,
    controller_capability_id: Option<Uuid>,
    paused_origin: Option<BrowserOrigin>,
    pending_navigation_url: Option<String>,
    dispatch: BrowserDispatchState,
    semantic_snapshot: Option<StoredSemanticSnapshot>,
    screenshot_epoch: Option<u64>,
    inspect_enabled: bool,
    resetting: bool,
}

pub(crate) struct ManagedBrowserRegistration {
    pub(crate) owner_id: OwnerId,
    pub(crate) profile_id: String,
    pub(crate) url: String,
    pub(crate) title: Option<String>,
    pub(crate) visible: bool,
}

#[derive(Clone)]
struct BrowserDispatchState {
    halt: watch::Sender<bool>,
    gate: Arc<AsyncMutex<()>>,
}

impl Default for BrowserDispatchState {
    fn default() -> Self {
        Self {
            halt: watch::channel(false).0,
            gate: Arc::new(AsyncMutex::new(())),
        }
    }
}

#[derive(Clone)]
struct BrowserAgentCapability {
    workspace_id: String,
    controller_label: String,
    expires_at: Instant,
}

#[derive(Clone)]
struct BrowserGrant {
    workspace_id: String,
    scope: BrowserOriginScope,
    capabilities: HashSet<BrowserGrantCapability>,
}

#[derive(Clone)]
struct BrowserConfirmationRecord {
    capability_id: Uuid,
    browser_id: String,
    workspace_id: String,
    origin: BrowserOrigin,
    action_type: String,
    target_label: Option<String>,
    expires_at: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrowserTargetFingerprint {
    pub(crate) tag: String,
    pub(crate) role: String,
    pub(crate) name: String,
    pub(crate) input_type: Option<String>,
    pub(crate) href: Option<String>,
    pub(crate) sensitive: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BrowserTargetRecord {
    pub(crate) frame_path: Vec<String>,
    pub(crate) selector: String,
    pub(crate) marker: String,
    pub(crate) marker_value: String,
    pub(crate) fingerprint: BrowserTargetFingerprint,
    pub(crate) sensitive: bool,
    pub(crate) consequential: bool,
}

#[derive(Clone, Debug)]
struct StoredSemanticSnapshot {
    snapshot_id: String,
    document_epoch: u64,
    targets: HashMap<String, BrowserTargetRecord>,
}

/// Instance and document identity read while re-validating live agent
/// authorization under one registry lock.
///
/// Long-running observations take a fence before and after their async work
/// and compare, so a result is never attributed to a session, document, or
/// authority it did not come from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BrowserObservationFence {
    pub(crate) instance_id: u64,
    pub(crate) document_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserTargetError {
    StaleTarget,
    BrowserHidden,
}

#[allow(
    clippy::large_enum_variant,
    reason = "navigation decisions are short-lived and carry the exact paused-state snapshot"
)]
pub(crate) enum BrowserNavigationDecision {
    Allow,
    Pause {
        origin: String,
        snapshot: BrowserSnapshot,
    },
    Deny,
}

impl BrowserRecord {
    fn snapshot(&self, browser_id: &str, agent_access: BrowserAgentAccess) -> BrowserSnapshot {
        BrowserSnapshot {
            exists: true,
            browser_id: browser_id.to_owned(),
            workspace_id: self.workspace_id.clone(),
            profile_id: Some(self.profile_id.clone()),
            url: self.url.clone(),
            title: self.title.clone(),
            load_state: Some(self.load_state),
            document_epoch: Some(self.document_epoch),
            visible: Some(self.visible),
            engine: Some(self.engine.clone()),
            controller: Some(self.controller.clone()),
            agent_access: Some(agent_access),
            inspect_enabled: Some(self.inspect_enabled),
        }
    }

    fn summary(&self, browser_id: &str) -> BrowserSessionSummary {
        BrowserSessionSummary {
            browser_id: browser_id.to_owned(),
            url: self.url.clone(),
            title: self.title.clone(),
            load_state: self.load_state,
            visible: self.visible,
            engine: self.engine.clone(),
            controller: self.controller.clone(),
        }
    }
}

#[derive(Default)]
struct BrowserRegistryState {
    records: HashMap<String, BrowserRecord>,
    next_instance_id: u64,
    capabilities: HashMap<Uuid, BrowserAgentCapability>,
    grants: Vec<BrowserGrant>,
    confirmations: HashMap<Uuid, BrowserConfirmationRecord>,
}

#[derive(Clone)]
pub(crate) struct BrowserRegistry {
    state: Arc<Mutex<BrowserRegistryState>>,
    audit: BrowserAuditLog,
    sessions: BrowserSessionStore,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrowserProfileResetSession {
    pub(crate) browser_id: String,
    pub(crate) workspace_id: String,
    instance_id: u64,
    previous_halt: bool,
}

/// Exclusive native reset lease for every live session using one managed
/// profile. The dispatch guards keep agent work drained until the profile is
/// either deleted or the reset aborts. Dropping an unfinished lease releases
/// the temporary halt latch on any session that is still the same instance.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct BrowserProfileResetLease {
    registry: BrowserRegistry,
    owner_id: OwnerId,
    profile_id: String,
    sessions: Vec<BrowserProfileResetSession>,
    _dispatch_guards: Vec<OwnedMutexGuard<()>>,
    finished: bool,
}

#[cfg(any(target_os = "macos", test))]
impl BrowserProfileResetLease {
    pub(crate) fn owner_id(&self) -> &OwnerId {
        &self.owner_id
    }

    pub(crate) fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub(crate) fn sessions(&self) -> &[BrowserProfileResetSession] {
        &self.sessions
    }

    pub(crate) fn finish(mut self) {
        self.registry.finish_profile_reset(&self);
        self.finished = true;
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for BrowserProfileResetLease {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.cancel_profile_reset(self);
        }
    }
}

impl Default for BrowserRegistry {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(BrowserRegistryState::default())),
            audit: BrowserAuditLog::default(),
            sessions: BrowserSessionStore::default(),
        }
    }
}

impl BrowserRegistry {
    pub(crate) fn initialize_private_state(&self, data_dir: &Path) -> Result<(), String> {
        self.audit.initialize(data_dir)?;
        self.sessions.initialize(data_dir)
    }

    pub(crate) fn recover_session(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<Option<RecoveredBrowserSession>, String> {
        self.sessions.recover(owner_id, browser_id, workspace_id)
    }

    pub(crate) fn ensure_recovery_binding(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        self.sessions
            .ensure_binding(owner_id, browser_id, workspace_id)
    }

    pub(crate) fn import_legacy_session(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
        legacy: Option<LegacyBrowserSession>,
    ) -> Result<LegacyBrowserImportResult, String> {
        self.sessions
            .import_legacy(owner_id, browser_id, workspace_id, legacy)
    }

    pub(crate) fn forget_recovery(
        &self,
        owner_id: &OwnerId,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        self.sessions.forget(owner_id, browser_id, workspace_id)
    }

    pub(crate) fn register_managed(
        &self,
        browser_id: &str,
        workspace_id: &str,
        owner_id: OwnerId,
        profile_id: String,
        url: String,
        visible: bool,
    ) -> Result<u64, String> {
        self.register_managed_with_title(
            browser_id,
            workspace_id,
            ManagedBrowserRegistration {
                owner_id,
                profile_id,
                url,
                title: None,
                visible,
            },
        )
    }

    pub(crate) fn register_managed_with_title(
        &self,
        browser_id: &str,
        workspace_id: &str,
        registration: ManagedBrowserRegistration,
    ) -> Result<u64, String> {
        let ManagedBrowserRegistration {
            owner_id,
            profile_id,
            url,
            title,
            visible,
        } = registration;
        let mut state = self.lock();
        if let Some(record) = state.records.get(browser_id) {
            ensure_workspace(browser_id, workspace_id, record)?;
            return Err("browser session is already registered".to_owned());
        }

        state.next_instance_id = state.next_instance_id.saturating_add(1);
        let instance_id = state.next_instance_id;
        state.records.insert(
            browser_id.to_owned(),
            BrowserRecord {
                instance_id,
                owner_id,
                workspace_id: workspace_id.to_owned(),
                profile_id,
                url: Some(url),
                title,
                load_state: BrowserLoadState::Loading,
                document_epoch: 0,
                visible,
                engine: platform_default_engine(),
                controller: BrowserController::default(),
                controller_capability_id: None,
                paused_origin: None,
                pending_navigation_url: None,
                dispatch: BrowserDispatchState::default(),
                semantic_snapshot: None,
                screenshot_epoch: None,
                inspect_enabled: false,
                resetting: false,
            },
        );
        Ok(instance_id)
    }

    #[cfg(test)]
    pub(crate) fn register(
        &self,
        browser_id: &str,
        workspace_id: &str,
        url: String,
        visible: bool,
    ) -> Result<u64, String> {
        self.register_managed(
            browser_id,
            workspace_id,
            OwnerId::local(),
            "00000000-0000-4000-8000-000000000001".to_owned(),
            url,
            visible,
        )
    }

    /// Halt and drain every live session on the exact profile selected by the
    /// trusted triggering browser. No owner or profile selector is accepted
    /// from renderer IPC.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) async fn begin_profile_reset(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserProfileResetLease, String> {
        let (owner_id, profile_id, sessions, gates) = {
            let mut state = self.lock();
            let trigger = state
                .records
                .get(browser_id)
                .ok_or_else(|| "browser session is not registered".to_owned())?;
            ensure_workspace(browser_id, workspace_id, trigger)?;
            let owner_id = trigger.owner_id.clone();
            let profile_id = trigger.profile_id.clone();
            let mut sessions = state
                .records
                .iter()
                .filter(|(_, record)| {
                    record.owner_id == owner_id && record.profile_id == profile_id
                })
                .map(|(browser_id, record)| BrowserProfileResetSession {
                    browser_id: browser_id.clone(),
                    workspace_id: record.workspace_id.clone(),
                    instance_id: record.instance_id,
                    previous_halt: *record.dispatch.halt.borrow(),
                })
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| left.browser_id.cmp(&right.browser_id));
            let gates = sessions
                .iter()
                .map(|session| {
                    let record = state
                        .records
                        .get_mut(&session.browser_id)
                        .expect("reset session was collected from the same registry lock");
                    record.dispatch.halt.send_replace(true);
                    record.resetting = true;
                    Arc::clone(&record.dispatch.gate)
                })
                .collect::<Vec<_>>();
            (owner_id, profile_id, sessions, gates)
        };

        let mut lease = BrowserProfileResetLease {
            registry: self.clone(),
            owner_id,
            profile_id,
            sessions,
            _dispatch_guards: Vec::with_capacity(gates.len()),
            finished: false,
        };
        for gate in gates {
            lease._dispatch_guards.push(gate.lock_owned().await);
        }
        Ok(lease)
    }

    #[cfg(any(target_os = "macos", test))]
    fn cancel_profile_reset(&self, reset: &BrowserProfileResetLease) {
        let mut state = self.lock();
        for session in &reset.sessions {
            let Some(record) = state.records.get_mut(&session.browser_id) else {
                continue;
            };
            if record.instance_id == session.instance_id
                && record.owner_id == reset.owner_id
                && record.profile_id == reset.profile_id
            {
                record.dispatch.halt.send_replace(session.previous_halt);
                record.resetting = false;
            }
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn finish_profile_reset(&self, reset: &BrowserProfileResetLease) {
        let reset_instances = reset
            .sessions
            .iter()
            .map(|session| (session.browser_id.as_str(), session.instance_id))
            .collect::<HashMap<_, _>>();
        let mut state = self.lock();
        state.records.retain(|browser_id, record| {
            !(record.owner_id == reset.owner_id
                && record.profile_id == reset.profile_id
                && reset_instances.get(browser_id.as_str()) == Some(&record.instance_id))
        });
        state.confirmations.retain(|_, confirmation| {
            !reset_instances.contains_key(confirmation.browser_id.as_str())
        });
    }

    pub(crate) fn ensure_workspace(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<(), String> {
        let state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)
    }

    pub(crate) fn snapshot(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    /// List only sessions in the exact workspace a trusted caller has already
    /// authorized. The workspace id is deliberately absent from each returned
    /// model-facing summary, so it cannot become a transferable capability.
    pub(crate) fn list_for_workspace(&self, workspace_id: &str) -> Vec<BrowserSessionSummary> {
        let state = self.lock();
        let mut sessions: Vec<_> = state
            .records
            .iter()
            .filter(|(_, record)| record.workspace_id == workspace_id && !record.resetting)
            .map(|(browser_id, record)| record.summary(browser_id))
            .collect();
        sessions.sort_by(|left, right| left.browser_id.cmp(&right.browser_id));
        sessions
    }

    pub(crate) fn set_visible(
        &self,
        browser_id: &str,
        workspace_id: &str,
        visible: bool,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if record.visible != visible {
            // A person can change the page while the child view is obscured,
            // and a newly revealed view must never inherit an old target map.
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
        record.visible = visible;
        Ok(())
    }

    pub(crate) fn set_inspect(
        &self,
        browser_id: &str,
        workspace_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        record.inspect_enabled = enabled;
        if !enabled {
            record.semantic_snapshot = None;
        }
        Ok(())
    }

    pub(crate) fn clear_inspect(&self, browser_id: &str, workspace_id: &str) -> Result<(), String> {
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        record.inspect_enabled = false;
        Ok(())
    }

    pub(crate) fn issue_agent_capability(
        &self,
        workspace_id: &str,
        controller_label: &str,
    ) -> Uuid {
        self.issue_agent_capability_for(workspace_id, controller_label, AGENT_CAPABILITY_TTL)
    }

    fn issue_agent_capability_for(
        &self,
        workspace_id: &str,
        controller_label: &str,
        ttl: Duration,
    ) -> Uuid {
        let capability_id = Uuid::new_v4();
        self.lock().capabilities.insert(
            capability_id,
            BrowserAgentCapability {
                workspace_id: workspace_id.to_owned(),
                controller_label: clean_controller_text(controller_label, 80, "Agent"),
                expires_at: Instant::now() + ttl,
            },
        );
        capability_id
    }

    pub(crate) fn heartbeat_agent_capability(
        &self,
        capability_id: Uuid,
        workspace_id: &str,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let capability = state
            .capabilities
            .get_mut(&capability_id)
            .ok_or_else(|| "browser capability is unavailable".to_owned())?;
        if capability.workspace_id != workspace_id || capability.expires_at <= Instant::now() {
            return Err("browser capability is unavailable".to_owned());
        }
        capability.expires_at = Instant::now() + AGENT_CAPABILITY_TTL;
        Ok(())
    }

    /// Atomically replace one expired capability for the same live workspace.
    ///
    /// Controller ownership moves to the replacement id without changing the
    /// halt latch, pending navigation, or semantic fences. This lets a live
    /// code session recover after an idle TTL while preserving an explicit
    /// user Stop or human takeover exactly as the native registry recorded it.
    pub(crate) fn rotate_expired_agent_capability(
        &self,
        capability_id: Uuid,
        workspace_id: &str,
        controller_label: &str,
    ) -> Result<Uuid, String> {
        let mut state = self.lock();
        let previous = state
            .capabilities
            .get(&capability_id)
            .cloned()
            .ok_or_else(|| "browser capability is unavailable".to_owned())?;
        if previous.workspace_id != workspace_id || previous.expires_at > Instant::now() {
            return Err("browser capability is unavailable".to_owned());
        }

        let replacement = Uuid::new_v4();
        let controller_label = clean_controller_text(controller_label, 80, "Agent");
        state.capabilities.remove(&capability_id);
        state.capabilities.insert(
            replacement,
            BrowserAgentCapability {
                workspace_id: workspace_id.to_owned(),
                controller_label: controller_label.clone(),
                expires_at: Instant::now() + AGENT_CAPABILITY_TTL,
            },
        );
        for record in state.records.values_mut() {
            if record.controller_capability_id == Some(capability_id) {
                record.controller_capability_id = Some(replacement);
                if record.controller.kind == BrowserControllerKind::Agent {
                    record.controller.label = Some(controller_label.clone());
                }
            }
        }
        state
            .confirmations
            .retain(|_, confirmation| confirmation.capability_id != capability_id);
        Ok(replacement)
    }

    #[cfg(test)]
    pub(crate) fn expire_agent_capability_for_test(&self, capability_id: Uuid) {
        if let Some(capability) = self.lock().capabilities.get_mut(&capability_id) {
            capability.expires_at = Instant::now();
        }
    }

    pub(crate) fn revoke_agent_capability(&self, capability_id: Uuid) {
        let mut state = self.lock();
        state.capabilities.remove(&capability_id);
        for record in state.records.values_mut() {
            if record.controller_capability_id == Some(capability_id) {
                record.dispatch.halt.send_replace(true);
                record.controller.halted = true;
                record.controller.action = None;
                record.controller.takeover_required = false;
                record.paused_origin = None;
                record.pending_navigation_url = None;
                record.semantic_snapshot = None;
                record.screenshot_epoch = None;
            }
        }
        state
            .confirmations
            .retain(|_, confirmation| confirmation.capability_id != capability_id);
    }

    pub(crate) fn grant_browser_access(
        &self,
        browser_id: &str,
        workspace_id: &str,
        target_origin: &BrowserOrigin,
        scope: BrowserOriginScope,
        capabilities: &[BrowserGrantCapability],
    ) -> Result<BrowserSnapshot, String> {
        if !scope.covers(target_origin) {
            return Err("browser grant scope does not cover this origin".to_owned());
        }
        if matches!(scope, BrowserOriginScope::LoopbackWorkspace) && !target_origin.is_loopback() {
            return Err("all-local-sites access is available only for loopback origins".to_owned());
        }
        let mut state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if share_target_for_record(record).as_ref() != Some(target_origin) {
            return Err("browser origin changed while permission was being requested".to_owned());
        }

        if let Some(grant) = state
            .grants
            .iter_mut()
            .find(|grant| grant.workspace_id == workspace_id && grant.scope == scope)
        {
            grant.capabilities.extend(capabilities.iter().copied());
        } else {
            state.grants.push(BrowserGrant {
                workspace_id: workspace_id.to_owned(),
                scope: scope.clone(),
                capabilities: capabilities.iter().copied().collect(),
            });
        }

        {
            let record = state
                .records
                .get_mut(browser_id)
                .expect("browser was checked above");
            if record
                .paused_origin
                .as_ref()
                .is_some_and(|origin| scope.covers(origin))
            {
                record.paused_origin = None;
            }
            record.dispatch.halt.send_replace(false);
            if record.controller.halted {
                record.controller = BrowserController::default();
                record.controller_capability_id = None;
            }
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    pub(crate) fn revoke_browser_access(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let mut state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        let target = share_target_for_record(record)
            .ok_or_else(|| "browser has no shareable HTTP origin".to_owned())?;
        state
            .grants
            .retain(|grant| grant.workspace_id != workspace_id || !grant.scope.covers(&target));
        {
            let record = state
                .records
                .get_mut(browser_id)
                .expect("browser was checked above");
            record.dispatch.halt.send_replace(true);
            record.paused_origin = None;
            record.pending_navigation_url = None;
            if record.controller.kind == BrowserControllerKind::Agent {
                record.controller = BrowserController::default();
                record.controller_capability_id = None;
            }
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    pub(crate) fn share_target_origin(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserOrigin, String> {
        let state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        share_target_for_record(record)
            .ok_or_else(|| "browser has no shareable HTTP origin".to_owned())
    }

    /// Return the exact validated URL that native navigation paused before
    /// exposing it, after the user has approved access to that destination.
    /// The renderer never receives or chooses this replay target.
    pub(crate) fn take_pending_navigation(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<Option<String>, String> {
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if record.paused_origin.is_some() {
            return Err("browser navigation has not been approved".to_owned());
        }
        Ok(record.pending_navigation_url.take())
    }

    pub(crate) fn list_for_capability(
        &self,
        capability_id: Uuid,
    ) -> Result<Vec<BrowserSessionSummary>, String> {
        let state = self.lock();
        let capability = active_capability(&state, capability_id)?;
        let mut sessions: Vec<_> = state
            .records
            .iter()
            .filter(|(_, record)| {
                record.workspace_id == capability.workspace_id
                    && record.visible
                    && !record.resetting
            })
            .filter(|(_, record)| {
                current_origin(record).is_some_and(|origin| {
                    grants_cover(
                        &state,
                        &record.workspace_id,
                        &origin,
                        BrowserGrantCapability::BrowserObserveOrigin,
                    )
                })
            })
            .map(|(browser_id, record)| record.summary(browser_id))
            .collect();
        sessions.sort_by(|left, right| left.browser_id.cmp(&right.browser_id));
        Ok(sessions)
    }

    pub(crate) fn begin_agent_control(
        &self,
        capability_id: Uuid,
        browser_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let mut state = self.lock();
        let capability = active_capability(&state, capability_id)?.clone();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, &capability.workspace_id, record)?;
        let origin = current_origin(record)
            .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
        if !record.visible {
            return Err("browser is hidden".to_owned());
        }
        if *record.dispatch.halt.borrow() {
            return Err("browser control was stopped by the user".to_owned());
        }
        if !grants_cover(
            &state,
            &capability.workspace_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
        ) {
            return Err("browser origin is not shared with this agent".to_owned());
        }
        {
            let record = state
                .records
                .get_mut(browser_id)
                .expect("browser was checked above");
            match record.controller_capability_id {
                Some(active) if active != capability_id => {
                    return Err("browser is controlled by another agent".to_owned());
                }
                _ => {}
            }
            record.controller = BrowserController {
                kind: BrowserControllerKind::Agent,
                label: Some(capability.controller_label),
                action: None,
                halted: false,
                takeover_required: false,
            };
            record.controller_capability_id = Some(capability_id);
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    /// Re-check live authorization for observation without clearing the
    /// stored semantic snapshot.
    ///
    /// Unlike [`begin_agent_control`](Self::begin_agent_control), this
    /// preserves `semantic_snapshot` and `screenshot_epoch` so a wait or
    /// screenshot can chain on a prior snapshot. Used by observation-only
    /// operations (wait, screenshot) that validate `snapshot_id` and
    /// `document_epoch` against the stored snapshot.
    ///
    /// Requires exclusive agent control from a prior
    /// [`begin_agent_control`](Self::begin_agent_control) — this method does
    /// NOT acquire or mutate controller state. It returns
    /// `"browser is not controlled by this agent"` when the controller is not
    /// already this agent capability (human takeover, different agent, or
    /// no prior control). Two concurrent observers for the same browser by
    /// different capabilities are refused.
    pub(crate) fn begin_agent_observation(
        &self,
        capability_id: Uuid,
        browser_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let state = self.lock();
        let capability = active_capability(&state, capability_id)?.clone();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, &capability.workspace_id, record)?;
        let origin = current_origin(record)
            .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
        if !record.visible {
            return Err("browser is hidden".to_owned());
        }
        if *record.dispatch.halt.borrow() {
            return Err("browser control was stopped by the user".to_owned());
        }
        if !grants_cover(
            &state,
            &capability.workspace_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
        ) {
            return Err("browser origin is not shared with this agent".to_owned());
        }
        // Observation does NOT acquire or mutate controller state — it only
        // succeeds when this capability already holds exclusive agent control
        // (set by a prior begin_agent_control). A human takeover, a different
        // agent, or no controller at all is refused. This preserves the stored
        // semantic snapshot that issued the refs the observation chains from.
        if record.controller.kind != BrowserControllerKind::Agent
            || record.controller_capability_id != Some(capability_id)
        {
            return Err("browser is not controlled by this agent".to_owned());
        }
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    pub(crate) fn set_agent_action(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        action: Option<&str>,
        takeover_required: bool,
    ) -> Result<BrowserSnapshot, String> {
        let mut state = self.lock();
        let capability = active_capability(&state, capability_id)?.clone();
        {
            let record = state
                .records
                .get_mut(browser_id)
                .ok_or_else(|| "browser session is not registered".to_owned())?;
            ensure_workspace(browser_id, &capability.workspace_id, record)?;
            if record.controller.kind != BrowserControllerKind::Agent
                || record.controller_capability_id != Some(capability_id)
            {
                return Err("browser is not controlled by this agent".to_owned());
            }
            record.controller.action =
                action.map(|value| clean_controller_text(value, 160, "Working"));
            record.controller.takeover_required = takeover_required;
        }
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    pub(crate) async fn stop_agent_control(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let gate = {
            let mut state = self.lock();
            let record = state
                .records
                .get_mut(browser_id)
                .ok_or_else(|| "browser session is not registered".to_owned())?;
            ensure_workspace(browser_id, workspace_id, record)?;
            record.dispatch.halt.send_replace(true);
            if record.controller.kind == BrowserControllerKind::Agent {
                record.controller.halted = true;
                record.controller.action = None;
                record.controller.takeover_required = false;
                record.semantic_snapshot = None;
                record.screenshot_epoch = None;
            }
            Arc::clone(&record.dispatch.gate)
        };
        let _dispatch = gate.lock().await;
        self.snapshot(browser_id, workspace_id)
    }

    pub(crate) async fn take_human_control(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<BrowserSnapshot, String> {
        let (gate, instance_id) = {
            let mut state = self.lock();
            let record = state
                .records
                .get_mut(browser_id)
                .ok_or_else(|| "browser session is not registered".to_owned())?;
            ensure_workspace(browser_id, workspace_id, record)?;
            record.dispatch.halt.send_replace(true);
            if record.controller.kind == BrowserControllerKind::Agent {
                record.controller = BrowserController::default();
                record.controller_capability_id = None;
            }
            record.paused_origin = None;
            record.pending_navigation_url = None;
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
            record.inspect_enabled = false;
            (Arc::clone(&record.dispatch.gate), record.instance_id)
        };

        // Publish the halt before waiting so queued input fails closed. Once
        // the active dispatch drains, release the latch as part of the same
        // explicit human-control transition. A recreated browser instance is
        // never modified by this older takeover request.
        let _dispatch = gate.lock().await;
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if record.instance_id != instance_id {
            return Err("browser session changed while control was transferring".to_owned());
        }
        record.dispatch.halt.send_replace(false);
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        Ok(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    /// Record one native act-time confirmation. Only native browser executor
    /// code can receive the returned opaque id; there is no renderer command
    /// for enumerating or redeeming confirmation records.
    pub(crate) fn record_native_confirmation(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        action_type: &str,
        target_label: Option<&str>,
    ) -> Result<Uuid, String> {
        self.record_native_confirmation_for(
            capability_id,
            browser_id,
            origin,
            action_type,
            target_label,
            AGENT_CONFIRMATION_TTL,
        )
    }

    fn record_native_confirmation_for(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        action_type: &str,
        target_label: Option<&str>,
        ttl: Duration,
    ) -> Result<Uuid, String> {
        let mut state = self.lock();
        let capability = active_capability(&state, capability_id)?.clone();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, &capability.workspace_id, record)?;
        if current_origin(record).as_ref() != Some(origin) {
            return Err("browser origin changed before confirmation".to_owned());
        }
        if record.controller.kind != BrowserControllerKind::Agent
            || record.controller_capability_id != Some(capability_id)
            || *record.dispatch.halt.borrow()
        {
            return Err("browser is not controlled by this agent".to_owned());
        }
        if !grants_cover(
            &state,
            &capability.workspace_id,
            origin,
            BrowserGrantCapability::BrowserControlOrigin,
        ) {
            return Err("browser origin is not shared for control".to_owned());
        }
        let confirmation_id = Uuid::new_v4();
        state.confirmations.insert(
            confirmation_id,
            BrowserConfirmationRecord {
                capability_id,
                browser_id: browser_id.to_owned(),
                workspace_id: capability.workspace_id,
                origin: origin.clone(),
                action_type: clean_audit_text(
                    action_type,
                    MAX_AUDIT_ACTION_CHARS,
                    "browser_action",
                ),
                target_label: target_label.map(|value| {
                    clean_audit_text(value, MAX_AUDIT_TARGET_CHARS, "unlabeled target")
                }),
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(confirmation_id)
    }

    /// Linearize one agent operation with Stop/takeover and reauthorize it at
    /// the last possible native boundary before engine dispatch.
    #[allow(
        clippy::too_many_arguments,
        reason = "security-sensitive authorization inputs stay explicit at the native dispatch boundary"
    )]
    pub(crate) async fn dispatch_agent<T, F, Fut>(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        required_capability: BrowserGrantCapability,
        action_type: &str,
        target_label: Option<&str>,
        effect: BrowserDispatchEffect,
        confirmation_id: Option<Uuid>,
        dispatch: F,
    ) -> Result<T, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, String>>,
    {
        let gate = {
            let state = self.lock();
            Arc::clone(
                &state
                    .records
                    .get(browser_id)
                    .ok_or_else(|| "browser session is not registered".to_owned())?
                    .dispatch
                    .gate,
            )
        };
        let _dispatch_gate = gate.lock().await;
        let action_type = clean_audit_text(action_type, MAX_AUDIT_ACTION_CHARS, "browser_action");
        let target_label = target_label
            .map(|value| clean_audit_text(value, MAX_AUDIT_TARGET_CHARS, "unlabeled target"));
        let capability = {
            let mut state = self.lock();
            authorize_agent_dispatch(
                &mut state,
                capability_id,
                browser_id,
                origin,
                required_capability,
                &action_type,
                target_label.as_deref(),
                effect,
                confirmation_id,
                false,
            )?
        };
        let event_id = Uuid::new_v4();
        let confirmation = effect.confirmation_state();
        self.audit.append(&BrowserAuditEvent {
            version: 1,
            event_id,
            timestamp: chrono::Utc::now(),
            phase: BrowserAuditPhase::Intent,
            outcome: BrowserAuditOutcome::Pending,
            capability_id,
            controller_label: &capability.controller_label,
            browser_id,
            workspace_id: &capability.workspace_id,
            origin: origin.as_str(),
            action_type: &action_type,
            semantic_target_label: target_label.as_deref(),
            confirmation,
        })?;

        let final_authorization = {
            let mut state = self.lock();
            authorize_agent_dispatch(
                &mut state,
                capability_id,
                browser_id,
                origin,
                required_capability,
                &action_type,
                target_label.as_deref(),
                effect,
                confirmation_id,
                true,
            )
        };
        if let Err(error) = final_authorization {
            let _ = self.audit.append(&BrowserAuditEvent {
                version: 1,
                event_id,
                timestamp: chrono::Utc::now(),
                phase: BrowserAuditPhase::Outcome,
                outcome: BrowserAuditOutcome::Failed,
                capability_id,
                controller_label: &capability.controller_label,
                browser_id,
                workspace_id: &capability.workspace_id,
                origin: origin.as_str(),
                action_type: &action_type,
                semantic_target_label: target_label.as_deref(),
                confirmation,
            });
            return Err(error);
        }

        // No await occurs between the final native state check and polling the
        // engine future. Stop may publish its latch immediately, then waits on
        // this gate before reporting completion.
        let result = dispatch().await;
        {
            let mut state = self.lock();
            if let Some(record) = state.records.get_mut(browser_id) {
                if record.controller_capability_id == Some(capability_id) {
                    record.controller.action = None;
                }
            }
        }
        if let Err(error) = self.audit.append(&BrowserAuditEvent {
            version: 1,
            event_id,
            timestamp: chrono::Utc::now(),
            phase: BrowserAuditPhase::Outcome,
            outcome: if result.is_ok() {
                BrowserAuditOutcome::Succeeded
            } else {
                BrowserAuditOutcome::Failed
            },
            capability_id,
            controller_label: &capability.controller_label,
            browser_id,
            workspace_id: &capability.workspace_id,
            origin: origin.as_str(),
            action_type: &action_type,
            semantic_target_label: target_label.as_deref(),
            confirmation,
        }) {
            eprintln!("tidebreak-desktop: browser audit outcome was not persisted: {error}");
        }
        result
    }

    /// Gate both agent-initiated navigation and page redirects before the new
    /// document is exposed. Human navigation remains subject only to the URL
    /// safety checks in the child-webview host.
    pub(crate) fn authorize_navigation(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        destination_url: &str,
        destination: &BrowserOrigin,
    ) -> BrowserNavigationDecision {
        let mut state = self.lock();
        let Some(record) = state.records.get(browser_id) else {
            return BrowserNavigationDecision::Deny;
        };
        if record.workspace_id != workspace_id || record.instance_id != instance_id {
            return BrowserNavigationDecision::Deny;
        }
        if record.resetting {
            return BrowserNavigationDecision::Deny;
        }
        if record.controller.kind != BrowserControllerKind::Agent {
            return BrowserNavigationDecision::Allow;
        }

        let authorized = record
            .controller_capability_id
            .and_then(|capability_id| state.capabilities.get(&capability_id))
            .is_some_and(|capability| {
                capability.workspace_id == workspace_id
                    && capability.expires_at > Instant::now()
                    && !*record.dispatch.halt.borrow()
                    && grants_cover(
                        &state,
                        workspace_id,
                        destination,
                        BrowserGrantCapability::BrowserControlOrigin,
                    )
            });
        if authorized {
            return BrowserNavigationDecision::Allow;
        }

        {
            let record = state
                .records
                .get_mut(browser_id)
                .expect("browser was checked above");
            record.dispatch.halt.send_replace(true);
            record.paused_origin = Some(destination.clone());
            record.pending_navigation_url = Some(destination_url.to_owned());
            record.controller.halted = true;
            record.controller.action = Some(format!(
                "Navigation paused for {}",
                clean_controller_text(destination.as_str(), 120, "another site")
            ));
            record.controller.takeover_required = false;
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
        let record = state
            .records
            .get(browser_id)
            .expect("browser was checked above");
        let snapshot = record.snapshot(browser_id, agent_access_for_record(&state, record));
        BrowserNavigationDecision::Pause {
            origin: destination.as_str().to_owned(),
            snapshot,
        }
    }

    pub(crate) fn page_started(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        url: String,
    ) -> Option<BrowserSnapshot> {
        self.update_instance(browser_id, workspace_id, instance_id, |record| {
            record.url = Some(url);
            record.title = None;
            record.load_state = BrowserLoadState::Loading;
            record.document_epoch = record.document_epoch.saturating_add(1);
            record.paused_origin = None;
            record.pending_navigation_url = None;
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
            record.inspect_enabled = false;
        })
    }

    pub(crate) fn page_finished(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        url: String,
    ) -> Option<BrowserSnapshot> {
        let (snapshot, owner_id, title) = {
            let mut state = self.lock();
            {
                let record = state.records.get_mut(browser_id)?;
                if record.workspace_id != workspace_id
                    || record.instance_id != instance_id
                    || record.resetting
                {
                    return None;
                }
                record.url = Some(url.clone());
                record.load_state = BrowserLoadState::Ready;
            }
            let record = state.records.get(browser_id)?;
            (
                record.snapshot(browser_id, agent_access_for_record(&state, record)),
                record.owner_id.clone(),
                record.title.clone(),
            )
        };
        if let Err(error) =
            self.sessions
                .commit(&owner_id, browser_id, workspace_id, &url, title.as_deref())
        {
            log_recovery_persistence_error(error);
        }
        Some(snapshot)
    }

    /// Record a validated same-document URL change without advancing the
    /// document epoch. The instance and epoch fence prevents a late observer
    /// from writing into a replacement view or a later document.
    pub(crate) fn same_document_navigation(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        document_epoch: u64,
        url: String,
    ) -> Option<BrowserSnapshot> {
        let (snapshot, owner_id, title) = {
            let mut state = self.lock();
            {
                let record = state.records.get_mut(browser_id)?;
                if record.workspace_id != workspace_id
                    || record.instance_id != instance_id
                    || record.document_epoch != document_epoch
                    || record.resetting
                    || record.url.as_deref() == Some(url.as_str())
                {
                    return None;
                }
                record.url = Some(url.clone());
                record.load_state = BrowserLoadState::Ready;
            }
            let record = state.records.get(browser_id)?;
            (
                record.snapshot(browser_id, agent_access_for_record(&state, record)),
                record.owner_id.clone(),
                record.title.clone(),
            )
        };
        if let Err(error) =
            self.sessions
                .commit(&owner_id, browser_id, workspace_id, &url, title.as_deref())
        {
            log_recovery_persistence_error(error);
        }
        Some(snapshot)
    }

    pub(crate) fn title_changed(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        url: Option<String>,
        title: String,
    ) -> Option<BrowserSnapshot> {
        let (snapshot, owner_id, title) = {
            let mut state = self.lock();
            {
                let record = state.records.get_mut(browser_id)?;
                if record.workspace_id != workspace_id
                    || record.instance_id != instance_id
                    || record.resetting
                    || url
                        .as_ref()
                        .is_some_and(|url| record.url.as_ref() != Some(url))
                {
                    return None;
                }
                record.title = Some(title);
            }
            let record = state.records.get(browser_id)?;
            (
                record.snapshot(browser_id, agent_access_for_record(&state, record)),
                record.owner_id.clone(),
                record.title.clone(),
            )
        };
        if let Some(url) = url {
            if let Err(error) = self.sessions.update_title(
                &owner_id,
                browser_id,
                workspace_id,
                &url,
                title.as_deref(),
            ) {
                log_recovery_persistence_error(error);
            }
        }
        Some(snapshot)
    }

    pub(crate) fn remove(&self, browser_id: &str, workspace_id: &str) -> Result<bool, String> {
        let mut state = self.lock();
        if let Some(record) = state.records.get(browser_id) {
            ensure_workspace(browser_id, workspace_id, record)?;
        }
        Ok(state.records.remove(browser_id).is_some())
    }

    pub(crate) fn remove_instance(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
    ) -> bool {
        let mut state = self.lock();
        let matches = state.records.get(browser_id).is_some_and(|record| {
            record.workspace_id == workspace_id && record.instance_id == instance_id
        });
        if matches {
            state.records.remove(browser_id);
        }
        matches
    }

    /// Store a snapshot against the current document epoch with basic validation.
    /// Prefer [`complete_semantic_snapshot`] for agent-driven workflows; this
    /// method skips instance-id, halt, and grant checks needed by the agent path.
    pub(crate) fn record_semantic_snapshot(
        &self,
        browser_id: &str,
        workspace_id: &str,
        document_epoch: u64,
        snapshot_id: String,
        targets: HashMap<String, BrowserTargetRecord>,
    ) -> Result<(), BrowserTargetError> {
        let mut state = self.lock();
        let Some(record) = state.records.get_mut(browser_id) else {
            return Err(BrowserTargetError::StaleTarget);
        };
        if record.workspace_id != workspace_id
            || record.document_epoch != document_epoch
            || record.resetting
        {
            return Err(BrowserTargetError::StaleTarget);
        }
        record.semantic_snapshot = Some(StoredSemanticSnapshot {
            snapshot_id,
            document_epoch,
            targets,
        });
        Ok(())
    }

    /// Store a completed semantic snapshot under one registry lock,
    /// atomically rechecking every authorization that was live when the
    /// observation started: capability, workspace, visibility, halt latch,
    /// controller, origin grant, exact instance identity, and document
    /// epoch/load state.
    ///
    /// The caller must capture `instance_id` via [`observation_fence`]
    /// before the webview JavaScript evaluation and forward it here; a
    /// replaced or recreated browser view cannot pass its stale result
    /// into the live registry.
    #[allow(
        clippy::too_many_arguments,
        reason = "atomic security fence gathers every authorization dimension"
    )]
    pub(crate) fn complete_semantic_snapshot(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        instance_id: u64,
        document_epoch: u64,
        snapshot_id: String,
        targets: HashMap<String, BrowserTargetRecord>,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let workspace_id = active_capability(&state, capability_id)
            .map_err(|_| "browser capability is unavailable".to_owned())?
            .workspace_id
            .clone();
        {
            let Some(record) = state.records.get(browser_id) else {
                return Err("browser session is not registered".to_owned());
            };
            if record.workspace_id != workspace_id
                || record.instance_id != instance_id
                || record.document_epoch != document_epoch
                || record.load_state != BrowserLoadState::Ready
            {
                return Err("browser document changed while it was being inspected".to_owned());
            }
            if !record.visible {
                return Err("browser is hidden".to_owned());
            }
            if *record.dispatch.halt.borrow() {
                return Err("browser control was stopped by the user".to_owned());
            }
            if record.controller.kind != BrowserControllerKind::Agent
                || record.controller_capability_id != Some(capability_id)
            {
                return Err("browser is not controlled by this agent".to_owned());
            }
            let origin = current_origin(record)
                .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
            if !grants_cover(
                &state,
                &workspace_id,
                &origin,
                BrowserGrantCapability::BrowserObserveOrigin,
            ) {
                return Err("browser origin is not shared for this operation".to_owned());
            }
        }
        let record = state
            .records
            .get_mut(browser_id)
            .expect("browser record was validated under the same registry lock");
        record.semantic_snapshot = Some(StoredSemanticSnapshot {
            snapshot_id,
            document_epoch,
            targets,
        });
        Ok(())
    }

    /// Basic epoch-only recording for tests and non-agent pathways.
    /// Prefer [`complete_screenshot_recording`] for agent-driven workflows.
    pub(crate) fn record_screenshot_epoch(
        &self,
        browser_id: &str,
        workspace_id: &str,
        epoch: u64,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let record = state
            .records
            .get_mut(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if record.document_epoch != epoch {
            return Err("browser document changed while screenshot was being captured".to_owned());
        }
        record.screenshot_epoch = Some(epoch);
        Ok(())
    }

    /// Record screenshot completion under one registry lock, atomically
    /// rechecking capability, workspace, visibility, halt, controller,
    /// grant, exact instance, document epoch, and stored snapshot identity
    /// before marking the epoch as captured.
    ///
    /// This replaces the previous three-lock sequence of
    /// `observation_fence` + `validate_snapshot_id` + `record_screenshot_epoch`
    /// which had a TOCTOU gap between each lock acquisition.
    pub(crate) fn complete_screenshot_recording(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        instance_id: u64,
        document_epoch: u64,
        snapshot_id: &str,
    ) -> Result<(), String> {
        let mut state = self.lock();
        let workspace_id = active_capability(&state, capability_id)?
            .workspace_id
            .clone();
        {
            let record = state
                .records
                .get(browser_id)
                .ok_or_else(|| "browser session is not registered".to_owned())?;
            if record.workspace_id != workspace_id {
                return Err("browser session belongs to a different workspace".to_owned());
            }
            if record.instance_id != instance_id || record.document_epoch != document_epoch {
                return Err(
                    "browser document changed while screenshot was being captured".to_owned(),
                );
            }
            if !record.visible {
                return Err("browser is hidden".to_owned());
            }
            if *record.dispatch.halt.borrow() {
                return Err("browser control was stopped by the user".to_owned());
            }
            if record.controller.kind != BrowserControllerKind::Agent
                || record.controller_capability_id != Some(capability_id)
            {
                return Err("browser is not controlled by this agent".to_owned());
            }
            let origin = current_origin(record)
                .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
            if !grants_cover(
                &state,
                &workspace_id,
                &origin,
                BrowserGrantCapability::BrowserObserveOrigin,
            ) {
                return Err("browser origin is not shared for this operation".to_owned());
            }
            let Some(snapshot) = &record.semantic_snapshot else {
                return Err("browser snapshot is stale; take a new browser snapshot".to_owned());
            };
            if snapshot.snapshot_id != snapshot_id || snapshot.document_epoch != document_epoch {
                return Err("browser snapshot is stale; take a new browser snapshot".to_owned());
            }
        }
        let record = state
            .records
            .get_mut(browser_id)
            .expect("browser record was validated under the same registry lock");
        record.screenshot_epoch = Some(document_epoch);
        Ok(())
    }

    /// Re-check, under one registry lock, that the capability, workspace,
    /// visibility, halt latch, controller, and observe grant that authorized
    /// an observation are all still live, and return the record's instance
    /// id and document epoch for fencing.
    ///
    /// The grant is evaluated against the record's current origin, so a
    /// navigation to an unshared origin revokes the fence.
    pub(crate) fn observation_fence(
        &self,
        capability_id: Uuid,
        browser_id: &str,
    ) -> Result<BrowserObservationFence, String> {
        let state = self.lock();
        let capability = active_capability(&state, capability_id)?;
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, &capability.workspace_id, record)?;
        if !record.visible {
            return Err("browser is hidden".to_owned());
        }
        if *record.dispatch.halt.borrow() {
            return Err("browser control was stopped by the user".to_owned());
        }
        if record.controller.kind != BrowserControllerKind::Agent
            || record.controller_capability_id != Some(capability_id)
        {
            return Err("browser is not controlled by this agent".to_owned());
        }
        let origin = current_origin(record)
            .ok_or_else(|| "browser has no authorized HTTP origin".to_owned())?;
        if !grants_cover(
            &state,
            &capability.workspace_id,
            &origin,
            BrowserGrantCapability::BrowserObserveOrigin,
        ) {
            return Err("browser origin is not shared for this operation".to_owned());
        }
        Ok(BrowserObservationFence {
            instance_id: record.instance_id,
            document_epoch: record.document_epoch,
        })
    }

    /// Watch the halt latch so a long-running observation can abort the
    /// moment the user hits Stop, instead of noticing at its next poll.
    pub(crate) fn subscribe_halt(
        &self,
        browser_id: &str,
        workspace_id: &str,
    ) -> Result<watch::Receiver<bool>, String> {
        let state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        Ok(record.dispatch.halt.subscribe())
    }

    /// Confirm that `snapshot_id` names the live stored semantic snapshot at
    /// `document_epoch`. A screenshot must never echo a model-supplied
    /// snapshot id the host did not issue for the current document.
    pub(crate) fn validate_snapshot_id(
        &self,
        browser_id: &str,
        workspace_id: &str,
        snapshot_id: &str,
        document_epoch: u64,
    ) -> Result<(), String> {
        let state = self.lock();
        let record = state
            .records
            .get(browser_id)
            .ok_or_else(|| "browser session is not registered".to_owned())?;
        ensure_workspace(browser_id, workspace_id, record)?;
        if record.document_epoch != document_epoch {
            return Err("browser document changed since the snapshot was taken".to_owned());
        }
        let Some(snapshot) = &record.semantic_snapshot else {
            return Err("browser snapshot is stale; take a new browser snapshot".to_owned());
        };
        if snapshot.snapshot_id != snapshot_id || snapshot.document_epoch != document_epoch {
            return Err("browser snapshot is stale; take a new browser snapshot".to_owned());
        }
        Ok(())
    }

    pub(crate) fn semantic_target(
        &self,
        browser_id: &str,
        workspace_id: &str,
        snapshot_id: &str,
        document_epoch: u64,
        target_ref: &str,
    ) -> Result<BrowserTargetRecord, BrowserTargetError> {
        let state = self.lock();
        let Some(record) = state.records.get(browser_id) else {
            return Err(BrowserTargetError::StaleTarget);
        };
        if record.workspace_id != workspace_id
            || record.document_epoch != document_epoch
            || record.resetting
            || record.load_state != BrowserLoadState::Ready
        {
            return Err(BrowserTargetError::StaleTarget);
        }
        if !record.visible {
            return Err(BrowserTargetError::BrowserHidden);
        }
        let Some(snapshot) = &record.semantic_snapshot else {
            return Err(BrowserTargetError::StaleTarget);
        };
        if snapshot.snapshot_id != snapshot_id || snapshot.document_epoch != document_epoch {
            return Err(BrowserTargetError::StaleTarget);
        }
        snapshot
            .targets
            .get(target_ref)
            .cloned()
            .ok_or(BrowserTargetError::StaleTarget)
    }

    pub(crate) fn invalidate_semantic_snapshot(
        &self,
        browser_id: &str,
        workspace_id: &str,
        snapshot_id: &str,
    ) {
        let mut state = self.lock();
        let Some(record) = state.records.get_mut(browser_id) else {
            return;
        };
        if record.workspace_id == workspace_id
            && !record.resetting
            && record
                .semantic_snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.snapshot_id == snapshot_id)
        {
            record.semantic_snapshot = None;
            record.screenshot_epoch = None;
        }
    }

    fn update_instance(
        &self,
        browser_id: &str,
        workspace_id: &str,
        instance_id: u64,
        update: impl FnOnce(&mut BrowserRecord),
    ) -> Option<BrowserSnapshot> {
        let mut state = self.lock();
        {
            let record = state.records.get_mut(browser_id)?;
            if record.workspace_id != workspace_id
                || record.instance_id != instance_id
                || record.resetting
            {
                return None;
            }
            update(record);
        }
        let record = state.records.get(browser_id)?;
        Some(record.snapshot(browser_id, agent_access_for_record(&state, record)))
    }

    fn lock(&self) -> MutexGuard<'_, BrowserRegistryState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn active_capability(
    state: &BrowserRegistryState,
    capability_id: Uuid,
) -> Result<&BrowserAgentCapability, String> {
    state
        .capabilities
        .get(&capability_id)
        .filter(|capability| capability.expires_at > Instant::now())
        .ok_or_else(|| "browser capability is unavailable".to_owned())
}

fn current_origin(record: &BrowserRecord) -> Option<BrowserOrigin> {
    record.url.as_deref().and_then(BrowserOrigin::from_url)
}

fn share_target_for_record(record: &BrowserRecord) -> Option<BrowserOrigin> {
    record
        .paused_origin
        .clone()
        .or_else(|| current_origin(record))
}

fn grants_cover(
    state: &BrowserRegistryState,
    workspace_id: &str,
    origin: &BrowserOrigin,
    requested: BrowserGrantCapability,
) -> bool {
    state.grants.iter().any(|grant| {
        grant.workspace_id == workspace_id
            && grant.scope.covers(origin)
            && grant
                .capabilities
                .iter()
                .copied()
                .any(|granted| BrowserGrantCapability::implies(granted, requested))
    })
}

fn agent_access_for_record(
    state: &BrowserRegistryState,
    record: &BrowserRecord,
) -> BrowserAgentAccess {
    let origin = share_target_for_record(record);
    let halted = *record.dispatch.halt.borrow();
    let paused = record.paused_origin.is_some()
        || (halted && record.controller.kind == BrowserControllerKind::Agent);
    let Some(origin) = origin else {
        return BrowserAgentAccess {
            shared: false,
            paused,
            halted,
            origin: None,
            scope: None,
            can_observe: false,
            can_control: false,
            can_transfer_files: false,
        };
    };
    let matching: Vec<_> = state
        .grants
        .iter()
        .filter(|grant| grant.workspace_id == record.workspace_id && grant.scope.covers(&origin))
        .collect();
    let covers = |requested| {
        matching.iter().any(|grant| {
            grant
                .capabilities
                .iter()
                .copied()
                .any(|granted| BrowserGrantCapability::implies(granted, requested))
        })
    };
    let can_observe = covers(BrowserGrantCapability::BrowserObserveOrigin);
    let can_control = covers(BrowserGrantCapability::BrowserControlOrigin);
    let can_transfer_files = covers(BrowserGrantCapability::BrowserTransferFiles);
    let scope = matching
        .iter()
        .find(|grant| matches!(grant.scope, BrowserOriginScope::LoopbackWorkspace))
        .or_else(|| matching.first())
        .map(|grant| match grant.scope {
            BrowserOriginScope::Origin { .. } => BrowserAgentAccessScope::Origin,
            BrowserOriginScope::LoopbackWorkspace => BrowserAgentAccessScope::LoopbackWorkspace,
        });
    BrowserAgentAccess {
        shared: can_observe,
        paused,
        halted,
        origin: Some(origin.as_str().to_owned()),
        scope,
        can_observe,
        can_control,
        can_transfer_files,
    }
}

#[allow(clippy::too_many_arguments)]
fn authorize_agent_dispatch(
    state: &mut BrowserRegistryState,
    capability_id: Uuid,
    browser_id: &str,
    origin: &BrowserOrigin,
    required_capability: BrowserGrantCapability,
    action_type: &str,
    target_label: Option<&str>,
    effect: BrowserDispatchEffect,
    confirmation_id: Option<Uuid>,
    consume_confirmation: bool,
) -> Result<BrowserAgentCapability, String> {
    let capability = active_capability(state, capability_id)?.clone();
    let record = state
        .records
        .get(browser_id)
        .ok_or_else(|| "browser session is not registered".to_owned())?;
    ensure_workspace(browser_id, &capability.workspace_id, record)?;
    if current_origin(record).as_ref() != Some(origin) {
        return Err("browser origin changed before dispatch".to_owned());
    }
    if !record.visible {
        return Err("browser is hidden".to_owned());
    }
    if *record.dispatch.halt.borrow() {
        return Err("browser control was stopped by the user".to_owned());
    }
    if record.controller.kind != BrowserControllerKind::Agent
        || record.controller_capability_id != Some(capability_id)
    {
        return Err("browser is not controlled by this agent".to_owned());
    }
    if !grants_cover(state, &capability.workspace_id, origin, required_capability) {
        return Err("browser origin is not shared for this operation".to_owned());
    }

    if effect == BrowserDispatchEffect::Consequential {
        let confirmation_id = confirmation_id
            .ok_or_else(|| "browser action requires native confirmation".to_owned())?;
        let confirmation = state
            .confirmations
            .get(&confirmation_id)
            .filter(|confirmation| confirmation.expires_at > Instant::now())
            .ok_or_else(|| "browser confirmation is unavailable".to_owned())?;
        if confirmation.capability_id != capability_id
            || confirmation.browser_id != browser_id
            || confirmation.workspace_id != capability.workspace_id
            || confirmation.origin != *origin
            || confirmation.action_type != action_type
            || confirmation.target_label.as_deref() != target_label
        {
            return Err("browser confirmation does not match this action".to_owned());
        }
        if consume_confirmation {
            state.confirmations.remove(&confirmation_id);
        }
    }

    if consume_confirmation {
        let record = state
            .records
            .get_mut(browser_id)
            .expect("browser was checked above");
        record.controller.action = Some(clean_controller_text(action_type, 160, "Working"));
        record.controller.takeover_required = false;
    }
    Ok(capability)
}

fn clean_audit_text(value: &str, max_chars: usize, fallback: &str) -> String {
    clean_controller_text(value, max_chars, fallback)
}

fn log_recovery_persistence_error(error: String) {
    eprintln!("tidebreak-desktop: browser recovery state was not persisted: {error}");
}

fn ensure_workspace(
    browser_id: &str,
    workspace_id: &str,
    record: &BrowserRecord,
) -> Result<(), String> {
    if record.workspace_id != workspace_id {
        Err(format!(
            "browser session {browser_id} belongs to a different workspace"
        ))
    } else if record.resetting {
        Err("browser profile is being reset".to_owned())
    } else {
        Ok(())
    }
}

fn clean_controller_text(value: &str, max_chars: usize, fallback: &str) -> String {
    let clean: String = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect();
    if clean.is_empty() {
        fallback.to_owned()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use tokio::sync::oneshot;

    fn target(name: &str) -> BrowserTargetRecord {
        BrowserTargetRecord {
            frame_path: Vec::new(),
            selector: "button:nth-of-type(1)".to_owned(),
            marker: "__marker".to_owned(),
            marker_value: "@e1".to_owned(),
            fingerprint: BrowserTargetFingerprint {
                tag: "button".to_owned(),
                role: "button".to_owned(),
                name: name.to_owned(),
                input_type: None,
                href: None,
                sensitive: false,
            },
            sensitive: false,
            consequential: false,
        }
    }

    fn ready_registry(visible: bool) -> (BrowserRegistry, u64) {
        let registry = BrowserRegistry::default();
        let instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com".to_owned(),
                visible,
            )
            .unwrap();
        registry
            .page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com".to_owned(),
            )
            .unwrap();
        (registry, instance)
    }

    fn controlled_registry() -> (BrowserRegistry, u64, BrowserOrigin, Uuid, tempfile::TempDir) {
        let (registry, instance) = ready_registry(true);
        let private = tempfile::tempdir().unwrap();
        registry.initialize_private_state(private.path()).unwrap();
        let origin = BrowserOrigin::from_url("https://example.com/path").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        let capability = registry.issue_agent_capability("workspace-1", "Code agent");
        registry
            .begin_agent_control(capability, "browser-1")
            .unwrap();
        (registry, instance, origin, capability, private)
    }

    fn force_agent_controller(registry: &BrowserRegistry, browser_id: &str, capability_id: Uuid) {
        let mut state = registry.lock();
        let record = state.records.get_mut(browser_id).unwrap();
        record.dispatch.halt.send_replace(false);
        record.controller = BrowserController {
            kind: BrowserControllerKind::Agent,
            label: Some("Code agent".to_owned()),
            action: None,
            halted: false,
            takeover_required: false,
        };
        record.controller_capability_id = Some(capability_id);
    }

    async fn dispatch_probe(
        registry: BrowserRegistry,
        capability_id: Uuid,
        origin: BrowserOrigin,
        ran: Arc<AtomicBool>,
    ) -> Result<(), String> {
        registry
            .dispatch_agent(
                capability_id,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "click",
                Some("Continue"),
                BrowserDispatchEffect::Mutate,
                None,
                move || async move {
                    ran.store(true, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
    }

    #[test]
    fn browser_identity_cannot_be_rebound_to_another_workspace() {
        let registry = BrowserRegistry::default();
        registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();

        assert!(registry.snapshot("browser-1", "workspace-1").is_ok());
        assert!(registry.snapshot("browser-1", "workspace-2").is_err());
        assert!(registry
            .register(
                "browser-1",
                "workspace-2",
                "https://example.org".to_owned(),
                true,
            )
            .is_err());
    }

    #[test]
    fn restart_recovers_only_the_last_completed_navigation_without_authority() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let registry = BrowserRegistry::default();
        registry.initialize_private_state(private.path()).unwrap();
        let instance = registry
            .register_managed(
                "browser-1",
                "workspace-1",
                owner.clone(),
                Uuid::new_v4().to_string(),
                "https://example.com/committed".to_owned(),
                true,
            )
            .unwrap();
        let ready = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/committed".to_owned(),
            )
            .and_then(|_| {
                registry.page_finished(
                    "browser-1",
                    "workspace-1",
                    instance,
                    "https://example.com/committed".to_owned(),
                )
            })
            .unwrap();
        registry
            .title_changed(
                "browser-1",
                "workspace-1",
                instance,
                Some("https://example.com/committed".to_owned()),
                "Committed page".to_owned(),
            )
            .unwrap();
        let origin = BrowserOrigin::from_url("https://example.com/committed").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        let capability = registry.issue_agent_capability("workspace-1", "Code agent");
        force_agent_controller(&registry, "browser-1", capability);
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                ready.document_epoch.unwrap(),
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/in-flight".to_owned(),
            )
            .unwrap();
        registry
            .title_changed(
                "browser-1",
                "workspace-1",
                instance,
                Some("https://example.com/in-flight".to_owned()),
                "In-flight page".to_owned(),
            )
            .unwrap();
        drop(registry);

        let reopened = BrowserRegistry::default();
        reopened.initialize_private_state(private.path()).unwrap();
        let recovered = reopened
            .recover_session(&owner, "browser-1", "workspace-1")
            .unwrap()
            .unwrap();
        assert_eq!(recovered.url, "https://example.com/committed");
        assert_eq!(recovered.title.as_deref(), Some("Committed page"));

        reopened
            .register_managed_with_title(
                "browser-1",
                "workspace-1",
                ManagedBrowserRegistration {
                    owner_id: owner,
                    profile_id: Uuid::new_v4().to_string(),
                    url: recovered.url,
                    title: recovered.title,
                    visible: true,
                },
            )
            .unwrap();
        let snapshot = reopened.snapshot("browser-1", "workspace-1").unwrap();
        assert_eq!(snapshot.document_epoch, Some(0));
        assert_eq!(
            snapshot.controller.unwrap().kind,
            BrowserControllerKind::Human
        );
        let access = snapshot.agent_access.unwrap();
        assert!(!access.shared);
        assert!(!access.can_observe);
        assert!(!access.can_control);
        let state = reopened.lock();
        let record = state.records.get("browser-1").unwrap();
        assert!(record.controller_capability_id.is_none());
        assert!(record.semantic_snapshot.is_none());
        assert!(state.capabilities.is_empty());
        assert!(state.grants.is_empty());
    }

    #[test]
    fn same_document_navigation_becomes_the_recovery_url() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let registry = BrowserRegistry::default();
        registry.initialize_private_state(private.path()).unwrap();
        let instance = registry
            .register_managed(
                "browser-1",
                "workspace-1",
                owner.clone(),
                Uuid::new_v4().to_string(),
                "https://example.com/start".to_owned(),
                true,
            )
            .unwrap();
        let ready = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/start".to_owned(),
            )
            .and_then(|_| {
                registry.page_finished(
                    "browser-1",
                    "workspace-1",
                    instance,
                    "https://example.com/start".to_owned(),
                )
            })
            .unwrap();
        registry
            .same_document_navigation(
                "browser-1",
                "workspace-1",
                instance,
                ready.document_epoch.unwrap(),
                "https://example.com/start#details".to_owned(),
            )
            .unwrap();
        drop(registry);

        let reopened = BrowserRegistry::default();
        reopened.initialize_private_state(private.path()).unwrap();
        assert_eq!(
            reopened
                .recover_session(&owner, "browser-1", "workspace-1")
                .unwrap()
                .unwrap()
                .url,
            "https://example.com/start#details"
        );
    }

    #[test]
    fn explicit_close_forgets_only_the_exact_recovery_binding() {
        let private = tempfile::tempdir().unwrap();
        let owner = OwnerId::local();
        let registry = BrowserRegistry::default();
        registry.initialize_private_state(private.path()).unwrap();
        let instance = registry
            .register_managed(
                "browser-1",
                "workspace-1",
                owner.clone(),
                Uuid::new_v4().to_string(),
                "https://example.com/".to_owned(),
                true,
            )
            .unwrap();
        registry
            .page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/".to_owned(),
            )
            .unwrap();

        assert!(registry
            .recover_session(&owner, "browser-1", "workspace-2")
            .is_err());
        assert!(registry
            .forget_recovery(&owner, "browser-1", "workspace-2")
            .is_err());
        registry.remove("browser-1", "workspace-1").unwrap();
        registry
            .forget_recovery(&owner, "browser-1", "workspace-1")
            .unwrap();
        drop(registry);

        let reopened = BrowserRegistry::default();
        reopened.initialize_private_state(private.path()).unwrap();
        assert!(reopened
            .recover_session(&owner, "browser-1", "workspace-1")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn profile_reset_drains_and_removes_only_matching_native_sessions() {
        let registry = BrowserRegistry::default();
        let owner = OwnerId::local();
        let other_owner = OwnerId::new("other-owner").unwrap();
        let profile_id = Uuid::new_v4().to_string();
        registry
            .register_managed(
                "browser-1",
                "workspace-1",
                owner.clone(),
                profile_id.clone(),
                "https://example.com/one".to_owned(),
                true,
            )
            .unwrap();
        registry
            .register_managed(
                "browser-2",
                "workspace-2",
                owner.clone(),
                profile_id.clone(),
                "https://example.org/two".to_owned(),
                true,
            )
            .unwrap();
        registry
            .register_managed(
                "other-owner",
                "workspace-3",
                other_owner,
                profile_id.clone(),
                "https://owner.example/".to_owned(),
                true,
            )
            .unwrap();
        registry
            .register_managed(
                "other-profile",
                "workspace-1",
                owner.clone(),
                Uuid::new_v4().to_string(),
                "https://unrelated.example/".to_owned(),
                true,
            )
            .unwrap();
        let origin = BrowserOrigin::from_url("https://example.com/one").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserObserveOrigin],
            )
            .unwrap();

        let reset = registry
            .begin_profile_reset("browser-1", "workspace-1")
            .await
            .unwrap();

        assert_eq!(
            reset
                .sessions()
                .iter()
                .map(|session| (session.browser_id.as_str(), session.workspace_id.as_str()))
                .collect::<Vec<_>>(),
            [("browser-1", "workspace-1"), ("browser-2", "workspace-2")]
        );
        assert_eq!(reset.profile_id(), profile_id);
        assert_eq!(
            registry.snapshot("browser-1", "workspace-1").unwrap_err(),
            "browser profile is being reset"
        );

        reset.finish();

        assert!(registry.snapshot("browser-1", "workspace-1").is_err());
        assert!(registry.snapshot("browser-2", "workspace-2").is_err());
        assert!(registry.snapshot("other-owner", "workspace-3").is_ok());
        assert!(registry.snapshot("other-profile", "workspace-1").is_ok());

        registry
            .register_managed(
                "fresh-browser",
                "workspace-1",
                owner,
                Uuid::new_v4().to_string(),
                "https://example.com/fresh".to_owned(),
                true,
            )
            .unwrap();
        assert!(
            registry
                .snapshot("fresh-browser", "workspace-1")
                .unwrap()
                .agent_access
                .unwrap()
                .can_observe
        );
    }

    #[tokio::test]
    async fn aborted_profile_reset_restores_the_previous_dispatch_latch() {
        let (registry, _) = ready_registry(true);
        {
            let reset = registry
                .begin_profile_reset("browser-1", "workspace-1")
                .await
                .unwrap();
            assert_eq!(reset.sessions().len(), 1);
        }

        assert!(registry.snapshot("browser-1", "workspace-1").is_ok());
        assert!(!*registry
            .lock()
            .records
            .get("browser-1")
            .unwrap()
            .dispatch
            .halt
            .borrow());
    }
    #[test]
    fn model_facing_lists_are_workspace_scoped_and_stably_ordered() {
        let registry = BrowserRegistry::default();
        registry
            .register(
                "browser-b",
                "workspace-1",
                "https://example.com/b".to_owned(),
                true,
            )
            .unwrap();
        registry
            .register(
                "browser-a",
                "workspace-1",
                "https://example.com/a".to_owned(),
                false,
            )
            .unwrap();
        registry
            .register(
                "browser-secret",
                "workspace-2",
                "https://private.example.com".to_owned(),
                true,
            )
            .unwrap();

        let sessions = registry.list_for_workspace("workspace-1");
        assert_eq!(
            sessions
                .iter()
                .map(|session| session.browser_id.as_str())
                .collect::<Vec<_>>(),
            ["browser-a", "browser-b"]
        );
        assert!(sessions
            .iter()
            .all(|session| !session.browser_id.contains("secret")));
        assert_eq!(registry.list_for_workspace("workspace-missing"), []);
    }

    #[test]
    fn document_epoch_advances_on_each_started_document() {
        let registry = BrowserRegistry::default();
        let instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com".to_owned(),
                true,
            )
            .unwrap();

        let first = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com".to_owned(),
            )
            .unwrap();
        let second = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/next".to_owned(),
            )
            .unwrap();

        assert_eq!(first.document_epoch, Some(1));
        assert_eq!(first.load_state, Some(BrowserLoadState::Loading));
        assert_eq!(second.document_epoch, Some(2));
    }

    #[test]
    fn same_document_navigation_updates_every_registry_projection_without_advancing_epoch() {
        let registry = BrowserRegistry::default();
        let instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com/".to_owned(),
                true,
            )
            .unwrap();
        let ready = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/".to_owned(),
            )
            .and_then(|_| {
                registry.page_finished(
                    "browser-1",
                    "workspace-1",
                    instance,
                    "https://example.com/".to_owned(),
                )
            })
            .unwrap();
        let epoch = ready.document_epoch.unwrap();
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                epoch,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();
        registry
            .record_screenshot_epoch("browser-1", "workspace-1", epoch)
            .unwrap();

        for url in [
            "https://example.com/?view=details",
            "https://example.com/?view=replaced",
            "https://example.com/?view=replaced#summary",
            "https://example.com/?view=details",
            "https://example.com/?view=replaced#summary",
        ] {
            let snapshot = registry
                .same_document_navigation(
                    "browser-1",
                    "workspace-1",
                    instance,
                    epoch,
                    url.to_owned(),
                )
                .unwrap();
            assert_eq!(snapshot.url.as_deref(), Some(url));
            assert_eq!(snapshot.document_epoch, Some(epoch));
            assert_eq!(snapshot.load_state, Some(BrowserLoadState::Ready));
            assert_eq!(
                registry.list_for_workspace("workspace-1")[0].url.as_deref(),
                Some(url)
            );
            registry
                .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", epoch)
                .expect("same-document navigation keeps semantic targets live");
            assert_eq!(
                registry
                    .lock()
                    .records
                    .get("browser-1")
                    .and_then(|record| record.screenshot_epoch),
                Some(epoch)
            );
        }

        registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/full-navigation".to_owned(),
            )
            .unwrap();
        assert!(registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", epoch)
            .is_err());
        assert_eq!(
            registry
                .lock()
                .records
                .get("browser-1")
                .and_then(|record| record.screenshot_epoch),
            None
        );
    }

    #[test]
    fn stale_same_document_observers_cannot_update_a_later_document() {
        let (registry, instance) = ready_registry(true);
        let epoch = registry
            .snapshot("browser-1", "workspace-1")
            .unwrap()
            .document_epoch
            .unwrap();
        registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/full-navigation".to_owned(),
            )
            .unwrap();

        assert!(registry
            .same_document_navigation(
                "browser-1",
                "workspace-1",
                instance,
                epoch,
                "https://example.com/stale".to_owned(),
            )
            .is_none());
        assert_eq!(
            registry
                .snapshot("browser-1", "workspace-1")
                .unwrap()
                .url
                .as_deref(),
            Some("https://example.com/full-navigation")
        );
    }

    #[test]
    fn callbacks_from_a_replaced_native_view_cannot_mutate_the_new_record() {
        let registry = BrowserRegistry::default();
        let old_instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://old.example".to_owned(),
                true,
            )
            .unwrap();
        assert!(registry.remove("browser-1", "workspace-1").unwrap());
        let new_instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://new.example".to_owned(),
                true,
            )
            .unwrap();

        assert_ne!(old_instance, new_instance);
        assert!(registry
            .page_finished(
                "browser-1",
                "workspace-1",
                old_instance,
                "https://old.example/late".to_owned(),
            )
            .is_none());
        assert_eq!(
            registry
                .snapshot("browser-1", "workspace-1")
                .unwrap()
                .url
                .as_deref(),
            Some("https://new.example")
        );
    }

    #[test]
    fn close_then_recreate_gets_a_fresh_native_instance() {
        let registry = BrowserRegistry::default();
        let first = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com/one".to_owned(),
                false,
            )
            .unwrap();
        assert!(registry.remove("browser-1", "workspace-1").unwrap());
        assert!(registry.snapshot("browser-1", "workspace-1").is_err());

        let second = registry
            .register(
                "browser-1",
                "workspace-1",
                "https://example.com/two".to_owned(),
                true,
            )
            .unwrap();
        assert!(second > first);
        let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
        assert_eq!(snapshot.url.as_deref(), Some("https://example.com/two"));
        assert_eq!(snapshot.visible, Some(true));
    }

    #[test]
    fn public_grants_cover_only_the_exact_normalized_origin() {
        let (registry, instance) = ready_registry(true);
        let origin = BrowserOrigin::from_url("https://example.com/private?token=secret").unwrap();
        let shared = registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap()
            .agent_access
            .unwrap();
        assert!(shared.shared);
        assert!(shared.can_observe);
        assert!(shared.can_control);
        assert!(!shared.can_transfer_files);
        assert_eq!(shared.origin.as_deref(), Some("https://example.com"));
        assert_eq!(shared.scope, Some(BrowserAgentAccessScope::Origin));

        let same_origin = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/another/path".to_owned(),
            )
            .unwrap()
            .agent_access
            .unwrap();
        assert!(same_origin.shared);

        let other_port = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com:444/another/path".to_owned(),
            )
            .unwrap()
            .agent_access
            .unwrap();
        assert!(!other_port.shared);
        assert!(!other_port.can_observe);
        assert_eq!(other_port.scope, None);
    }

    #[test]
    fn loopback_workspace_grants_follow_local_port_changes_only() {
        let registry = BrowserRegistry::default();
        let instance = registry
            .register(
                "browser-1",
                "workspace-1",
                "http://localhost:3000".to_owned(),
                true,
            )
            .unwrap();
        registry
            .page_finished(
                "browser-1",
                "workspace-1",
                instance,
                "http://localhost:3000".to_owned(),
            )
            .unwrap();
        let origin = BrowserOrigin::from_url("http://localhost:3000").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::LoopbackWorkspace,
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();

        let another_local_origin = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "http://127.0.0.1:4317/review".to_owned(),
            )
            .unwrap()
            .agent_access
            .unwrap();
        assert!(another_local_origin.shared);
        assert!(another_local_origin.can_control);
        assert_eq!(
            another_local_origin.scope,
            Some(BrowserAgentAccessScope::LoopbackWorkspace)
        );

        let public_origin = registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com".to_owned(),
            )
            .unwrap()
            .agent_access
            .unwrap();
        assert!(!public_origin.shared);
        assert_eq!(public_origin.scope, None);
    }

    #[tokio::test]
    async fn cross_workspace_capabilities_fail_before_engine_dispatch() {
        let (registry, _) = ready_registry(true);
        let origin = BrowserOrigin::from_url("https://example.com").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        let capability = registry.issue_agent_capability("workspace-2", "Other agent");
        force_agent_controller(&registry, "browser-1", capability);
        let ran = Arc::new(AtomicBool::new(false));

        let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
            .await
            .unwrap_err();
        assert!(error.contains("different workspace"));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn expired_and_revoked_capabilities_fail_before_engine_dispatch() {
        let (expired_registry, _) = ready_registry(true);
        let origin = BrowserOrigin::from_url("https://example.com").unwrap();
        let expired = expired_registry.issue_agent_capability_for(
            "workspace-1",
            "Expired agent",
            Duration::ZERO,
        );
        force_agent_controller(&expired_registry, "browser-1", expired);
        let expired_ran = Arc::new(AtomicBool::new(false));
        let error = dispatch_probe(
            expired_registry,
            expired,
            origin.clone(),
            Arc::clone(&expired_ran),
        )
        .await
        .unwrap_err();
        assert!(error.contains("capability is unavailable"));
        assert!(!expired_ran.load(Ordering::SeqCst));

        let (revoked_registry, _, _, revoked, _private) = controlled_registry();
        revoked_registry.revoke_agent_capability(revoked);
        let revoked_ran = Arc::new(AtomicBool::new(false));
        let error = dispatch_probe(revoked_registry, revoked, origin, Arc::clone(&revoked_ran))
            .await
            .unwrap_err();
        assert!(error.contains("capability is unavailable"));
        assert!(!revoked_ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn hidden_browsers_fail_before_engine_dispatch() {
        let (registry, _, origin, capability, _private) = controlled_registry();
        registry
            .set_visible("browser-1", "workspace-1", false)
            .unwrap();
        let ran = Arc::new(AtomicBool::new(false));

        let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
            .await
            .unwrap_err();
        assert_eq!(error, "browser is hidden");
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn revoking_origin_access_returns_the_tab_to_unshared_human_control() {
        let (registry, _, origin, capability, _private) = controlled_registry();
        let revoked = registry
            .revoke_browser_access("browser-1", "workspace-1")
            .unwrap();
        assert_eq!(
            revoked.controller.unwrap().kind,
            BrowserControllerKind::Human
        );
        let access = revoked.agent_access.unwrap();
        assert!(!access.shared);
        assert!(!access.paused);
        assert!(access.halted);

        let ran = Arc::new(AtomicBool::new(false));
        let error = dispatch_probe(registry, capability, origin, Arc::clone(&ran))
            .await
            .unwrap_err();
        assert!(error.contains("stopped by the user"));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn semantic_targets_are_scoped_to_the_exact_snapshot_and_epoch() {
        let (registry, instance) = ready_registry(true);
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        assert!(registry
            .semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",)
            .is_ok());
        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-other", 0, "@e1",),
            Err(BrowserTargetError::StaleTarget)
        );
        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 1, "@e1",),
            Err(BrowserTargetError::StaleTarget)
        );

        registry
            .page_started(
                "browser-1",
                "workspace-1",
                instance,
                "https://example.com/next".to_owned(),
            )
            .unwrap();
        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
            Err(BrowserTargetError::StaleTarget)
        );
    }

    #[test]
    fn missing_refs_never_fall_back_to_another_target() {
        let (registry, _) = ready_registry(true);
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e404",),
            Err(BrowserTargetError::StaleTarget)
        );
    }

    #[test]
    fn hidden_or_revealed_browsers_require_a_fresh_snapshot() {
        let (registry, _) = ready_registry(true);
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        registry
            .set_visible("browser-1", "workspace-1", false)
            .unwrap();
        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
            Err(BrowserTargetError::BrowserHidden)
        );

        registry
            .set_visible("browser-1", "workspace-1", true)
            .unwrap();
        assert_eq!(
            registry.semantic_target("browser-1", "workspace-1", "snapshot-1", 0, "@e1",),
            Err(BrowserTargetError::StaleTarget)
        );
    }

    #[test]
    fn platform_claims_only_implemented_agent_capabilities() {
        let descriptor = platform_default_engine();
        assert_eq!(
            descriptor.capabilities.semantic_actions,
            cfg!(target_os = "macos")
        );
        assert!(!descriptor.capabilities.screenshot);
        assert_eq!(
            descriptor.capabilities.profile_reset,
            cfg!(target_os = "macos")
        );
    }

    #[tokio::test]
    async fn stop_latches_before_in_flight_dispatch_drains_and_rejects_queued_work() {
        let (registry, _, origin, capability, _private) = controlled_registry();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let first_registry = registry.clone();
        let first_origin = origin.clone();
        let first = tokio::spawn(async move {
            first_registry
                .dispatch_agent(
                    capability,
                    "browser-1",
                    &first_origin,
                    BrowserGrantCapability::BrowserControlOrigin,
                    "click",
                    Some("First action"),
                    BrowserDispatchEffect::Mutate,
                    None,
                    move || async move {
                        let _ = entered_tx.send(());
                        release_rx
                            .await
                            .map_err(|_| "test dispatch release was dropped".to_owned())?;
                        Ok(())
                    },
                )
                .await
        });
        entered_rx.await.unwrap();

        let stop_registry = registry.clone();
        let stop = tokio::spawn(async move {
            stop_registry
                .stop_agent_control("browser-1", "workspace-1")
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
                if snapshot.agent_access.unwrap().halted {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Stop should publish its latch before the active dispatch returns");
        assert!(!stop.is_finished());

        let queued_ran = Arc::new(AtomicBool::new(false));
        let queued = tokio::spawn(dispatch_probe(
            registry.clone(),
            capability,
            origin,
            Arc::clone(&queued_ran),
        ));
        release_tx.send(()).unwrap();

        first.await.unwrap().unwrap();
        let queued_error = queued.await.unwrap().unwrap_err();
        assert!(queued_error.contains("stopped by the user"));
        assert!(!queued_ran.load(Ordering::SeqCst));
        let stopped = stop.await.unwrap().unwrap();
        assert!(stopped.controller.unwrap().halted);
    }

    #[tokio::test]
    async fn human_takeover_wins_over_queued_agent_input() {
        let (registry, _, origin, capability, _private) = controlled_registry();
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let first_registry = registry.clone();
        let first_origin = origin.clone();
        let first = tokio::spawn(async move {
            first_registry
                .dispatch_agent(
                    capability,
                    "browser-1",
                    &first_origin,
                    BrowserGrantCapability::BrowserControlOrigin,
                    "click",
                    Some("First action"),
                    BrowserDispatchEffect::Mutate,
                    None,
                    move || async move {
                        let _ = entered_tx.send(());
                        release_rx
                            .await
                            .map_err(|_| "test dispatch release was dropped".to_owned())?;
                        Ok(())
                    },
                )
                .await
        });
        entered_rx.await.unwrap();

        let takeover_registry = registry.clone();
        let takeover = tokio::spawn(async move {
            takeover_registry
                .take_human_control("browser-1", "workspace-1")
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = registry.snapshot("browser-1", "workspace-1").unwrap();
                if snapshot.controller.unwrap().kind == BrowserControllerKind::Human {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("takeover should transfer ownership before the active dispatch returns");
        assert!(!takeover.is_finished());

        let queued_ran = Arc::new(AtomicBool::new(false));
        let queued = tokio::spawn(dispatch_probe(
            registry.clone(),
            capability,
            origin,
            Arc::clone(&queued_ran),
        ));
        release_tx.send(()).unwrap();

        first.await.unwrap().unwrap();
        let queued_error = queued.await.unwrap().unwrap_err();
        assert!(
            queued_error.contains("stopped by the user")
                || queued_error.contains("not controlled by this agent")
        );
        assert!(!queued_ran.load(Ordering::SeqCst));
        let human = takeover.await.unwrap().unwrap();
        assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
    }

    #[tokio::test]
    async fn native_confirmations_are_exact_expiring_and_single_use() {
        let (registry, _, origin, capability, _private) = controlled_registry();
        let confirmation = registry
            .record_native_confirmation(
                capability,
                "browser-1",
                &origin,
                "submit_form",
                Some("Create deployment"),
            )
            .unwrap();
        {
            let state = registry.lock();
            let record = state.confirmations.get(&confirmation).unwrap();
            assert_eq!(record.capability_id, capability);
            assert_eq!(record.browser_id, "browser-1");
            assert_eq!(record.workspace_id, "workspace-1");
            assert_eq!(record.origin, origin);
            assert_eq!(record.action_type, "submit_form");
            assert_eq!(record.target_label.as_deref(), Some("Create deployment"));
        }

        let missing_ran = Arc::new(AtomicBool::new(false));
        let error = registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "submit_form",
                Some("Create deployment"),
                BrowserDispatchEffect::Consequential,
                None,
                {
                    let missing_ran = Arc::clone(&missing_ran);
                    move || async move {
                        missing_ran.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await
            .unwrap_err();
        assert!(error.contains("requires native confirmation"));
        assert!(!missing_ran.load(Ordering::SeqCst));

        let mismatch_ran = Arc::new(AtomicBool::new(false));
        let error = registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "submit_form",
                Some("Delete deployment"),
                BrowserDispatchEffect::Consequential,
                Some(confirmation),
                {
                    let mismatch_ran = Arc::clone(&mismatch_ran);
                    move || async move {
                        mismatch_ran.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await
            .unwrap_err();
        assert!(error.contains("does not match"));
        assert!(!mismatch_ran.load(Ordering::SeqCst));

        registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "submit_form",
                Some("Create deployment"),
                BrowserDispatchEffect::Consequential,
                Some(confirmation),
                || async { Ok(()) },
            )
            .await
            .unwrap();
        let error = registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "submit_form",
                Some("Create deployment"),
                BrowserDispatchEffect::Consequential,
                Some(confirmation),
                || async { Ok(()) },
            )
            .await
            .unwrap_err();
        assert!(error.contains("confirmation is unavailable"));

        let expired = registry
            .record_native_confirmation_for(
                capability,
                "browser-1",
                &origin,
                "submit_form",
                Some("Create deployment"),
                Duration::ZERO,
            )
            .unwrap();
        let error = registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "submit_form",
                Some("Create deployment"),
                BrowserDispatchEffect::Consequential,
                Some(expired),
                || async { Ok(()) },
            )
            .await
            .unwrap_err();
        assert!(error.contains("confirmation is unavailable"));
    }

    #[tokio::test]
    async fn audit_intent_is_durable_before_dispatch_and_excludes_sensitive_data() {
        const ENTERED_TEXT: &str = "do-not-store-this-password";
        const FULL_URL: &str = "https://example.com/private/report?token=do-not-store";
        const PAGE_CONTENT: &str = "untrusted page content must not enter the audit";

        let (registry, _, origin, capability, private) = controlled_registry();
        let audit_path = private.path().join(BROWSER_AUDIT_FILE);
        let dispatch_audit_path = audit_path.clone();
        registry
            .dispatch_agent(
                capability,
                "browser-1",
                &origin,
                BrowserGrantCapability::BrowserControlOrigin,
                "type",
                Some("Email address"),
                BrowserDispatchEffect::Mutate,
                None,
                move || async move {
                    let audit = std::fs::read_to_string(&dispatch_audit_path).unwrap();
                    let lines = audit.lines().collect::<Vec<_>>();
                    assert_eq!(lines.len(), 1);
                    let intent: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
                    assert_eq!(intent["phase"], "intent");
                    assert_eq!(intent["outcome"], "pending");
                    assert_eq!(intent["origin"], "https://example.com");
                    assert_eq!(intent["actionType"], "type");
                    assert_eq!(intent["semanticTargetLabel"], "Email address");
                    let _engine_only = (ENTERED_TEXT, FULL_URL, PAGE_CONTENT);
                    Ok(())
                },
            )
            .await
            .unwrap();

        let audit = std::fs::read_to_string(audit_path).unwrap();
        let events = audit
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["phase"], "intent");
        assert_eq!(events[1]["phase"], "outcome");
        assert_eq!(events[1]["outcome"], "succeeded");
        assert_eq!(events[0]["eventId"], events[1]["eventId"]);
        assert!(!audit.contains(ENTERED_TEXT));
        assert!(!audit.contains(FULL_URL));
        assert!(!audit.contains(PAGE_CONTENT));
    }

    #[tokio::test]
    async fn stop_and_takeover_survive_broken_audit_storage() {
        let (registry, _, origin, capability, private) = controlled_registry();
        let audit_path = private.path().join(BROWSER_AUDIT_FILE);
        std::fs::create_dir(&audit_path).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let error = dispatch_probe(registry.clone(), capability, origin, Arc::clone(&ran))
            .await
            .unwrap_err();
        assert!(error.contains("audit storage is unavailable"));
        assert!(!ran.load(Ordering::SeqCst));

        let stopped = registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();
        assert!(stopped.controller.unwrap().halted);
        let human = registry
            .take_human_control("browser-1", "workspace-1")
            .await
            .unwrap();
        assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
    }

    #[test]
    fn cross_origin_redirects_pause_before_the_destination_is_exposed() {
        let (registry, instance, _origin, _capability, _private) = controlled_registry();
        let destination_url = "https://accounts.example.org/login?continue=%2Fsettings";
        let destination = BrowserOrigin::from_url("https://accounts.example.org/login").unwrap();

        let decision = registry.authorize_navigation(
            "browser-1",
            "workspace-1",
            instance,
            destination_url,
            &destination,
        );
        let BrowserNavigationDecision::Pause {
            origin: paused_origin,
            snapshot,
        } = decision
        else {
            panic!("ungranted redirect should pause");
        };
        assert_eq!(paused_origin, "https://accounts.example.org");
        assert_eq!(snapshot.url.as_deref(), Some("https://example.com"));
        let access = snapshot.agent_access.unwrap();
        assert!(access.paused);
        assert!(access.halted);
        assert!(!access.shared);
        assert_eq!(
            access.origin.as_deref(),
            Some("https://accounts.example.org")
        );
        assert_eq!(
            registry
                .share_target_origin("browser-1", "workspace-1")
                .unwrap(),
            destination
        );

        let approved = registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &destination,
                BrowserOriginScope::Origin {
                    origin: destination.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        assert!(!approved.agent_access.unwrap().paused);
        assert_eq!(
            registry
                .take_pending_navigation("browser-1", "workspace-1")
                .unwrap()
                .as_deref(),
            Some(destination_url)
        );
        assert!(registry
            .take_pending_navigation("browser-1", "workspace-1")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn stop_and_takeover_make_agent_control_legible_and_recoverable() {
        let (registry, _) = ready_registry(true);
        let origin = BrowserOrigin::from_url("https://example.com").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        let capability = registry.issue_agent_capability("workspace-1", "  Code   agent  ");
        let claimed = registry
            .begin_agent_control(capability, "browser-1")
            .unwrap();
        let controller = claimed.controller.unwrap();
        assert_eq!(controller.kind, BrowserControllerKind::Agent);
        assert_eq!(controller.label.as_deref(), Some("Code agent"));
        assert!(!controller.halted);

        let active = registry
            .set_agent_action(capability, "browser-1", Some("Clicking Continue"), false)
            .unwrap();
        assert_eq!(
            active.controller.unwrap().action.as_deref(),
            Some("Clicking Continue")
        );

        let stopped = registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();
        let controller = stopped.controller.unwrap();
        assert_eq!(controller.kind, BrowserControllerKind::Agent);
        assert!(controller.halted);
        assert!(controller.action.is_none());

        let human = registry
            .take_human_control("browser-1", "workspace-1")
            .await
            .unwrap();
        assert_eq!(human.controller.unwrap().kind, BrowserControllerKind::Human);
        assert!(!human.agent_access.unwrap().halted);

        let next_capability = registry.issue_agent_capability("workspace-1", "Next agent");
        let reclaimed = registry
            .begin_agent_control(next_capability, "browser-1")
            .unwrap();
        assert_eq!(
            reclaimed.controller.unwrap().label.as_deref(),
            Some("Next agent")
        );
    }

    #[test]
    fn screenshot_snapshot_ids_are_validated_against_the_stored_snapshot() {
        let (registry, _) = ready_registry(true);
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 0)
            .expect("the live snapshot id validates");
        assert!(registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-forged", 0)
            .is_err());
        assert!(registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 1)
            .is_err());
        assert!(registry
            .validate_snapshot_id("browser-1", "other-workspace", "snapshot-1", 0)
            .is_err());
    }

    // ── begin_agent_observation tests ─────────────────────────────

    #[test]
    fn begin_agent_observation_preserves_stored_snapshot_for_same_capability() {
        let (registry, _, _origin, capability, _private) = controlled_registry();
        // First acquire control via begin_agent_control (clears snapshot).
        let _ = registry
            .begin_agent_control(capability, "browser-1")
            .unwrap();
        // Then record a snapshot — this is what a real snapshot op does.
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::from([("@e1".to_owned(), target("Continue"))]),
            )
            .unwrap();

        // begin_agent_observation must NOT clear the stored snapshot.
        let _snap = registry
            .begin_agent_observation(capability, "browser-1")
            .unwrap();

        registry
            .validate_snapshot_id("browser-1", "workspace-1", "snapshot-1", 0)
            .expect("snapshot must survive begin_agent_observation");
    }

    #[test]
    fn begin_agent_observation_refuses_wrong_capability() {
        let (registry, _, _origin, capability, _private) = controlled_registry();
        let other = registry.issue_agent_capability("workspace-1", "Other");

        let result = registry.begin_agent_observation(other, "browser-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not controlled by this agent"));
        assert!(registry
            .begin_agent_observation(capability, "browser-1")
            .is_ok());
    }

    #[tokio::test]
    async fn begin_agent_observation_refuses_human_takeover() {
        let (registry, _, _origin, capability, _private) = controlled_registry();
        // Human takes over — clears the agent controller.
        registry
            .take_human_control("browser-1", "workspace-1")
            .await
            .unwrap();

        // Observation must refuse: a human currently holds the browser.
        let result = registry.begin_agent_observation(capability, "browser-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not controlled by this agent"));
    }

    #[test]
    fn begin_agent_observation_refuses_no_prior_control() {
        // ready_registry() registers a browser but issues no capability
        // and calls no begin_agent_control — the controller is Human.
        let (registry, _private) = ready_registry(true);
        let origin = BrowserOrigin::from_url("https://example.com").unwrap();
        registry
            .grant_browser_access(
                "browser-1",
                "workspace-1",
                &origin,
                BrowserOriginScope::Origin {
                    origin: origin.clone(),
                },
                &[BrowserGrantCapability::BrowserControlOrigin],
            )
            .unwrap();
        let capability = registry.issue_agent_capability("workspace-1", "Code agent");
        let result = registry.begin_agent_observation(capability, "browser-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not controlled by this agent"));
    }

    #[tokio::test]
    async fn begin_agent_observation_refuses_after_stop() {
        let (registry, _, _origin, capability, _private) = controlled_registry();
        let _ = registry
            .begin_agent_control(capability, "browser-1")
            .unwrap();
        registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();
        let result = registry.begin_agent_observation(capability, "browser-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("stopped"));
    }

    #[test]
    fn begin_agent_observation_same_capability_continuation_preserves_snapshot() {
        let (registry, _, _origin, capability, _private) = controlled_registry();
        let _ = registry
            .begin_agent_control(capability, "browser-1")
            .unwrap();
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snap-2".to_owned(),
                HashMap::from([("@btn".to_owned(), target("Submit"))]),
            )
            .unwrap();

        // Same capability, observation continues.
        let snap = registry
            .begin_agent_observation(capability, "browser-1")
            .unwrap();
        assert_eq!(snap.controller.unwrap().kind, BrowserControllerKind::Agent);

        // Snapshot still validates.
        registry
            .validate_snapshot_id("browser-1", "workspace-1", "snap-2", 0)
            .expect("snapshot must persist across observation continuation");
    }

    #[tokio::test]
    async fn observation_fence_and_halt_watch_fail_closed_on_stop() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();

        let fence = registry.observation_fence(capability, "browser-1").unwrap();
        assert_eq!(fence.instance_id, instance);
        assert_eq!(fence.document_epoch, 0);

        let mut halt = registry.subscribe_halt("browser-1", "workspace-1").unwrap();
        assert!(!*halt.borrow_and_update());

        registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();
        assert!(*halt.borrow_and_update());
        assert!(registry.observation_fence(capability, "browser-1").is_err());
    }

    // ── Atomic completion regression tests ──────────────────────────

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_after_stop() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();

        let error = registry
            .complete_semantic_snapshot(
                capability,
                "browser-1",
                instance,
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("stopped by the user") || error.contains("capability is unavailable"),
            "expected Stop to block completion, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_wrong_instance() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        // The instance was registered as `instance`, but we pass a different value.
        let error = registry
            .complete_semantic_snapshot(
                capability,
                "browser-1",
                instance + 1, // intentionally wrong instance
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("document changed while it was being inspected"),
            "expected instance-id fence to reject, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_wrong_document_epoch() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        let error = registry
            .complete_semantic_snapshot(
                capability,
                "browser-1",
                instance,
                1, // wrong epoch
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("document changed while it was being inspected"),
            "expected epoch fence to reject, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_when_hidden() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        registry
            .set_visible("browser-1", "workspace-1", false)
            .unwrap();

        let error = registry
            .complete_semantic_snapshot(
                capability,
                "browser-1",
                instance,
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("hidden"),
            "expected visibility check to reject, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_revoked_capability() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        registry.revoke_agent_capability(capability);

        let error = registry
            .complete_semantic_snapshot(
                capability,
                "browser-1",
                instance,
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("capability is unavailable"),
            "expected revoked capability to reject, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_semantic_snapshot_rejects_wrong_controller() {
        let (registry, instance, _origin, _original_capability, _private) = controlled_registry();
        // Issue a second capability that never began control.
        let other_capability = registry.issue_agent_capability("workspace-1", "Other agent");

        let error = registry
            .complete_semantic_snapshot(
                other_capability,
                "browser-1",
                instance,
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap_err();
        assert!(
            error.contains("not controlled by this agent"),
            "expected controller check to reject, got: {error}"
        );
    }

    #[tokio::test]
    async fn complete_screenshot_recording_rejects_after_stop() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        // Plant a stored snapshot so screenshot recording has something to validate.
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap();

        registry
            .stop_agent_control("browser-1", "workspace-1")
            .await
            .unwrap();

        let error = registry
            .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-1")
            .unwrap_err();
        assert!(
            error.contains("stopped by the user") || error.contains("capability is unavailable"),
            "expected Stop to block screenshot recording, got: {error}"
        );
    }

    #[test]
    fn complete_screenshot_recording_rejects_wrong_instance() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap();

        let error = registry
            .complete_screenshot_recording(
                capability,
                "browser-1",
                instance + 1, // intentionally wrong
                0,
                "snapshot-1",
            )
            .unwrap_err();
        assert!(
            error.contains("document changed while screenshot"),
            "expected instance-id fence to reject, got: {error}"
        );
    }

    #[test]
    fn complete_screenshot_recording_rejects_forged_snapshot_id() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        registry
            .record_semantic_snapshot(
                "browser-1",
                "workspace-1",
                0,
                "snapshot-1".to_owned(),
                HashMap::new(),
            )
            .unwrap();

        let error = registry
            .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-forged")
            .unwrap_err();
        assert!(
            error.contains("snapshot is stale"),
            "expected forged snapshot id to reject, got: {error}"
        );
    }

    #[test]
    fn complete_screenshot_recording_rejects_missing_snapshot() {
        let (registry, instance, _origin, capability, _private) = controlled_registry();
        // No record_semantic_snapshot call — there is no stored snapshot.

        let error = registry
            .complete_screenshot_recording(capability, "browser-1", instance, 0, "snapshot-1")
            .unwrap_err();
        assert!(
            error.contains("snapshot is stale"),
            "expected missing stored snapshot to reject, got: {error}"
        );
    }
}
