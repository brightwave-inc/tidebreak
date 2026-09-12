//! Wire remote sessions into the running server: the configured transport,
//! the per-session pump tasks, and the sweep that reconciles both.
//!
//! The sweep is the only scheduler. Every pass it closes stale intents and
//! makes sure each incarnation that still owes events has a pump task. Pump
//! tasks hold the long event wait; everything else is a cheap store read, so
//! a crashed or finished task is simply respawned on the next pass from
//! durable state.
//!
//! Passes are wake-driven. A submit that provisioned, a parked follow-up, an
//! external message, or a pump task exiting wakes the sweep at once; the
//! timer between wakes is only a safety net, and it slows right down when no
//! incarnation is draining so an idle deployment stops paying for a scan
//! nobody needs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::Notify;
use tracing::{debug, warn};

use tidebreak_core::db::code::{get_session, latest_incarnations_of_live_sessions_all_owners};
use tidebreak_core::{DbStore, IncarnationState, OwnerId, SessionId, SessionLifecycle};

use super::super::runtime::CodeRuntime;
use super::driver::{sweep_stale_intents, RemoteDriver, RemoteSpawnSettings};
use super::SandboxProvisioner;
use crate::retry::LaneBackoff;

/// The safety-net interval between passes while an incarnation is draining.
/// Wakes carry the normal traffic; this bounds how long a missed wake or a
/// pump that exited without one can go unnoticed.
const ACTIVE_SWEEP_FLOOR: std::time::Duration = std::time::Duration::from_secs(2);

/// The safety-net interval between passes while nothing remote is draining.
/// The stale-intent cutoff is measured in minutes, so this loses nothing.
const IDLE_SWEEP_FLOOR: std::time::Duration = std::time::Duration::from_secs(20);

/// The held wait a pump task asks the events read to hold.
const PUMP_HELD_WAIT_SECONDS: u16 = 20;

/// How long a pump task sleeps after its first fault before retrying. Each
/// consecutive fault doubles the wait up to [`PUMP_FAULT_BACKOFF_CAP`]; one
/// read that goes through resets it. Retryable faults count too: an
/// unavailable environment answers the events read at once, and without a
/// wait the pump would re-issue it as fast as the store answers.
const PUMP_FAULT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);

/// Ceiling on the wait between pump retries while faults persist.
const PUMP_FAULT_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(60);

/// How long promotion leaves a session alone after a machine-side refusal
/// (cap full, sign-in needed). Refusals journal a notice and poke attention;
/// retrying every sweep tick would repeat both every two seconds.
const PROMOTION_RETRY_HOLD: std::time::Duration = std::time::Duration::from_secs(150);

/// Resolve one deployment's remote spawn settings from boot configuration.
/// The per-spawn ceiling bounds one runaway incarnation. The per-session
/// ledger bounds their sum because reincarnation multiplies the former.
pub(crate) fn configured_settings(
    profile: String,
    config: &tidebreak_core::Config,
) -> RemoteSpawnSettings {
    RemoteSpawnSettings {
        profile,
        engine: config.runtime_engine,
        engines: config.runtime_engines.clone(),
        embedded_engine_registration: config.runtime_embedded_engine_registration,
        incarnation_cap: config.runtime_concurrency_cap,
        spend_ceiling_microusd: config.runtime_spawn_spend_ceiling_microusd,
        session_spend_ceiling_microusd: config.runtime_session_spend_ceiling_microusd,
    }
}

/// The remote-session context one deployment configures: the transport and
/// the spawn settings, plus the live pump tasks the sweep reconciles.
pub struct RemoteSessions {
    /// The environment transport.
    pub(crate) provisioner: Arc<dyn SandboxProvisioner>,
    /// Spawn-time settings.
    pub(crate) settings: RemoteSpawnSettings,
    /// Protected-tool executor for supervised sandboxes, when wired.
    host_tool: std::sync::OnceLock<Arc<dyn tidebreak_code_remote::driver::HostToolExecutor>>,
    /// Live pump tasks by session. The sweep prunes finished entries and
    /// spawns missing ones; a pump task removes its own entry on the way out
    /// so the pass it wakes sees the slot free.
    pumps: Mutex<HashMap<SessionId, tokio::task::JoinHandle<()>>>,
    /// Sessions whose queue promotion is on hold until the given instant,
    /// after a machine-side refusal. In-memory on purpose: a restart retries
    /// once and re-arms the hold from the fresh refusal.
    promotion_holds: Mutex<HashMap<SessionId, std::time::Instant>>,
    /// The last published startup failure, independent of display attention.
    /// A successful promotion clears it; expiring a retry hold does not.
    startup_failures: Mutex<HashMap<SessionId, String>>,
    /// Serialize promotion through failure publication for each session.
    promotion_locks: Mutex<HashMap<SessionId, Weak<tokio::sync::Mutex<()>>>>,
    /// Wakes the sweep for an immediate pass. A wake with no waiter is kept
    /// until the sweep next listens, so none is lost between passes.
    sweep_wake: Notify,
}

impl RemoteSessions {
    pub fn new(
        provisioner: Arc<dyn SandboxProvisioner>,
        settings: RemoteSpawnSettings,
    ) -> Arc<Self> {
        Arc::new(Self {
            provisioner,
            settings,
            host_tool: std::sync::OnceLock::new(),
            pumps: Mutex::new(HashMap::new()),
            promotion_holds: Mutex::new(HashMap::new()),
            startup_failures: Mutex::new(HashMap::new()),
            promotion_locks: Mutex::new(HashMap::new()),
            sweep_wake: Notify::new(),
        })
    }

    /// Attach the protected-tool executor this deployment serves.
    pub fn with_host_tool(
        self: &Arc<Self>,
        host: Arc<dyn tidebreak_code_remote::driver::HostToolExecutor>,
    ) {
        // May only be set before pumps start; recovery happens after boot
        // wiring, so this is safe.
        let _ = self.host_tool.set(host);
    }

    /// Ask the sweep for a pass now instead of at its next floor.
    pub(crate) fn wake_sweep(&self) {
        self.sweep_wake.notify_one();
    }

    /// The driver view over this context for one call.
    pub fn driver<'a>(
        &'a self,
        db: &'a Arc<DbStore>,
        bus: &'a super::super::bus::CodeEventBus,
    ) -> RemoteDriver<'a> {
        RemoteDriver {
            db,
            bus,
            provisioner: self.provisioner.as_ref(),
            settings: &self.settings,
            host_tool: self.host_tool.get().map(AsRef::as_ref),
        }
    }

    /// Whether promotion for `session` is inside a refusal hold.
    pub(crate) fn promotion_held(&self, session: SessionId) -> bool {
        let mut holds = self.promotion_holds.lock().expect("promotion holds");
        let now = std::time::Instant::now();
        holds.retain(|_, until| *until > now);
        match holds.get(&session) {
            Some(until) if *until > std::time::Instant::now() => true,
            Some(_) => {
                holds.remove(&session);
                false
            }
            None => false,
        }
    }

    /// Hold promotion for `session` for [`PROMOTION_RETRY_HOLD`].
    pub(crate) fn hold_promotion(&self, session: SessionId) {
        let mut holds = self.promotion_holds.lock().expect("promotion holds");
        let now = std::time::Instant::now();
        holds.retain(|_, until| *until > now);
        holds.insert(session, now + PROMOTION_RETRY_HOLD);
    }

    /// Clear a hold after a promotion that went through.
    pub(crate) fn clear_promotion_hold(&self, session: SessionId) {
        let mut holds = self.promotion_holds.lock().expect("promotion holds");
        let now = std::time::Instant::now();
        holds.retain(|_, until| *until > now);
        holds.remove(&session);
    }

    pub(crate) fn promotion_lock(&self, session: SessionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.promotion_locks.lock().expect("promotion locks");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(&session).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(session, Arc::downgrade(&lock));
        lock
    }

    pub(crate) fn startup_failure_matches(&self, session: SessionId, message: &str) -> bool {
        self.startup_failures
            .lock()
            .expect("startup failures")
            .get(&session)
            .is_some_and(|last| last == message)
    }

    pub(crate) fn record_startup_failure(&self, session: SessionId, message: String) {
        self.startup_failures
            .lock()
            .expect("startup failures")
            .insert(session, message);
    }

    pub(crate) fn clear_startup_failure(&self, session: SessionId) {
        self.startup_failures
            .lock()
            .expect("startup failures")
            .remove(&session);
    }

    /// Fresh input retries startup failures but does not bypass capacity holds.
    pub(crate) fn retry_startup_failure(&self, session: SessionId) {
        if self
            .startup_failures
            .lock()
            .expect("startup failures")
            .contains_key(&session)
        {
            self.clear_promotion_hold(session);
        }
    }

    /// Make sure `session` has a pump task, spawning one when it has none.
    fn ensure_pump(
        self: &Arc<Self>,
        runtime: &Arc<CodeRuntime>,
        owner: OwnerId,
        session: SessionId,
    ) {
        let mut pumps = self.pumps.lock().expect("remote pumps");
        pumps.retain(|_, handle| !handle.is_finished());
        if pumps.contains_key(&session) {
            return;
        }
        let runtime = Arc::downgrade(runtime);
        let remote = Arc::clone(self);
        pumps.insert(
            session,
            tokio::spawn(async move {
                let backoff = LaneBackoff::new(PUMP_FAULT_BACKOFF, PUMP_FAULT_BACKOFF_CAP);
                let wake =
                    pump_session(runtime, Arc::clone(&remote), owner, session, backoff).await;
                // Free the slot before waking: the pass this wake starts
                // must see no entry, or it skips the respawn and waits out
                // a floor instead. The entry is always this task's own —
                // the sweep only inserts where none exists, and this one
                // stands until here.
                remote.pumps.lock().expect("remote pumps").remove(&session);
                if wake {
                    remote.wake_sweep();
                }
            }),
        );
    }
}

impl Drop for RemoteSessions {
    fn drop(&mut self) {
        for (_, handle) in self.pumps.lock().expect("remote pumps").drain() {
            handle.abort();
        }
    }
}

/// One session's pump loop: drain events on the held wait until the session
/// stops being pumpable. Exits on any fault or terminal condition — the
/// sweep respawns from durable state, so an exit is never a leak.
///
/// Returns whether the exit is worth an immediate sweep pass. A stopped
/// incarnation or a fence may leave a queue head to promote; a sign-in wait
/// does not, and waking on it would respawn the pump in a tight loop until
/// the owner signs in, so that exit waits for the floor.
///
/// `backoff` paces the retries after a fault, retryable or hard: the wait
/// doubles per consecutive fault and one clean pump resets it.
async fn pump_session(
    runtime: Weak<CodeRuntime>,
    remote: Arc<RemoteSessions>,
    owner: OwnerId,
    session_id: SessionId,
    mut backoff: LaneBackoff,
) -> bool {
    loop {
        let Some(runtime) = runtime.upgrade() else {
            return false;
        };
        let Ok(Some(mut session)) = get_session(&runtime.db, &owner, session_id).await else {
            return false;
        };
        if matches!(
            session.lifecycle,
            SessionLifecycle::Fenced | SessionLifecycle::Ended
        ) {
            return false;
        }
        let driver = remote.driver(&runtime.db, runtime.bus.as_ref());
        match driver.pump(&mut session, PUMP_HELD_WAIT_SECONDS).await {
            Ok(report) if report.read_unavailable => {
                // The environment is unavailable; the read came back at
                // once instead of holding the wait. Pace the retry so the
                // pump does not spin on a transport that is down.
                let wait = backoff.next_delay();
                debug!(session = %session_id, ?wait, "the sandbox stream is unavailable; backing off");
                drop(runtime);
                tokio::time::sleep(wait).await;
            }
            Ok(report) => {
                backoff.reset();
                if report.sign_in_required {
                    // Nothing drains until the owner signs in; the sweep
                    // brings the task back to try again.
                    return false;
                }
                if report.incarnation_stopped || report.fenced.is_some() {
                    return true;
                }
            }
            Err(error) => {
                let wait = backoff.next_delay();
                warn!(session = %session_id, %error, ?wait, "a remote pump failed; backing off");
                // Drop the runtime handle across the sleep so shutdown is
                // not held open by a backoff.
                drop(runtime);
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Holds the remote sweep alive; aborts it on drop.
pub(crate) struct RemoteSweepGuard {
    handle: Option<tokio::task::JoinHandle<()>>,
    runtime: Weak<CodeRuntime>,
}

impl RemoteSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
        let guard_runtime = runtime.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                let activity = sweep_remote(&runtime).await;
                let remote = runtime.remote_sessions();
                // Drop the strong handle across the wait so shutdown is not
                // held open by a sleeping sweep.
                drop(runtime);
                let floor = sweep_floor(activity);
                match remote {
                    Some(remote) => {
                        tokio::select! {
                            () = tokio::time::sleep(floor) => {}
                            () = remote.sweep_wake.notified() => {}
                        }
                    }
                    None => tokio::time::sleep(floor).await,
                }
            }
        });
        Self {
            handle: Some(handle),
            runtime: guard_runtime,
        }
    }
}

impl Drop for RemoteSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
        if let Some(runtime) = self.runtime.upgrade() {
            if let Some(remote) = runtime.remote_sessions() {
                if let Some(host) = remote.host_tool.get() {
                    host.shutdown();
                }
            }
        }
    }
}

/// What one sweep pass found, which sets the floor before the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RemoteSweepActivity {
    /// Incarnations that still owe events and so hold a pump task.
    draining: usize,
}

/// The safety-net wait after a pass: tight while something drains, slow
/// when nothing does. Wakes cut either short.
fn sweep_floor(activity: RemoteSweepActivity) -> std::time::Duration {
    if activity.draining > 0 {
        ACTIVE_SWEEP_FLOOR
    } else {
        IDLE_SWEEP_FLOOR
    }
}

/// One sweep pass: expire stale intents and reconcile pump tasks against
/// the incarnations that still owe events.
async fn sweep_remote(runtime: &Arc<CodeRuntime>) -> RemoteSweepActivity {
    let mut activity = RemoteSweepActivity::default();
    let Some(remote) = runtime.remote_sessions() else {
        return activity;
    };
    if let Err(error) =
        sweep_stale_intents(&runtime.db, runtime.bus.as_ref(), chrono::Utc::now()).await
    {
        warn!(%error, "the stale-intent sweep failed");
    }
    // One join: the latest incarnation of every session that is neither
    // fenced nor ended. The cost tracks live sessions, not session history.
    match latest_incarnations_of_live_sessions_all_owners(&runtime.db).await {
        Ok(rows) => {
            for row in rows {
                let drains = match row.state {
                    IncarnationState::Active => true,
                    IncarnationState::Stopped => !row.terminal_events_journaled,
                    IncarnationState::Intent => false,
                };
                if drains && row.sandbox_id.is_some() {
                    activity.draining += 1;
                    remote.ensure_pump(runtime, row.owner.clone(), row.session_id);
                }
            }
        }
        Err(error) => warn!(%error, "could not list incarnations for the remote sweep"),
    }
    if let Err(error) = runtime.promote_remote_queue_heads().await {
        warn!(error = ?error, "remote queue promotion failed");
    }
    activity
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;

    use tidebreak_core::db::code::{insert_repo, latest_turn};
    use tidebreak_core::{
        CodeRepo, CodeWorkspaceStatus, FenceReason, HarnessKind, OwnerId, PermissionMode, RepoId,
        SessionLifecycle, TurnStatus,
    };

    use super::super::super::runtime::{
        ExternalMessageOutcome, NewSessionSettings, SubmitTurnOutcome,
    };
    use super::super::driver::RemoteSpawnSettings;
    use super::super::wire::{
        EventCursor, MessageReceipt, SandboxEvent, SandboxEvents, SandboxLease, SandboxMessage,
        SandboxState, SandboxStatus, SpawnArguments,
    };
    use super::super::{RemoteSandboxError, SandboxProvisioner};
    use super::*;

    #[derive(Default)]
    struct ProvisionGate {
        entered: Notify,
        release: Notify,
    }

    async fn wait_for_provision_gate(gate: &StdMutex<Option<Arc<ProvisionGate>>>) {
        let gate = gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }

    #[derive(Default)]
    struct FakeProvisioner {
        spawns: StdMutex<Vec<SpawnArguments>>,
        spawn_gate: StdMutex<Option<Arc<ProvisionGate>>>,
        send_gate: StdMutex<Option<Arc<ProvisionGate>>>,
        spawn_errors: StdMutex<VecDeque<RemoteSandboxError>>,
        sends: StdMutex<Vec<String>>,
        event_reads: StdMutex<VecDeque<SandboxEvents>>,
        /// Every events read issued, scripted or not.
        event_reads_issued: StdMutex<usize>,
        cancels: StdMutex<Vec<String>>,
        status_override: StdMutex<Option<SandboxStatus>>,
        status_reads: StdMutex<usize>,
    }

    #[async_trait]
    impl SandboxProvisioner for FakeProvisioner {
        async fn spawn(
            &self,
            _owner: &OwnerId,
            _session: tidebreak_core::SessionId,
            arguments: &SpawnArguments,
        ) -> Result<SandboxLease, RemoteSandboxError> {
            wait_for_provision_gate(&self.spawn_gate).await;
            let sandbox_id = {
                let mut spawns = self.spawns.lock().unwrap();
                spawns.push(arguments.clone());
                format!("sb-{}", spawns.len())
            };
            if let Some(error) = self.spawn_errors.lock().unwrap().pop_front() {
                return Err(error);
            }
            Ok(SandboxLease {
                sandbox_id,
                state: SandboxState::Pending,
                latest_event_seq: 0,
                expires_in_seconds: 7200,
            })
        }

        async fn status(
            &self,
            _owner: &OwnerId,
            _session: tidebreak_core::SessionId,
            sandbox_id: &str,
        ) -> Result<SandboxStatus, RemoteSandboxError> {
            *self.status_reads.lock().unwrap() += 1;
            if let Some(status) = self.status_override.lock().unwrap().clone() {
                return Ok(status);
            }
            Ok(SandboxStatus {
                sandbox_id: sandbox_id.to_owned(),
                state: SandboxState::Running,
                failure_reason: None,
                termination_reason: None,
                latest_event_seq: 0,
                pending_messages: 0,
                spend_microusd: None,
                spend_ceiling_microusd: None,
                possibly_stalled: false,
                repository_url: None,
                completed_at: None,
            })
        }

        async fn events(
            &self,
            _owner: &OwnerId,
            _session: tidebreak_core::SessionId,
            _sandbox_id: &str,
            _cursor: EventCursor,
        ) -> Result<SandboxEvents, RemoteSandboxError> {
            *self.event_reads_issued.lock().unwrap() += 1;
            self.event_reads
                .lock()
                .unwrap()
                .pop_front()
                .ok_or(RemoteSandboxError::Unavailable {
                    operation: "events",
                    detail: "no scripted read".to_owned(),
                })
        }

        async fn send(
            &self,
            _owner: &OwnerId,
            _session: tidebreak_core::SessionId,
            _sandbox_id: &str,
            message: &SandboxMessage,
        ) -> Result<MessageReceipt, RemoteSandboxError> {
            wait_for_provision_gate(&self.send_gate).await;
            let super::super::wire::SupervisorMessageBody::Input(body) = &message.body;
            self.sends.lock().unwrap().push(body.clone());
            Ok(MessageReceipt {
                seq: 1,
                interrupt: false,
                pending_messages: 0,
            })
        }

        async fn cancel(
            &self,
            _owner: &OwnerId,
            _session: tidebreak_core::SessionId,
            sandbox_id: &str,
        ) -> Result<(), RemoteSandboxError> {
            self.cancels.lock().unwrap().push(sandbox_id.to_owned());
            Ok(())
        }
    }

    fn settings() -> RemoteSpawnSettings {
        RemoteSpawnSettings {
            profile: "tidebreak-remote".to_owned(),
            engine: None,
            engines: None,
            embedded_engine_registration: false,
            incarnation_cap: 2,
            spend_ceiling_microusd: None,
            session_spend_ceiling_microusd: None,
        }
    }

    #[test]
    fn configured_settings_use_operator_limits() {
        let mut config = tidebreak_core::Config::desktop("/data");
        config.runtime_concurrency_cap = 7;
        config.runtime_engine = Some(HarnessKind::ClaudeCode);
        config.runtime_embedded_engine_registration = true;
        config.runtime_engines = Some(vec![HarnessKind::ClaudeCode, HarnessKind::Codex]);
        config.runtime_spawn_spend_ceiling_microusd = Some(9_000_000);
        config.runtime_session_spend_ceiling_microusd = None;

        let settings = configured_settings("remote-large".to_owned(), &config);
        assert_eq!(settings.profile, "remote-large");
        assert_eq!(settings.engine, Some(HarnessKind::ClaudeCode));
        assert!(settings.embedded_engine_registration);
        assert_eq!(
            settings.engines,
            Some(vec![HarnessKind::ClaudeCode, HarnessKind::Codex])
        );
        assert_eq!(settings.incarnation_cap, 7);
        assert_eq!(settings.spend_ceiling_microusd, Some(9_000_000));
        assert_eq!(settings.session_spend_ceiling_microusd, None);
    }

    async fn runtime_with_remote(
        root: &std::path::Path,
    ) -> (Arc<CodeRuntime>, Arc<FakeProvisioner>, OwnerId, CodeRepo) {
        runtime_with_remote_settings(root, settings()).await
    }

    async fn runtime_with_remote_settings(
        root: &std::path::Path,
        spawn_settings: RemoteSpawnSettings,
    ) -> (Arc<CodeRuntime>, Arc<FakeProvisioner>, OwnerId, CodeRepo) {
        let db = tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            root.join("code.db").display()
        ))
        .await
        .unwrap();
        let fake = Arc::new(FakeProvisioner::default());
        let runtime = CodeRuntime::new(
            Arc::new(db),
            root.to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .with_remote_sessions(RemoteSessions::new(fake.clone(), spawn_settings));
        let runtime = Arc::new(runtime);
        let owner = OwnerId::local();
        let repo = CodeRepo {
            id: RepoId::new(),
            owner: owner.clone(),
            root_path: root.join("repo").display().to_string(),
            display_name: "tools".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: Some("github.com".into()),
            origin_owner: Some("acme".into()),
            origin_name: Some("tools".into()),
        };
        insert_repo(&runtime.db, &repo).await.unwrap();
        (runtime, fake, owner, repo)
    }

    fn session_settings() -> NewSessionSettings {
        NewSessionSettings {
            permission_mode: PermissionMode::Allow,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            permission_mode_ceiling: None,
            acts_as: None,
        }
    }

    fn event(seq: i64, kind: &str, payload: serde_json::Value) -> SandboxEvent {
        SandboxEvent {
            seq,
            kind: kind.to_owned(),
            payload,
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn repeated_remote_titles_get_distinct_branches() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) =
            runtime_with_remote_settings(dir.path(), settings()).await;
        let (first, second) = tokio::join!(
            runtime.create_remote_workspace(&owner, repo.id, Some("Verify this repository".into())),
            runtime.create_remote_workspace(&owner, repo.id, Some("Verify this repository".into()))
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_ne!(first.branch_name, second.branch_name);
        assert_eq!(first.title, second.title);
        assert!(first.branch_name.starts_with(&repo.branch_prefix));
        let third = runtime
            .create_remote_workspace(&owner, repo.id, Some(first.title.clone()))
            .await
            .unwrap();
        assert_ne!(first.branch_name, third.branch_name);
        assert_ne!(second.branch_name, third.branch_name);
    }

    #[tokio::test]
    async fn a_service_owner_labels_the_remote_session_it_creates() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) =
            runtime_with_remote_settings(dir.path(), settings()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("service-owned".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                Some("service"),
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        assert_eq!(session.owner_kind.as_deref(), Some("service"));
        let stored = runtime.get_session(&owner, session.id).await.unwrap();
        assert_eq!(
            stored.owner_kind.as_deref(),
            Some("service"),
            "the kind is persisted with the session, not only returned"
        );
    }

    #[tokio::test]
    async fn a_declared_profile_rejects_settings_before_saving_or_queueing() {
        let dir = tempfile::tempdir().unwrap();
        let mut spawn_settings = settings();
        spawn_settings.engine = Some(HarnessKind::ClaudeCode);
        let (runtime, fake, owner, repo) =
            runtime_with_remote_settings(dir.path(), spawn_settings).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("declared".into()))
            .await
            .unwrap();
        let mut unsupported = session_settings();
        unsupported.permission_mode = PermissionMode::Ask;
        assert!(runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                unsupported
            )
            .await
            .is_err());
        let mut fast = session_settings();
        fast.fast_mode = true;
        assert!(runtime
            .create_remote_session(&owner, None, workspace.id, HarnessKind::ClaudeCode, fast)
            .await
            .is_err());
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        assert!(runtime
            .set_permission_mode(&owner, session.id, PermissionMode::Ask)
            .await
            .is_err());
        assert!(runtime
            .set_fast_mode(&owner, session.id, true)
            .await
            .is_err());
        assert!(runtime
            .set_reasoning_effort(
                &owner,
                session.id,
                Some(tidebreak_core::ReasoningEffort::High)
            )
            .await
            .is_err());
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        assert!(runtime
            .submit_turn(
                &owner,
                session.id,
                "changed model".into(),
                Some("another-model".into()),
                None,
                Vec::new(),
                None
            )
            .await
            .is_err());
        let stored = runtime.get_session(&owner, session.id).await.unwrap();
        assert_eq!(stored.permission_mode, PermissionMode::Allow);
        assert!(!stored.fast_mode);
        assert_eq!(stored.model, None);
        assert_eq!(stored.reasoning_effort, None);
        let mut incompatible = stored;
        incompatible.harness_kind = HarnessKind::Codex;
        tidebreak_core::db::code::save_session(&runtime.db, &incompatible)
            .await
            .unwrap();
        assert!(runtime
            .submit_turn(
                &owner,
                session.id,
                "unsupported queued engine".into(),
                None,
                None,
                Vec::new(),
                None
            )
            .await
            .is_err());
        let (queue, _) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert!(queue.is_empty());
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);
        assert!(fake.sends.lock().unwrap().is_empty());
    }

    /// The runtime carries a turn to a sandbox with no local harness: create
    /// records the empty worktree marker, and submit provisions remotely.
    ///
    /// If submit still went local, `require_worker` would fail (no harness
    /// child) and the fake would see no spawn. A follow-up while the turn
    /// runs parks, then promotion delivers it as an inbox message after idle.
    #[tokio::test]
    async fn a_remote_session_carries_a_turn_without_a_local_harness() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;

        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        assert!(workspace.is_remote());
        // The marker is per-workspace: the column is unique, and a shared
        // sentinel would cap the machine at one remote workspace.
        assert_eq!(
            workspace.worktree_path,
            tidebreak_core::CodeWorkspace::remote_worktree_marker(workspace.id)
        );
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        assert_eq!(session.lifecycle, SessionLifecycle::Idle);

        let outcome = runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(outcome, SubmitTurnOutcome::Ran(_)));
        {
            let spawns = fake.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1, "submit must provision a sandbox");
            assert_eq!(spawns[0].repository_ref.as_deref(), Some("main"));
            assert_eq!(
                spawns[0].repository.as_deref(),
                Some("https://github.com/acme/tools")
            );
        }

        let queued = runtime
            .submit_turn(
                &owner,
                session.id,
                "and then".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        assert!(matches!(queued, SubmitTurnOutcome::Queued(_)));

        fake.event_reads.lock().unwrap().push_back(SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state: SandboxState::Running,
            latest_event_seq: 2,
            events: vec![
                event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                event(
                    2,
                    "turn_completed",
                    serde_json::json!({ "turn": 1, "exit_code": 0 }),
                ),
            ],
        });
        let mut live = runtime.get_session(&owner, session.id).await.unwrap();
        runtime
            .remote_sessions()
            .unwrap()
            .driver(&runtime.db, runtime.bus.as_ref())
            .pump(&mut live, 0)
            .await
            .unwrap();
        runtime.promote_remote_queue_heads().await.unwrap();

        let (queued_rows, _) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert!(queued_rows.is_empty());
        let turn = latest_turn(&runtime.db, &owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.ordinal, 2);
        assert_eq!(turn.status, TurnStatus::Running);
        assert_eq!(
            fake.sends.lock().unwrap().as_slice(),
            &["and then".to_owned()]
        );
    }

    #[tokio::test]
    async fn recovery_waits_for_remote_spawn_and_queued_send_admission() {
        for queued in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
            let workspace = runtime
                .create_remote_workspace(&owner, repo.id, Some("remote".into()))
                .await
                .unwrap();
            let session = runtime
                .create_remote_session(
                    &owner,
                    None,
                    workspace.id,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                )
                .await
                .unwrap();
            if queued {
                runtime
                    .submit_turn(&owner, session.id, "first".into(), None, None, vec![], None)
                    .await
                    .unwrap();
                runtime
                    .submit_turn(&owner, session.id, "next".into(), None, None, vec![], None)
                    .await
                    .unwrap();
                fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                    sandbox_id: "sb-1".into(),
                    state: SandboxState::Running,
                    latest_event_seq: 2,
                    events: vec![
                        event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                        event(
                            2,
                            "turn_completed",
                            serde_json::json!({ "turn": 1, "exit_code": 0 }),
                        ),
                    ],
                });
                let mut live = runtime.get_session(&owner, session.id).await.unwrap();
                runtime
                    .remote_sessions()
                    .unwrap()
                    .driver(&runtime.db, runtime.bus.as_ref())
                    .pump(&mut live, 0)
                    .await
                    .unwrap();
            }
            let gate = Arc::new(ProvisionGate::default());
            if queued {
                *fake.send_gate.lock().unwrap() = Some(gate.clone());
            } else {
                *fake.spawn_gate.lock().unwrap() = Some(gate.clone());
            }
            let admission = tokio::spawn({
                let runtime = runtime.clone();
                let owner = owner.clone();
                async move {
                    if queued {
                        runtime.promote_remote_queue_heads().await
                    } else {
                        runtime
                            .submit_turn(
                                &owner,
                                session.id,
                                "first".into(),
                                None,
                                None,
                                vec![],
                                None,
                            )
                            .await
                            .map(|_| ())
                    }
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
                .await
                .expect("remote admission reaches the provisioner");
            let recovery = runtime.reap(&owner, session.id);
            tokio::pin!(recovery);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut recovery)
                    .await
                    .is_err(),
                "recovery must wait until remote admission commits, queued={queued}"
            );
            gate.release.notify_one();
            admission.await.unwrap().unwrap();
            assert_eq!(recovery.await.unwrap_err().kind(), "not_fenced");
            let current = runtime.get_session(&owner, session.id).await.unwrap();
            assert_eq!(current.lifecycle, SessionLifecycle::Running);
            assert_eq!(current.spawn_epoch, session.spawn_epoch);
            let turn = latest_turn(&runtime.db, &owner, session.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(turn.status, TurnStatus::Running);
            assert_eq!(turn.ordinal, if queued { 2 } else { 1 });
            assert!(runtime
                .list_queued_turns(&owner, session.id)
                .await
                .unwrap()
                .0
                .is_empty());
            assert!(fake.cancels.lock().unwrap().is_empty());
        }
    }

    /// Remote session creation must serialize with workspace lifecycle
    /// changes so archive cannot leave a new idle session on an archived row.
    #[tokio::test]
    async fn remote_session_creation_rechecks_status_under_the_lifecycle_lock() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let lifecycle = runtime.workspace_write_lock(workspace.id);
        let lifecycle_guard = lifecycle.lock().await;
        let creating_runtime = runtime.clone();
        let creating_owner = owner.clone();
        let mut creating = tokio::spawn(async move {
            creating_runtime
                .create_remote_session(
                    &creating_owner,
                    None,
                    workspace.id,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                )
                .await
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut creating)
                .await
                .is_err(),
            "session creation must wait for the workspace lifecycle lock"
        );
        assert!(tidebreak_core::db::code::compare_and_set_workspace_status(
            &runtime.db,
            &owner,
            workspace.id,
            CodeWorkspaceStatus::Active,
            CodeWorkspaceStatus::Archiving,
        )
        .await
        .unwrap());
        drop(lifecycle_guard);

        let error = tokio::time::timeout(std::time::Duration::from_secs(2), creating)
            .await
            .expect("session create finished after lifecycle release")
            .expect("session create task joined")
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_not_ready");
        assert!(runtime
            .list_workspace_sessions(&owner, workspace.id)
            .await
            .unwrap()
            .is_empty());
    }

    /// A queued row edited after the sweep's snapshot does not collide with
    /// the delivered turn: the stale claim records the delivered text under
    /// a fresh id, and the edited row promotes later under its own id.
    #[tokio::test]
    async fn a_stale_promotion_records_the_turn_without_eating_the_edit() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let remote = runtime.remote_sessions().unwrap();
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "original".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        let stale = tidebreak_core::db::code::queued_turn_head(&runtime.db, &owner, session.id)
            .await
            .unwrap()
            .unwrap();
        // The edit lands after the snapshot: the claim below must not eat it.
        tidebreak_core::db::code::update_queued_turn(
            &runtime.db,
            &owner,
            session.id,
            stale.id,
            Some("edited"),
            None,
        )
        .await
        .unwrap();

        fake.event_reads.lock().unwrap().push_back(SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state: SandboxState::Running,
            latest_event_seq: 2,
            events: vec![
                event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                event(
                    2,
                    "turn_completed",
                    serde_json::json!({ "turn": 1, "exit_code": 0 }),
                ),
            ],
        });
        let mut live = runtime.get_session(&owner, session.id).await.unwrap();
        let driver = remote.driver(&runtime.db, runtime.bus.as_ref());
        driver.pump(&mut live, 0).await.unwrap();

        // Deliver against the stale snapshot, as a sweep that raced the edit
        // would.
        driver
            .submit_turn_from(
                &mut live,
                Some(&workspace),
                Some(&repo),
                None,
                &stale.message,
                Some(&stale),
            )
            .await
            .unwrap();
        let delivered = latest_turn(&runtime.db, &owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.user_input, "original");
        assert_ne!(
            delivered.id, stale.id,
            "a stale claim must not take the row's id"
        );
        let (queued_rows, _) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert_eq!(queued_rows.len(), 1);
        assert_eq!(queued_rows[0].message, "edited");
        assert_eq!(queued_rows[0].id, stale.id);

        // The edited row promotes cleanly afterwards — no duplicate key.
        fake.event_reads.lock().unwrap().push_back(SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state: SandboxState::Running,
            latest_event_seq: 4,
            events: vec![
                event(3, "turn_started", serde_json::json!({ "turn": 2 })),
                event(
                    4,
                    "turn_completed",
                    serde_json::json!({ "turn": 2, "exit_code": 0 }),
                ),
            ],
        });
        let mut live = runtime.get_session(&owner, session.id).await.unwrap();
        driver.pump(&mut live, 0).await.unwrap();
        runtime.promote_remote_queue_heads().await.unwrap();
        let (queued_rows, _) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert!(queued_rows.is_empty());
        let promoted = latest_turn(&runtime.db, &owner, session.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.user_input, "edited");
        assert_eq!(promoted.id, stale.id);
    }

    /// A cap-refused promotion holds instead of re-journaling the refusal
    /// every sweep tick.
    #[tokio::test]
    async fn a_refused_promotion_holds_instead_of_spamming() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let remote = runtime.remote_sessions().unwrap();
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("occupied".into()))
            .await
            .unwrap();
        // Session A holds every cap slot (the test settings cap is 2; take
        // both through direct core reservations).
        for _ in 0..2 {
            let filler = runtime
                .create_remote_session(
                    &owner,
                    None,
                    workspace.id,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                )
                .await
                .unwrap();
            let admission = tidebreak_core::db::code::create_incarnation_intent(
                &runtime.db,
                &owner,
                filler.id,
                1,
                2,
            )
            .await
            .unwrap();
            let tidebreak_core::IncarnationAdmission::Admitted(row) = admission else {
                panic!("expected admission");
            };
            tidebreak_core::db::code::activate_incarnation(&runtime.db, &owner, row.id, "sb-x")
                .await
                .unwrap();
        }
        // Session B is idle with a queued head the sweep wants to promote.
        let blocked = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        let now = chrono::Utc::now();
        tidebreak_core::db::code::enqueue_queued_turn(
            &runtime.db,
            &owner,
            &tidebreak_core::code::QueuedTurn {
                actor: None,
                id: tidebreak_core::TurnId::new(),
                session_id: blocked.id,
                message: "waiting".into(),
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();

        runtime.promote_remote_queue_heads().await.unwrap();
        assert!(remote.promotion_held(blocked.id));
        let notices = |events: &[tidebreak_core::code::SequencedEvent]| {
            events
                .iter()
                .filter(|row| matches!(row.event, tidebreak_core::Event::HarnessNotice { .. }))
                .count()
        };
        let events = tidebreak_core::db::code::list_events(&runtime.db, &owner, blocked.id, 0, 50)
            .await
            .unwrap()
            .events;
        assert_eq!(notices(&events), 1);

        // The next tick skips the held session: no second notice, the row
        // stays queued.
        runtime.promote_remote_queue_heads().await.unwrap();
        let events = tidebreak_core::db::code::list_events(&runtime.db, &owner, blocked.id, 0, 50)
            .await
            .unwrap()
            .events;
        assert_eq!(notices(&events), 1);
        let (queued_rows, _) = runtime.list_queued_turns(&owner, blocked.id).await.unwrap();
        assert_eq!(queued_rows.len(), 1);
    }

    /// A spend-exhausted promotion pauses the queue: the condition is
    /// permanent for the session, so retrying would re-journal the refusal
    /// and re-cancel the sandbox every tick.
    #[tokio::test]
    async fn a_spend_exhausted_promotion_pauses_the_queue() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) = runtime_with_remote(dir.path()).await;
        // Rebuild the runtime context with a session ceiling for this test.
        let fake = Arc::new(FakeProvisioner::default());
        let runtime = {
            let db = runtime.db.clone();
            drop(runtime);
            Arc::new(
                CodeRuntime::new(
                    db,
                    dir.path().to_path_buf(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .with_remote_sessions(RemoteSessions::new(
                    fake.clone(),
                    RemoteSpawnSettings {
                        session_spend_ceiling_microusd: Some(2_000_000),
                        ..settings()
                    },
                )),
            )
        };
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("expensive".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        let admission = tidebreak_core::db::code::create_incarnation_intent(
            &runtime.db,
            &owner,
            session.id,
            1,
            4,
        )
        .await
        .unwrap();
        let tidebreak_core::IncarnationAdmission::Admitted(row) = admission else {
            panic!("expected admission");
        };
        tidebreak_core::db::code::record_incarnation_spend(&runtime.db, &owner, row.id, 2_500_000)
            .await
            .unwrap();
        tidebreak_core::db::code::stop_incarnation(&runtime.db, &owner, row.id, Some("done"))
            .await
            .unwrap();
        let now = chrono::Utc::now();
        tidebreak_core::db::code::enqueue_queued_turn(
            &runtime.db,
            &owner,
            &tidebreak_core::code::QueuedTurn {
                actor: None,
                id: tidebreak_core::TurnId::new(),
                session_id: session.id,
                message: "one more".into(),
                attachments: Vec::new(),
                position: 0,
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();

        runtime.promote_remote_queue_heads().await.unwrap();
        let (queued_rows, paused) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert_eq!(queued_rows.len(), 1);
        assert!(paused, "spend exhaustion must pause the queue");

        // Paused queues are skipped outright: nothing new journals.
        let events = tidebreak_core::db::code::list_events(&runtime.db, &owner, session.id, 0, 50)
            .await
            .unwrap()
            .events;
        let before = events.len();
        runtime.promote_remote_queue_heads().await.unwrap();
        let events = tidebreak_core::db::code::list_events(&runtime.db, &owner, session.id, 0, 50)
            .await
            .unwrap()
            .events;
        assert_eq!(events.len(), before);
    }

    /// A fenced remote session reaps through the driver: the sandbox is
    /// cancelled and no local worker is spawned.
    /// While the environment is unavailable, the events read answers at once
    /// with a retryable fault. The pump must wait between reads rather than
    /// re-issue the request as fast as the store answers; a tight loop here
    /// hammers a transport that just said it is down.
    #[tokio::test]
    async fn an_unavailable_environment_backs_the_pump_off() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);
        // No scripted reads: every events request answers Unavailable.
        assert!(fake.event_reads.lock().unwrap().is_empty());

        let remote = runtime.remote_sessions().unwrap();
        let initial = std::time::Duration::from_millis(40);
        let pump = tokio::spawn(pump_session(
            Arc::downgrade(&runtime),
            remote,
            owner.clone(),
            session.id,
            LaneBackoff::new(initial, initial * 4),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        pump.abort();

        // The waits are at least 20, 40, 80, then 80ms each, so at most
        // seven reads fit in the window. An unpaced loop issues hundreds.
        let reads = *fake.event_reads_issued.lock().unwrap();
        assert!(reads >= 2, "the pump never retried the read");
        assert!(
            reads <= 10,
            "the pump issued {reads} reads in 400ms; it must wait between retryable faults"
        );
    }

    #[tokio::test]
    async fn a_remote_reap_cancels_the_sandbox_and_spawns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        let mut live = runtime.get_session(&owner, session.id).await.unwrap();
        super::super::super::recovery::fence_session(
            &runtime.db,
            runtime.bus.as_ref(),
            &mut live,
            FenceReason::SandboxLost {
                detail: "the environment reports the sandbox failed".to_owned(),
            },
        )
        .await
        .unwrap();

        let reaped = runtime.reap(&owner, session.id).await.unwrap();
        assert_eq!(reaped.lifecycle, SessionLifecycle::Idle);
        assert!(reaped.fence_reason.is_none());
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
    }

    fn idle_status(sandbox_id: &str, latest_event_seq: i64) -> SandboxStatus {
        SandboxStatus {
            sandbox_id: sandbox_id.into(),
            state: SandboxState::Failed,
            failure_reason: Some("idle_ceiling".into()),
            termination_reason: None,
            latest_event_seq,
            pending_messages: 0,
            spend_microusd: Some(100_000),
            spend_ceiling_microusd: Some(5_000_000),
            possibly_stalled: false,
            repository_url: None,
            completed_at: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    #[tokio::test]
    async fn slack_message_after_idle_recovery_preserves_history_and_held_queues() {
        for (pending, manual_pause) in [(false, false), (true, false), (false, true)] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, fake, owner, _) = runtime_with_remote(dir.path()).await;
            let grant = tidebreak_core::CodeGrantId::new();
            let (resolution, _) = runtime
                .external_get_or_create(
                    &owner,
                    None,
                    grant,
                    "slack",
                    "T1/C1/recovery",
                    None,
                    None,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let tidebreak_core::ExternalSessionResolution::Created(binding) = resolution else {
                panic!("expected creation");
            };
            let message = |event: &str| crate::code::runtime::ExternalMessage {
                text: event.into(),
                event_id: event.into(),
                channel_ts: "1.0".into(),
                actor: tidebreak_core::TurnActor::default(),
                context: None,
                steer: false,
                expected_turn_id: None,
                correlation_uuid: None,
            };
            runtime
                .external_submit_message(&owner, grant, binding.session_id, message("start"))
                .await
                .unwrap();
            if pending {
                runtime
                    .external_submit_message(
                        &owner,
                        grant,
                        binding.session_id,
                        message("old queued work"),
                    )
                    .await
                    .unwrap();
            }
            let mut session = runtime
                .get_session(&owner, binding.session_id)
                .await
                .unwrap();
            // Reproduce the real idle-expiry path. The prior implementation
            // reaped an active lease, which hid the stopped-checkpoint refusal.
            fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                sandbox_id: "sb-1".into(),
                state: SandboxState::Running,
                latest_event_seq: 3,
                events: vec![
                    event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                    event(
                        2,
                        "assistant_record",
                        serde_json::json!({ "body": "Previous answer" }),
                    ),
                    event(
                        3,
                        "turn_completed",
                        serde_json::json!({ "turn": 1, "exit_code": 0 }),
                    ),
                ],
            });
            runtime
                .remote_sessions()
                .unwrap()
                .driver(&runtime.db, runtime.bus.as_ref())
                .pump(&mut session, 0)
                .await
                .unwrap();
            assert_eq!(
                latest_turn(&runtime.db, &owner, session.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .status,
                TurnStatus::Completed
            );
            // Expiry arrives on a later read, after the successful turn settled.
            fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                sandbox_id: "sb-1".into(),
                state: SandboxState::Failed,
                latest_event_seq: 4,
                events: vec![event(
                    4,
                    "failed",
                    serde_json::json!({ "reason": "idle_ceiling" }),
                )],
            });
            runtime
                .remote_sessions()
                .unwrap()
                .driver(&runtime.db, runtime.bus.as_ref())
                .pump(&mut session, 0)
                .await
                .unwrap();
            assert_eq!(session.lifecycle, SessionLifecycle::Fenced);
            runtime.recover().await.unwrap();
            assert_eq!(
                runtime
                    .get_session(&owner, session.id)
                    .await
                    .unwrap()
                    .lifecycle,
                SessionLifecycle::Fenced,
                "automatic recovery cannot accept missing output"
            );
            if manual_pause {
                runtime
                    .set_queue_paused(&owner, session.id, true)
                    .await
                    .unwrap();
            }
            assert!(matches!(
                runtime
                    .get_session(&owner, session.id)
                    .await
                    .unwrap()
                    .fence_reason,
                Some(FenceReason::TerminalFlushMissing { .. })
            ));
            *fake.status_override.lock().unwrap() = Some(idle_status("sb-1", 5));
            fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                sandbox_id: "sb-1".into(),
                state: SandboxState::Failed,
                latest_event_seq: 5,
                events: vec![event(
                    5,
                    "container_output",
                    serde_json::json!({ "body": "stopped" }),
                )],
            });
            // Upgrade a persisted fence without an explicit reap or queue reset.
            let runtime = Arc::new(
                CodeRuntime::new(
                    runtime.db.clone(),
                    dir.path().to_path_buf(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .with_remote_sessions(RemoteSessions::new(fake.clone(), settings())),
            );
            runtime.recover().await.unwrap();
            assert_eq!(
                runtime
                    .get_session(&owner, session.id)
                    .await
                    .unwrap()
                    .lifecycle,
                SessionLifecycle::Idle
            );
            assert!(fake.cancels.lock().unwrap().is_empty());
            assert_eq!(
                fake.spawns.lock().unwrap().len(),
                1,
                "recovery must not start work"
            );
            *fake.status_override.lock().unwrap() = None;
            let result = runtime
                .external_submit_message(&owner, grant, session.id, message("fresh retry"))
                .await
                .unwrap();
            if pending || manual_pause {
                assert!(matches!(result, ExternalMessageOutcome::Queued(_)));
                assert_eq!(fake.spawns.lock().unwrap().len(), 1);
                assert!(
                    runtime
                        .list_queued_turns(&owner, session.id)
                        .await
                        .unwrap()
                        .1
                );
            } else {
                assert!(
                    matches!(result, ExternalMessageOutcome::NewTurn(_)),
                    "fresh input must start after recovering an empty queue: {result:?}"
                );
                {
                    let spawns = fake.spawns.lock().unwrap();
                    assert_eq!(spawns.len(), 2);
                    assert!(spawns[1].repository.is_none());
                    assert!(spawns[1].task.contains("Previous answer"));
                    assert!(spawns[1]
                        .task
                        .contains("Temporary files from the previous sandbox are unavailable"));
                }
                let replay = runtime
                    .external_submit_message(&owner, grant, session.id, message("fresh retry"))
                    .await
                    .unwrap();
                let (
                    ExternalMessageOutcome::NewTurn(first),
                    ExternalMessageOutcome::NewTurn(replay),
                ) = (result, replay)
                else {
                    panic!("expected the same accepted turn");
                };
                assert_eq!(first.id, replay.id);
                assert_eq!(
                    fake.spawns.lock().unwrap().len(),
                    2,
                    "duplicate Slack input must not respawn"
                );

                // Recreate the server over its durable database before ingesting
                // the answer. The replacement sandbox starts its own sequence.
                let resumed = CodeRuntime::new(
                    runtime.db.clone(),
                    dir.path().to_path_buf(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .with_remote_sessions(RemoteSessions::new(fake.clone(), settings()));
                resumed.recover().await.unwrap();
                fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                    sandbox_id: "sb-2".into(),
                    state: SandboxState::Running,
                    latest_event_seq: 3,
                    events: vec![
                        event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                        event(
                            2,
                            "assistant_record",
                            serde_json::json!({ "body": "RECOVERY-OK" }),
                        ),
                        event(
                            3,
                            "turn_completed",
                            serde_json::json!({ "turn": 1, "exit_code": 0 }),
                        ),
                    ],
                });
                let mut session = resumed.get_session(&owner, session.id).await.unwrap();
                resumed
                    .remote_sessions()
                    .unwrap()
                    .driver(&resumed.db, resumed.bus.as_ref())
                    .pump(&mut session, 0)
                    .await
                    .unwrap();
                let completed = latest_turn(&resumed.db, &owner, session.id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(completed.id, first.id);
                assert_eq!(completed.status, TurnStatus::Completed);
                let events =
                    tidebreak_core::db::code::list_events(&resumed.db, &owner, session.id, 0, 100)
                        .await
                        .unwrap();
                assert_eq!(events.events.iter().filter(|row| matches!(
                    &row.event, tidebreak_core::Event::AssistantMessage { text, .. } if text == "RECOVERY-OK"
                )).count(), 1, "the recovered answer must be available to Slack and web replay");
                assert!(
                    !runtime
                        .list_queued_turns(&owner, session.id)
                        .await
                        .unwrap()
                        .1
                );
            }
        }
    }

    #[tokio::test]
    async fn idle_recovery_requires_completed_drained_scratch_output() {
        for case in [
            "running_turn",
            "interrupted_turn",
            "pending_message",
            "unread_events",
            "wrong_sandbox",
            "running_sandbox",
            "other_failure",
            "repository_status",
            "repository_workspace",
            "spend_stop",
            "spent_budget",
            "earlier_incarnation",
            "tail_has_turn",
            "tail_wrong_sandbox",
            "tail_not_terminal",
            "tail_partial",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
            let repository = (case == "repository_workspace").then_some(repo.id);
            let grant = tidebreak_core::CodeGrantId::new();
            let (resolution, _) = runtime
                .external_get_or_create(
                    &owner,
                    None,
                    grant,
                    "slack",
                    "T1/C1/refusal",
                    repository,
                    None,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let tidebreak_core::ExternalSessionResolution::Created(binding) = resolution else {
                panic!("expected creation");
            };
            let mut session = runtime
                .get_session(&owner, binding.session_id)
                .await
                .unwrap();
            runtime
                .submit_turn(&owner, session.id, "start".into(), None, None, vec![], None)
                .await
                .unwrap();
            fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                sandbox_id: "sb-1".into(),
                state: SandboxState::Failed,
                latest_event_seq: 4,
                events: vec![
                    event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                    event(
                        2,
                        "assistant_record",
                        serde_json::json!({ "body": "answer" }),
                    ),
                    event(
                        3,
                        "turn_completed",
                        serde_json::json!({ "turn": 1, "exit_code": 0 }),
                    ),
                    event(4, "failed", serde_json::json!({ "reason": "idle_ceiling" })),
                ],
            });
            runtime
                .remote_sessions()
                .unwrap()
                .driver(&runtime.db, runtime.bus.as_ref())
                .pump(&mut session, 0)
                .await
                .unwrap();
            let mut status = idle_status("sb-1", 4);
            match case {
                "pending_message" => status.pending_messages = 1,
                "unread_events" => status.latest_event_seq = 5,
                "wrong_sandbox" => status.sandbox_id = "another-sandbox".into(),
                "running_sandbox" => status.state = SandboxState::Running,
                "other_failure" => status.failure_reason = Some("wall_clock_ceiling".into()),
                "repository_status" => {
                    status.repository_url = Some("https://github.com/test/tools".into())
                }
                "spend_stop" => {
                    status.state = SandboxState::CeilingExceeded;
                    status.failure_reason = Some("spend_ceiling_exceeded".into());
                }
                "spent_budget" => status.spend_microusd = status.spend_ceiling_microusd,
                "running_turn" | "interrupted_turn" => {
                    let mut turn = latest_turn(&runtime.db, &owner, session.id)
                        .await
                        .unwrap()
                        .unwrap();
                    turn.status = if case == "running_turn" {
                        TurnStatus::Running
                    } else {
                        TurnStatus::Interrupted
                    };
                    tidebreak_core::db::code::save_turn(&runtime.db, &owner, &turn)
                        .await
                        .unwrap();
                }
                "earlier_incarnation" => {
                    use tidebreak_core::db::code::{
                        activate_incarnation, create_incarnation_intent, stop_incarnation,
                    };
                    let tidebreak_core::IncarnationAdmission::Admitted(row) =
                        create_incarnation_intent(&runtime.db, &owner, session.id, 2, 2)
                            .await
                            .unwrap()
                    else {
                        panic!("expected incarnation")
                    };
                    activate_incarnation(&runtime.db, &owner, row.id, "sb-2")
                        .await
                        .unwrap();
                    tidebreak_core::db::code::ingest_incarnation_event(
                        &runtime.db,
                        &owner,
                        session.id,
                        session.spawn_epoch,
                        row.id,
                        4,
                        tidebreak_core::db::code::IncarnationSideEffects {
                            journal: &[],
                            task_output: None,
                            wip_ref: None,
                            terminal_events_journaled: false,
                        },
                    )
                    .await
                    .unwrap();
                    stop_incarnation(&runtime.db, &owner, row.id, Some("failed"))
                        .await
                        .unwrap();
                    status.sandbox_id = "sb-2".into();
                }
                "tail_has_turn" | "tail_wrong_sandbox" | "tail_not_terminal" | "tail_partial" => {
                    status.latest_event_seq = 6;
                    fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                        sandbox_id: if case == "tail_wrong_sandbox" {
                            "sb-other"
                        } else {
                            "sb-1"
                        }
                        .into(),
                        state: if case == "tail_not_terminal" {
                            SandboxState::Running
                        } else {
                            SandboxState::Failed
                        },
                        latest_event_seq: 6,
                        events: vec![event(
                            5,
                            if case == "tail_has_turn" {
                                "turn_started"
                            } else {
                                "container_output"
                            },
                            serde_json::json!({ "body": "late diagnostic", "turn": 2 }),
                        )],
                    });
                }
                "repository_workspace" => {}
                _ => unreachable!(),
            }
            *fake.status_override.lock().unwrap() = Some(status);
            let status_reads = *fake.status_reads.lock().unwrap();
            // Recovery sees the same persisted fence on subsequent sweeps.
            runtime.recover().await.unwrap();
            runtime.recover().await.unwrap();
            if case == "other_failure" {
                assert_eq!(
                    *fake.status_reads.lock().unwrap(),
                    status_reads + 1,
                    "repeated sweeps must pace status probes"
                );
            }
            let current = runtime.get_session(&owner, session.id).await.unwrap();
            assert_eq!(current.lifecycle, SessionLifecycle::Fenced, "{case}");
            let row = tidebreak_core::db::code::latest_incarnation(&runtime.db, &owner, session.id)
                .await
                .unwrap()
                .unwrap();
            assert!(!row.terminal_events_journaled, "{case}");
            assert_eq!(fake.spawns.lock().unwrap().len(), 1, "{case}");
            assert!(fake.cancels.lock().unwrap().is_empty(), "{case}");
        }
    }

    /// Stop on a remote session sends an interrupt to the sandbox instead of
    /// looking for a host worker.
    #[tokio::test]
    async fn a_remote_interrupt_reaches_the_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        runtime.interrupt(session.id).await.unwrap();
        assert_eq!(fake.sends.lock().unwrap().as_slice(), &["stop".to_owned()]);
    }

    /// Changing permission mode on a remote session does not launch a host
    /// harness against the empty worktree.
    #[tokio::test]
    async fn a_remote_permission_mode_change_does_not_spawn_locally() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        let updated = runtime
            .set_permission_mode(&owner, session.id, PermissionMode::Ask)
            .await
            .unwrap();
        assert_eq!(updated.permission_mode, PermissionMode::Ask);
        let stored = runtime.get_session(&owner, session.id).await.unwrap();
        assert_eq!(stored.permission_mode, PermissionMode::Ask);
        assert!(fake.spawns.lock().unwrap().is_empty());
    }

    /// Remote trigger and attachment delivery stay refused until the runtime
    /// can preserve their delivery contracts. Neither refusal may reach the
    /// sandbox or create a turn row.
    #[tokio::test]
    async fn remote_inputs_without_transport_contracts_are_refused_before_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();

        let trigger_error = match runtime
            .submit_trigger_turn(
                &owner,
                session.id,
                "review changed".into(),
                "Trigger: checks failed",
                tidebreak_core::CodeTriggerDeliveryId::new(),
                uuid::Uuid::new_v4(),
            )
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("remote trigger delivery must refuse"),
        };
        assert_eq!(trigger_error.kind(), "remote_triggers_unsupported");
        assert!(
            trigger_error.message().contains("idempotency key"),
            "{}",
            trigger_error.message()
        );

        let attachment_error = match runtime
            .submit_turn(
                &owner,
                session.id,
                "inspect this".into(),
                None,
                None,
                vec![tidebreak_core::ImageRef {
                    blob_id: uuid::Uuid::new_v4(),
                    media_type: tidebreak_core::ImageMediaType::Png,
                    width: 1,
                    height: 1,
                    byte_len: 1,
                }],
                None,
            )
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("remote attachment delivery must refuse"),
        };
        assert_eq!(attachment_error.kind(), "remote_attachments_unsupported");
        assert!(
            attachment_error.message().contains("carries text only"),
            "{}",
            attachment_error.message()
        );

        assert!(fake.spawns.lock().unwrap().is_empty());
        assert!(fake.sends.lock().unwrap().is_empty());
        assert!(latest_turn(&runtime.db, &owner, session.id)
            .await
            .unwrap()
            .is_none());
    }

    /// Host worktree reads refuse a remote workspace with the remote marker
    /// instead of treating the empty path as a missing checkout.
    #[tokio::test]
    async fn a_remote_workspace_tree_refuses_as_remote() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let error = runtime
            .workspace_tree(&owner, workspace.id, "", None)
            .await
            .unwrap_err();
        assert!(error.message().contains("remote sandbox"));
    }

    /// Ending a remote session cancels the sandbox so it does not keep spending.
    #[tokio::test]
    async fn ending_a_remote_session_cancels_the_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        let session = runtime
            .create_remote_session(
                &owner,
                None,
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(
                &owner,
                session.id,
                "start".into(),
                None,
                None,
                Vec::new(),
                None,
            )
            .await
            .unwrap();
        runtime.end_session_row(&owner, session.id).await.unwrap();
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
    }

    /// External get-or-create builds everything on first contact, is
    /// idempotent on the conversation key, scopes by grant, and never
    /// resurrects an ended session.
    #[tokio::test]
    async fn an_external_conversation_binds_once_and_scopes_by_grant() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let grant = tidebreak_core::CodeGrantId::new();
        let (resolved, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                Some("fix the flake".into()),
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Created(binding) = resolved else {
            panic!("expected a create");
        };
        let session = runtime
            .get_session(&owner, binding.session_id)
            .await
            .unwrap();
        assert_eq!(session.lifecycle, SessionLifecycle::Idle);
        let workspace = runtime
            .get_workspace(&owner, session.workspace_id.expect("workspace"))
            .await
            .unwrap();
        assert!(workspace.is_remote());

        // The channel's retry answers with the same session.
        let (again, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Existing(hit) = again else {
            panic!("expected the existing binding");
        };
        assert_eq!(hit.session_id, binding.session_id);

        // Another grant's call on the same conversation refuses.
        let (refused, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                tidebreak_core::CodeGrantId::new(),
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(matches!(
            refused,
            tidebreak_core::ExternalSessionResolution::GrantMismatch
        ));

        // An ended session answers `Ended` rather than resurrecting.
        let mut stored = runtime
            .get_session(&owner, binding.session_id)
            .await
            .unwrap();
        stored.lifecycle = SessionLifecycle::Ended;
        assert!(tidebreak_core::db::code::save_session(&runtime.db, &stored)
            .await
            .unwrap());
        let (ended, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            ended,
            tidebreak_core::ExternalSessionResolution::Ended {
                session_id: binding.session_id
            }
        );
    }

    #[tokio::test]
    async fn a_repositoryless_managed_slack_message_spawns_with_native_tools_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let mut spawn_settings = settings();
        spawn_settings.engine = Some(HarnessKind::ClaudeCode);
        spawn_settings.engines = Some(vec![HarnessKind::ClaudeCode, HarnessKind::Codex]);
        spawn_settings.embedded_engine_registration = true;
        let (runtime, fake, owner, _) =
            runtime_with_remote_settings(dir.path(), spawn_settings).await;
        let session_tools = Arc::new(crate::code::session_tools::SessionTools::default());
        let conversation_tools =
            Arc::new(crate::code::conversation_tools::ConversationTools::default());
        let mut tools = tidebreak_core::ToolRegistry::new();
        session_tools.register(&mut tools);
        conversation_tools.register(&mut tools);
        let runtime = Arc::new(
            Arc::try_unwrap(runtime)
                .unwrap_or_else(|_| panic!("fixture runtime must have one owner"))
                .with_tool_registry(Arc::new(tools)),
        );
        session_tools.attach(&runtime);
        conversation_tools.attach(&runtime);
        runtime.remote_sessions().unwrap().with_host_tool(Arc::new(
            crate::code::sandbox_tools::SandboxToolExecutor::new(Arc::downgrade(&runtime)),
        ));
        let (grant, _) = runtime
            .mint_adapter_grant(&owner, "slack", "U1", "T1")
            .await
            .unwrap();
        let (resolved, _) = runtime
            .external_get_or_create_with_channel_context(
                &owner,
                None,
                grant.id,
                "slack",
                "T1/C1/first",
                None,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
                Some(tidebreak_core::db::code::ExternalSessionChannelContext {
                    channel_id: Some("C1"),
                    instructions: "Keep answers concise.",
                }),
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Created(binding) = resolved else {
            panic!("expected a create");
        };
        assert!(fake.spawns.lock().unwrap().is_empty());
        let message = || crate::code::runtime::ExternalMessage {
            text: "Read this thread and say hello.".into(),
            event_id: "EvFirst".into(),
            channel_ts: "1700000001.000100".into(),
            actor: tidebreak_core::TurnActor::default(),
            context: None,
            steer: false,
            expected_turn_id: None,
            correlation_uuid: None,
        };
        let first = runtime
            .external_submit_message(&owner, grant.id, binding.session_id, message())
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(turn) = first else {
            panic!("the managed first message must spawn, got {first:?}");
        };
        {
            let spawns = fake.spawns.lock().unwrap();
            assert_eq!(spawns.len(), 1);
            assert!(spawns[0].repository.is_none());
            assert!(spawns[0].model.is_none(), "an unset harness model is valid");
            assert!(spawns[0].task.contains("Keep answers concise."));
            for name in tidebreak_core::code::supervisor_tools::TOOLS {
                assert!(
                    spawns[0].task.contains(name),
                    "missing bootstrap schema {name}"
                );
            }
            assert!(spawns[0].task.contains("Read this thread and say hello."));
            assert!(spawns[0].embedded_engine.is_some());
        }
        let replay = runtime
            .external_submit_message(&owner, grant.id, binding.session_id, message())
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(replayed) = replay else {
            panic!("the duplicate must resolve the first turn, got {replay:?}");
        };
        assert_eq!(turn.id, replayed.id);
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn external_steering_rejects_missing_target_and_replays_original_receipt() {
        use tidebreak_core::code::ExternalSteerQueuedReason;
        for (harness, compatible, stale, oversized_frame) in [
            (HarnessKind::ClaudeCode, false, false, false),
            (HarnessKind::Codex, false, false, false),
            (HarnessKind::Codex, true, false, false),
            (HarnessKind::Codex, true, true, false),
            (HarnessKind::Codex, true, false, true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut spawn_settings = settings();
            spawn_settings.engine = Some(harness);
            spawn_settings.engines = Some(vec![harness]);
            spawn_settings.embedded_engine_registration = true;
            let (runtime, fake, owner, _) =
                runtime_with_remote_settings(dir.path(), spawn_settings).await;
            let (grant, _) = runtime
                .mint_adapter_grant(&owner, "slack", "U1", "T1")
                .await
                .unwrap();
            let (resolution, _) = runtime
                .external_get_or_create(
                    &owner,
                    None,
                    grant.id,
                    "slack",
                    "T1/C1/steer",
                    None,
                    None,
                    harness,
                    session_settings(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let tidebreak_core::ExternalSessionResolution::Created(binding) = resolution else {
                panic!("expected creation");
            };
            let make_message =
                |event: &str, steer, target, correlation| crate::code::runtime::ExternalMessage {
                    // Escaping exceeds the frame limit while the original text still fits.
                    text: if oversized_frame && event == "steer" {
                        "\"".repeat(20 * 1024)
                    } else {
                        "Read the thread.".into()
                    },
                    event_id: event.into(),
                    channel_ts: "1.0".into(),
                    actor: tidebreak_core::TurnActor::default(),
                    context: None,
                    steer,
                    expected_turn_id: target,
                    correlation_uuid: correlation,
                };
            let invalid = runtime
                .external_submit_message(
                    &owner,
                    grant.id,
                    binding.session_id,
                    make_message("invalid", true, None, None),
                )
                .await;
            assert!(invalid.is_err());
            assert!(tidebreak_core::db::code::list_queued_turns(
                &runtime.db,
                &owner,
                binding.session_id
            )
            .await
            .unwrap()
            .is_empty());
            assert!(fake.spawns.lock().unwrap().is_empty());
            let first = runtime
                .external_submit_message(
                    &owner,
                    grant.id,
                    binding.session_id,
                    make_message("first", false, None, None),
                )
                .await
                .unwrap();
            let ExternalMessageOutcome::NewTurn(active) = first else {
                panic!("expected active turn: {first:?}");
            };
            if compatible {
                fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                    sandbox_id: "sb-1".into(), state: SandboxState::Running, latest_event_seq: 1,
                    events: vec![event(1, "supervisor_started", serde_json::json!({"agent":"tidebreak-supervised-agent", "steering_protocol":1,"runtime_id":uuid::Uuid::new_v4().to_string()}))],
                });
                let remote = runtime.remote_sessions().unwrap();
                let mut session = runtime
                    .get_session(&owner, binding.session_id)
                    .await
                    .unwrap();
                remote
                    .driver(&runtime.db, runtime.bus.as_ref())
                    .pump(&mut session, 0)
                    .await
                    .unwrap();
            }
            let correlation = uuid::Uuid::new_v4();
            let expected_turn = if stale {
                tidebreak_core::TurnId::new()
            } else {
                active.id
            };
            let first_steer = runtime
                .external_submit_message(
                    &owner,
                    grant.id,
                    binding.session_id,
                    make_message("steer", true, Some(expected_turn), Some(correlation)),
                )
                .await
                .unwrap();
            let expected_reason = if stale || oversized_frame {
                ExternalSteerQueuedReason::StaleTurn
            } else if compatible {
                ExternalSteerQueuedReason::Unacknowledged
            } else {
                ExternalSteerQueuedReason::SteerUnsupported
            };
            let ExternalMessageOutcome::SteerQueued { turn_id, reason } = first_steer else {
                panic!("expected queued receipt: {first_steer:?}");
            };
            assert_eq!(reason, expected_reason);
            let sends = fake.sends.lock().unwrap().len();
            assert_eq!(sends, usize::from(compatible && !stale && !oversized_frame));
            let tail = runtime
                .external_submit_message(
                    &owner,
                    grant.id,
                    binding.session_id,
                    make_message("later", false, None, None),
                )
                .await
                .unwrap();
            let ExternalMessageOutcome::Queued(tail) = tail else {
                panic!("expected later message to queue: {tail:?}");
            };
            let rows = tidebreak_core::db::code::list_queued_turns(
                &runtime.db,
                &owner,
                binding.session_id,
            )
            .await
            .unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].id, turn_id);
            assert_eq!(rows[1].id, tail.id);
            let replay = runtime
                .external_submit_message(
                    &owner,
                    grant.id,
                    binding.session_id,
                    make_message(
                        "steer",
                        true,
                        Some(tidebreak_core::TurnId::new()),
                        Some(uuid::Uuid::new_v4()),
                    ),
                )
                .await
                .unwrap();
            assert!(
                matches!(replay, ExternalMessageOutcome::SteerQueued { turn_id: replayed, reason } if replayed == turn_id && reason == expected_reason)
            );
            assert_eq!(
                fake.sends.lock().unwrap().len(),
                sends,
                "a replay must not dispatch again"
            );
            if compatible && !stale && !oversized_frame {
                let target = tidebreak_core::db::code::external_steer_target_by_correlation(
                    &runtime.db,
                    &owner,
                    binding.session_id,
                    correlation,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(target.expected_turn_id, active.id);
                assert!(
                    tidebreak_core::db::code::queued_turn_head(
                        &runtime.db,
                        &owner,
                        binding.session_id
                    )
                    .await
                    .unwrap()
                    .is_none(),
                    "unknown delivery holds the queue"
                );
            } else {
                assert_eq!(
                    tidebreak_core::db::code::queued_turn_head(
                        &runtime.db,
                        &owner,
                        binding.session_id
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                    turn_id,
                    "proven fallback releases the original head before the later message"
                );
                // Once fallback promotes, its original admission receipt still replays.
                fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                    sandbox_id: "sb-1".into(),
                    state: SandboxState::Running,
                    latest_event_seq: 3,
                    events: vec![
                        event(2, "turn_started", serde_json::json!({"turn":1})),
                        event(
                            3,
                            "turn_completed",
                            serde_json::json!({"turn":1,"exit_code":0}),
                        ),
                    ],
                });
                let remote = runtime.remote_sessions().unwrap();
                let mut session = runtime
                    .get_session(&owner, binding.session_id)
                    .await
                    .unwrap();
                remote
                    .driver(&runtime.db, runtime.bus.as_ref())
                    .pump(&mut session, 0)
                    .await
                    .unwrap();
                runtime.promote_remote_queue_heads().await.unwrap();
                assert_eq!(
                    tidebreak_core::db::code::get_open_turn(
                        &runtime.db,
                        &owner,
                        binding.session_id,
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                    turn_id
                );
                let remaining = tidebreak_core::db::code::list_queued_turns(
                    &runtime.db,
                    &owner,
                    binding.session_id,
                )
                .await
                .unwrap();
                assert_eq!(remaining.len(), 1);
                assert_eq!(remaining[0].id, tail.id);
                let replay = runtime
                    .external_submit_message(
                        &owner,
                        grant.id,
                        binding.session_id,
                        make_message("steer", true, Some(active.id), Some(correlation)),
                    )
                    .await
                    .unwrap();
                assert!(
                    matches!(replay, ExternalMessageOutcome::SteerQueued { turn_id: replayed, reason } if replayed == turn_id && reason == expected_reason)
                );
                fake.event_reads.lock().unwrap().push_back(SandboxEvents {
                    sandbox_id: "sb-1".into(),
                    state: SandboxState::Running,
                    latest_event_seq: 5,
                    events: vec![
                        event(4, "turn_started", serde_json::json!({"turn":2})),
                        event(
                            5,
                            "turn_completed",
                            serde_json::json!({"turn":2,"exit_code":0}),
                        ),
                    ],
                });
                session = runtime
                    .get_session(&owner, binding.session_id)
                    .await
                    .unwrap();
                remote
                    .driver(&runtime.db, runtime.bus.as_ref())
                    .pump(&mut session, 0)
                    .await
                    .unwrap();
                runtime.promote_remote_queue_heads().await.unwrap();
                assert_eq!(
                    tidebreak_core::db::code::get_open_turn(
                        &runtime.db,
                        &owner,
                        binding.session_id,
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .id,
                    tail.id
                );
            }
        }
    }

    #[tokio::test]
    async fn startup_failures_are_visible_safe_deduplicated_and_retryable_from_slack() {
        for (sign_in, pinned) in [(false, false), (true, false), (false, true), (true, true)] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, fake, owner, _) = runtime_with_remote(dir.path()).await;
            let remote = runtime.remote_sessions().unwrap();
            let grant = tidebreak_core::CodeGrantId::new();
            let (resolution, _) = runtime
                .external_get_or_create(
                    &owner,
                    None,
                    grant,
                    "slack",
                    "T1/C1/startup",
                    None,
                    None,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let tidebreak_core::ExternalSessionResolution::Created(binding) = resolution else {
                panic!("expected creation");
            };
            if pinned {
                crate::code::attention::apply_attention(
                    &runtime.db,
                    &runtime.bus,
                    &owner,
                    binding.session_id,
                    tidebreak_core::Attention::manual("Keep this pinned"),
                    true,
                )
                .await
                .unwrap();
            }
            for _ in 0..2 {
                fake.spawn_errors.lock().unwrap().push_back(if sign_in {
                    RemoteSandboxError::SignInRequired("private-token-and-upstream-body".into())
                } else {
                    RemoteSandboxError::Unavailable {
                        operation: "spawn",
                        detail: "private-token-and-upstream-body".into(),
                    }
                });
            }
            let message = |event: &str| crate::code::runtime::ExternalMessage {
                text: "start work".into(),
                event_id: event.into(),
                channel_ts: "1.0".into(),
                actor: tidebreak_core::TurnActor::default(),
                context: None,
                steer: false,
                expected_turn_id: None,
                correlation_uuid: None,
            };
            let first = runtime
                .external_submit_message(&owner, grant, binding.session_id, message("Ev1"))
                .await
                .unwrap();
            let ExternalMessageOutcome::Queued(first) = first else {
                panic!("failed startup stays queued");
            };
            assert!(remote.promotion_held(binding.session_id));
            assert!(latest_turn(&runtime.db, &owner, binding.session_id)
                .await
                .unwrap()
                .is_none());
            assert!(!tidebreak_core::db::code::queue_paused(
                &runtime.db,
                &owner,
                binding.session_id
            )
            .await
            .unwrap());
            runtime.promote_remote_queue_heads().await.unwrap();
            assert_eq!(
                fake.spawns.lock().unwrap().len(),
                1,
                "held sweep must not retry"
            );
            remote.clear_promotion_hold(binding.session_id);
            runtime.promote_remote_queue_heads().await.unwrap();
            assert_eq!(
                fake.spawns.lock().unwrap().len(),
                2,
                "automatic retry is allowed after hold"
            );
            let events = tidebreak_core::db::code::list_events(
                &runtime.db,
                &owner,
                binding.session_id,
                0,
                50,
            )
            .await
            .unwrap()
            .events;
            let notices: Vec<_> = events
                .iter()
                .filter_map(|row| match &row.event {
                    tidebreak_core::Event::HarnessNotice {
                        level: tidebreak_core::HarnessNoticeLevel::Warning,
                        message,
                    } => Some(message),
                    _ => None,
                })
                .collect();
            assert_eq!(
                notices.len(),
                1,
                "identical failures must not repeat a notice"
            );
            assert!(notices[0].contains(if sign_in {
                "sandbox_sign_in_required"
            } else {
                "sandbox_start_failed:store"
            }));
            assert!(!serde_json::to_string(&events)
                .unwrap()
                .contains("private-token-and-upstream-body"));
            let replay = runtime
                .external_submit_message(&owner, grant, binding.session_id, message("Ev1"))
                .await
                .unwrap();
            assert!(matches!(replay, ExternalMessageOutcome::Queued(_)));
            assert_eq!(
                fake.spawns.lock().unwrap().len(),
                2,
                "a transport replay cannot retry startup"
            );
            runtime
                .external_submit_message(&owner, grant, binding.session_id, message("Ev2"))
                .await
                .unwrap();
            assert_eq!(
                fake.spawns.lock().unwrap().len(),
                3,
                "a fresh message retries immediately"
            );
            let started = latest_turn(&runtime.db, &owner, binding.session_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                started.id, first.id,
                "retry promotes the original queue head"
            );
            assert!(!remote.promotion_held(binding.session_id));
            if pinned {
                assert_eq!(
                    runtime
                        .get_session(&owner, binding.session_id)
                        .await
                        .unwrap()
                        .attention,
                    tidebreak_core::Attention::manual("Keep this pinned"),
                    "startup and recovery preserve the manual pin"
                );
            }
            let queue = tidebreak_core::db::code::list_queued_turns(
                &runtime.db,
                &owner,
                binding.session_id,
            )
            .await
            .unwrap();
            assert_eq!(
                queue.len(),
                1,
                "the retry message follows the original turn"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_startup_retries_publish_failure_before_the_next_attempt() {
        for failures in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let (runtime, fake, owner, _) = runtime_with_remote(dir.path()).await;
            let remote = runtime.remote_sessions().unwrap();
            let grant = tidebreak_core::CodeGrantId::new();
            let (resolution, _) = runtime
                .external_get_or_create(
                    &owner,
                    None,
                    grant,
                    "slack",
                    "T1/C1/concurrent-startup",
                    None,
                    None,
                    HarnessKind::ClaudeCode,
                    session_settings(),
                    None,
                    None,
                )
                .await
                .unwrap();
            let tidebreak_core::ExternalSessionResolution::Created(binding) = resolution else {
                panic!("expected creation");
            };
            for _ in 0..failures {
                fake.spawn_errors
                    .lock()
                    .unwrap()
                    .push_back(RemoteSandboxError::Unavailable {
                        operation: "spawn",
                        detail: "private transport failure".into(),
                    });
            }
            let message = |event: &str| crate::code::runtime::ExternalMessage {
                text: "start work".into(),
                event_id: event.into(),
                channel_ts: "1.0".into(),
                actor: tidebreak_core::TurnActor::default(),
                context: None,
                steer: false,
                expected_turn_id: None,
                correlation_uuid: None,
            };
            let lock = remote.promotion_lock(binding.session_id);
            let guard = lock.lock().await;
            let (first, second, ()) =
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::join!(
                        runtime.external_submit_message(
                            &owner,
                            grant,
                            binding.session_id,
                            message("Ev1")
                        ),
                        runtime.external_submit_message(
                            &owner,
                            grant,
                            binding.session_id,
                            message("Ev2")
                        ),
                        async {
                            // Both requests must queue before either can attempt startup.
                            loop {
                                let queue = tidebreak_core::db::code::list_queued_turns(
                                    &runtime.db,
                                    &owner,
                                    binding.session_id,
                                )
                                .await
                                .unwrap();
                                if queue.len() == 2 {
                                    break;
                                }
                                tokio::task::yield_now().await;
                            }
                            assert!(fake.spawns.lock().unwrap().is_empty());
                            drop(guard);
                        }
                    )
                })
                .await
                .expect("concurrent retries must finish");
            first.unwrap();
            second.unwrap();
            assert_eq!(fake.spawns.lock().unwrap().len(), 2);
            let events = tidebreak_core::db::code::list_events(
                &runtime.db,
                &owner,
                binding.session_id,
                0,
                50,
            )
            .await
            .unwrap()
            .events;
            assert_eq!(
                events
                    .iter()
                    .filter(|row| matches!(
                        row.event,
                        tidebreak_core::Event::HarnessNotice {
                            level: tidebreak_core::HarnessNoticeLevel::Warning,
                            ..
                        }
                    ))
                    .count(),
                1
            );
            assert_eq!(remote.promotion_held(binding.session_id), failures == 2);
            assert_eq!(
                remote
                    .startup_failures
                    .lock()
                    .unwrap()
                    .contains_key(&binding.session_id),
                failures == 2
            );
            let turn = latest_turn(&runtime.db, &owner, binding.session_id)
                .await
                .unwrap();
            assert_eq!(turn.is_some(), failures == 1);
        }
    }

    /// External messages are idempotent on the event id: an idle session
    /// runs the message as a turn, a replay answers with that same turn,
    /// a busy session queues durably, and another grant cannot submit.
    #[tokio::test]
    async fn an_external_message_is_idempotent_and_queues_busy() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let grant = tidebreak_core::CodeGrantId::new();
        let (resolved, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                grant,
                "slack",
                "T1/C9/77.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Created(binding) = resolved else {
            panic!("expected a create");
        };
        let session_id = binding.session_id;

        let first = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: Some(tidebreak_core::code::ExternalThreadContext {
                        binding_id: binding.id,
                        grant_id: grant,
                        messages: vec![tidebreak_core::code::ExternalContextMessage {
                            author: "Casey".into(),
                            timestamp: "1700000000.000100".into(),
                            text: "The button fails on mobile.".into(),
                        }],
                    }),
                    text: "start".into(),
                    event_id: "Ev1".to_owned(),
                    channel_ts: "1700000001.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(turn) = first else {
            panic!("an idle session must run the message, got {first:?}");
        };
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);
        assert!(turn.user_input.contains("Untrusted thread context"));
        assert!(turn.user_input.contains("The button fails on mobile."));
        assert!(turn.user_input.ends_with("Current request:\nstart"));
        assert!(fake.spawns.lock().unwrap()[0]
            .task
            .contains("The button fails on mobile."));

        // The channel redelivers Ev1: same turn, no second spawn or row.
        let replay = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "start".into(),
                    event_id: "Ev1".to_owned(),
                    channel_ts: "1700000001.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(replayed) = replay else {
            panic!("a replay must answer with the promoted turn, got {replay:?}");
        };
        assert_eq!(replayed.id, turn.id);
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);

        // The session is busy: the next event parks durably, and its replay
        // answers with the same row.
        let busy = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "and then".into(),
                    event_id: "Ev2".to_owned(),
                    channel_ts: "1700000002.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::Queued(row) = busy else {
            panic!("a busy session must queue, got {busy:?}");
        };
        let busy_replay = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "and then".into(),
                    event_id: "Ev2".to_owned(),
                    channel_ts: "1700000002.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::Queued(replayed_row) = busy_replay else {
            panic!("a replayed queued event must answer queued, got {busy_replay:?}");
        };
        assert_eq!(replayed_row.id, row.id);
        let (queued_rows, _) = runtime.list_queued_turns(&owner, session_id).await.unwrap();
        assert_eq!(queued_rows.len(), 1);

        // Another grant holds no binding to this session and cannot submit.
        let foreign = runtime
            .external_submit_message(
                &owner,
                tidebreak_core::CodeGrantId::new(),
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "hijack".into(),
                    event_id: "Ev3".to_owned(),
                    channel_ts: "1700000003.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await;
        assert!(foreign.is_err(), "a foreign grant must refuse");

        // An ended session refuses instead of queueing into the void.
        let mut stored = runtime.get_session(&owner, session_id).await.unwrap();
        stored.lifecycle = SessionLifecycle::Ended;
        assert!(tidebreak_core::db::code::save_session(&runtime.db, &stored)
            .await
            .unwrap());
        let ended = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "still there?".into(),
                    event_id: "Ev4".to_owned(),
                    channel_ts: "1700000004.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await;
        assert!(ended.is_err(), "an ended session must refuse");
    }

    /// The race the channel's out-of-order delivery creates: the sweep
    /// snapshots head B, message A arrives with an earlier `channel_ts` and
    /// moves B, and the claim then runs with a stale position. B's text has
    /// already reached the sandbox, so B's row must be consumed under its
    /// own id — a surviving row would promote later and run the same
    /// message twice — while A stays queued for the next idle.
    #[tokio::test]
    async fn a_moved_head_is_consumed_by_its_promotion_not_run_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let grant = tidebreak_core::CodeGrantId::new();
        let (resolved, _) = runtime
            .external_get_or_create(
                &owner,
                None,
                grant,
                "slack",
                "T1/C4/5.5",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
                None,
                None,
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Created(binding) = resolved else {
            panic!("expected a create");
        };
        let session_id = binding.session_id;

        // Turn 1 runs; B parks behind it.
        runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "start".into(),
                    event_id: "Ev0".to_owned(),
                    channel_ts: "1700000000.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let queued_b = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "message B".into(),
                    event_id: "EvB".to_owned(),
                    channel_ts: "1700000002.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::Queued(row_b) = queued_b else {
            panic!("B must queue behind the running turn");
        };

        // The sweep's snapshot of head B, taken before A arrives.
        let stale = tidebreak_core::db::code::queued_turn_head(&runtime.db, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stale.id, row_b.id);

        // A lands with an earlier channel token and moves B.
        runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "message A".into(),
                    event_id: "EvA".to_owned(),
                    channel_ts: "1700000001.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();

        // Turn 1 settles; the session is idle for the promotion.
        fake.event_reads.lock().unwrap().push_back(SandboxEvents {
            sandbox_id: "sb-1".to_owned(),
            state: SandboxState::Running,
            latest_event_seq: 2,
            events: vec![
                event(1, "turn_started", serde_json::json!({ "turn": 1 })),
                event(
                    2,
                    "turn_completed",
                    serde_json::json!({ "turn": 1, "exit_code": 0 }),
                ),
            ],
        });
        let mut live = runtime.get_session(&owner, session_id).await.unwrap();
        let remote = runtime.remote_sessions().unwrap();
        let driver = remote.driver(&runtime.db, runtime.bus.as_ref());
        driver.pump(&mut live, 0).await.unwrap();

        // The promotion runs with the stale snapshot: B's text goes to the
        // sandbox, and the claim must consume B's row under its own id.
        let workspace = runtime
            .get_workspace(&owner, live.workspace_id.expect("workspace"))
            .await
            .unwrap();
        let stored_repo = runtime.get_repo(&owner, workspace.repo_id).await.unwrap();
        let mut promoting = runtime.get_session(&owner, session_id).await.unwrap();
        let outcome = driver
            .submit_turn_from(
                &mut promoting,
                Some(&workspace),
                Some(&stored_repo),
                None,
                &stale.message,
                Some(&stale),
            )
            .await
            .unwrap();
        let super::super::driver::RemoteTurnOutcome::Delivered { turn } = outcome else {
            panic!("the promotion must deliver, got {outcome:?}");
        };
        assert_eq!(
            turn.id, row_b.id,
            "a moved row promotes under its own id, keeping the event linkage"
        );
        let remaining =
            tidebreak_core::db::code::list_queued_turns(&runtime.db, &owner, session_id)
                .await
                .unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|row| row.message.as_str())
                .collect::<Vec<_>>(),
            vec!["message A"],
            "B's row must be consumed; only A stays queued"
        );

        // The channel's replay of EvB now answers the promoted turn.
        let replay = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                crate::code::runtime::ExternalMessage {
                    context: None,
                    text: "message B".into(),
                    event_id: "EvB".to_owned(),
                    channel_ts: "1700000002.000100".to_owned(),
                    actor: tidebreak_core::TurnActor::default(),
                    steer: false,
                    expected_turn_id: None,
                    correlation_uuid: None,
                },
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(replayed) = replay else {
            panic!("EvB's replay must answer the promoted turn, got {replay:?}");
        };
        assert_eq!(replayed.id, row_b.id);
    }

    /// A repository with no recorded origin cannot back a remote workspace:
    /// the sandbox would have nothing to clone.
    #[tokio::test]
    async fn a_remote_workspace_requires_a_recorded_origin() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, _repo) = runtime_with_remote(dir.path()).await;
        let local_only = CodeRepo {
            id: RepoId::new(),
            owner: owner.clone(),
            root_path: dir.path().join("local").display().to_string(),
            display_name: "local".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        };
        insert_repo(&runtime.db, &local_only).await.unwrap();
        let refused = runtime
            .create_remote_workspace(&owner, local_only.id, None)
            .await;
        assert!(refused.is_err());
    }
}
