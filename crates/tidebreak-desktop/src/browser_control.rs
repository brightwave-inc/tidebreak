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
    required_capability: BrowserGrantCapability,
    action_type: String,
    target_label: Option<String>,
    binding: Option<BrowserConfirmationBinding>,
    expires_at: Instant,
}

/// Private exact-resource identity covered by one native confirmation.
///
/// The digest and length never enter renderer state or browser audit output.
/// They only prevent a resource from changing between the prompt and native
/// dispatch.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct BrowserConfirmationBinding {
    pub(crate) filename: String,
    pub(crate) byte_len: u64,
    pub(crate) sha256: [u8; 32],
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
    #[allow(
        clippy::too_many_arguments,
        reason = "confirmation scope and exact resource identity stay explicit at the native boundary"
    )]
    pub(crate) fn record_native_confirmation(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        required_capability: BrowserGrantCapability,
        action_type: &str,
        target_label: Option<&str>,
        binding: Option<&BrowserConfirmationBinding>,
    ) -> Result<Uuid, String> {
        self.record_native_confirmation_for(
            capability_id,
            browser_id,
            origin,
            required_capability,
            action_type,
            target_label,
            binding,
            AGENT_CONFIRMATION_TTL,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the expiry override extends the same explicit confirmation boundary"
    )]
    fn record_native_confirmation_for(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        required_capability: BrowserGrantCapability,
        action_type: &str,
        target_label: Option<&str>,
        binding: Option<&BrowserConfirmationBinding>,
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
            required_capability,
        ) {
            return Err("browser origin is not shared for this operation".to_owned());
        }
        let confirmation_id = Uuid::new_v4();
        state.confirmations.insert(
            confirmation_id,
            BrowserConfirmationRecord {
                capability_id,
                browser_id: browser_id.to_owned(),
                workspace_id: capability.workspace_id,
                origin: origin.clone(),
                required_capability,
                action_type: clean_audit_text(
                    action_type,
                    MAX_AUDIT_ACTION_CHARS,
                    "browser_action",
                ),
                target_label: target_label.map(|value| {
                    clean_audit_text(value, MAX_AUDIT_TARGET_CHARS, "unlabeled target")
                }),
                binding: binding.cloned(),
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
        self.dispatch_agent_with_confirmation_binding(
            capability_id,
            browser_id,
            origin,
            required_capability,
            action_type,
            target_label,
            effect,
            confirmation_id,
            None,
            dispatch,
        )
        .await
    }

    /// Dispatch one operation whose native confirmation covers an exact
    /// resource identity in addition to the semantic browser target.
    #[allow(
        clippy::too_many_arguments,
        reason = "security-sensitive authorization inputs stay explicit at the native dispatch boundary"
    )]
    pub(crate) async fn dispatch_agent_with_confirmation_binding<T, F, Fut>(
        &self,
        capability_id: Uuid,
        browser_id: &str,
        origin: &BrowserOrigin,
        required_capability: BrowserGrantCapability,
        action_type: &str,
        target_label: Option<&str>,
        effect: BrowserDispatchEffect,
        confirmation_id: Option<Uuid>,
        confirmation_binding: Option<BrowserConfirmationBinding>,
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
                confirmation_binding.as_ref(),
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
                confirmation_binding.as_ref(),
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
    confirmation_binding: Option<&BrowserConfirmationBinding>,
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
            || confirmation.required_capability != required_capability
            || confirmation.action_type != action_type
            || confirmation.target_label.as_deref() != target_label
            || confirmation.binding.as_ref() != confirmation_binding
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
mod tests;
