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
use tracing::warn;

use tidebreak_core::db::code::{get_session, latest_incarnations_of_live_sessions_all_owners};
use tidebreak_core::{CodeSessionId, CodeSessionLifecycle, DbStore, IncarnationState, OwnerId};

use super::super::runtime::CodeRuntime;
use super::driver::{sweep_stale_intents, RemoteDriver, RemoteSpawnSettings};
use super::SandboxProvisioner;

/// The safety-net interval between passes while an incarnation is draining.
/// Wakes carry the normal traffic; this bounds how long a missed wake or a
/// pump that exited without one can go unnoticed.
const ACTIVE_SWEEP_FLOOR: std::time::Duration = std::time::Duration::from_secs(2);

/// The safety-net interval between passes while nothing remote is draining.
/// The stale-intent cutoff is measured in minutes, so this loses nothing.
const IDLE_SWEEP_FLOOR: std::time::Duration = std::time::Duration::from_secs(20);

/// The held wait a pump task asks the events read to hold.
const PUMP_HELD_WAIT_SECONDS: u16 = 20;

/// How long a pump task sleeps after a transport fault before retrying.
const PUMP_FAULT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);

/// How long promotion leaves a session alone after a machine-side refusal
/// (cap full, sign-in needed). Refusals journal a notice and poke attention;
/// retrying every sweep tick would repeat both every two seconds.
pub(crate) const PROMOTION_RETRY_HOLD: std::time::Duration = std::time::Duration::from_secs(150);

/// Resolve one deployment's remote spawn settings from boot configuration.
/// The per-spawn ceiling bounds one runaway incarnation. The per-session
/// ledger bounds their sum because reincarnation multiplies the former.
pub(crate) fn configured_settings(
    profile: String,
    config: &tidebreak_core::Config,
) -> RemoteSpawnSettings {
    RemoteSpawnSettings {
        profile,
        incarnation_cap: config.runtime_concurrency_cap,
        spend_ceiling_microusd: config.runtime_spawn_spend_ceiling_microusd,
        session_spend_ceiling_microusd: config.runtime_session_spend_ceiling_microusd,
    }
}

/// The remote-session context one deployment configures: the transport and
/// the spawn settings, plus the live pump tasks the sweep reconciles.
pub(crate) struct RemoteSessions {
    /// The environment transport.
    pub(crate) provisioner: Arc<dyn SandboxProvisioner>,
    /// Spawn-time settings.
    pub(crate) settings: RemoteSpawnSettings,
    /// Live pump tasks by session. The sweep prunes finished entries and
    /// spawns missing ones; a pump task removes its own entry on the way out
    /// so the pass it wakes sees the slot free.
    pumps: Mutex<HashMap<CodeSessionId, tokio::task::JoinHandle<()>>>,
    /// Sessions whose queue promotion is on hold until the given instant,
    /// after a machine-side refusal. In-memory on purpose: a restart retries
    /// once and re-arms the hold from the fresh refusal.
    promotion_holds: Mutex<HashMap<CodeSessionId, std::time::Instant>>,
    /// Wakes the sweep for an immediate pass. A wake with no waiter is kept
    /// until the sweep next listens, so none is lost between passes.
    sweep_wake: Notify,
}

impl RemoteSessions {
    pub(crate) fn new(
        provisioner: Arc<dyn SandboxProvisioner>,
        settings: RemoteSpawnSettings,
    ) -> Arc<Self> {
        Arc::new(Self {
            provisioner,
            settings,
            pumps: Mutex::new(HashMap::new()),
            promotion_holds: Mutex::new(HashMap::new()),
            sweep_wake: Notify::new(),
        })
    }

    /// Ask the sweep for a pass now instead of at its next floor.
    pub(crate) fn wake_sweep(&self) {
        self.sweep_wake.notify_one();
    }

    /// The driver view over this context for one call.
    pub(crate) fn driver<'a>(
        &'a self,
        db: &'a Arc<DbStore>,
        bus: &'a super::super::bus::CodeEventBus,
    ) -> RemoteDriver<'a> {
        RemoteDriver {
            db,
            bus,
            provisioner: self.provisioner.as_ref(),
            settings: &self.settings,
        }
    }

    /// Whether promotion for `session` is inside a refusal hold.
    pub(crate) fn promotion_held(&self, session: CodeSessionId) -> bool {
        let mut holds = self.promotion_holds.lock().expect("promotion holds");
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
    pub(crate) fn hold_promotion(&self, session: CodeSessionId) {
        self.promotion_holds
            .lock()
            .expect("promotion holds")
            .insert(session, std::time::Instant::now() + PROMOTION_RETRY_HOLD);
    }

    /// Clear a hold after a promotion that went through.
    pub(crate) fn clear_promotion_hold(&self, session: CodeSessionId) {
        self.promotion_holds
            .lock()
            .expect("promotion holds")
            .remove(&session);
    }

    /// Make sure `session` has a pump task, spawning one when it has none.
    fn ensure_pump(
        self: &Arc<Self>,
        runtime: &Arc<CodeRuntime>,
        owner: OwnerId,
        session: CodeSessionId,
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
                let wake = pump_session(runtime, Arc::clone(&remote), owner, session).await;
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
async fn pump_session(
    runtime: Weak<CodeRuntime>,
    remote: Arc<RemoteSessions>,
    owner: OwnerId,
    session_id: CodeSessionId,
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
            CodeSessionLifecycle::Fenced | CodeSessionLifecycle::Ended
        ) {
            return false;
        }
        let driver = remote.driver(&runtime.db, runtime.bus.as_ref());
        match driver.pump(&mut session, PUMP_HELD_WAIT_SECONDS).await {
            Ok(report) => {
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
                warn!(session = %session_id, %error, "a remote pump failed; backing off");
                // Drop the runtime handle across the sleep so shutdown is
                // not held open by a backoff.
                drop(runtime);
                tokio::time::sleep(PUMP_FAULT_BACKOFF).await;
            }
        }
    }
}

/// Holds the remote sweep alive; aborts it on drop.
pub(crate) struct RemoteSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl RemoteSweepGuard {
    pub(crate) fn spawn(runtime: Weak<CodeRuntime>) -> Self {
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
        Self(Some(handle))
    }
}

impl Drop for RemoteSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// What one sweep pass found, which sets the floor before the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteSweepActivity {
    /// Incarnations that still owe events and so hold a pump task.
    pub(crate) draining: usize,
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
pub(crate) async fn sweep_remote(runtime: &Arc<CodeRuntime>) -> RemoteSweepActivity {
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
        CodeRepo, CodeSessionLifecycle, CodeTurnStatus, CodeWorkspaceStatus, FenceReason,
        HarnessKind, OwnerId, PermissionMode, RepoId,
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
    struct FakeProvisioner {
        spawns: StdMutex<Vec<SpawnArguments>>,
        sends: StdMutex<Vec<String>>,
        event_reads: StdMutex<VecDeque<SandboxEvents>>,
        cancels: StdMutex<Vec<String>>,
    }

    #[async_trait]
    impl SandboxProvisioner for FakeProvisioner {
        async fn spawn(
            &self,
            _owner: &OwnerId,
            arguments: &SpawnArguments,
        ) -> Result<SandboxLease, RemoteSandboxError> {
            self.spawns.lock().unwrap().push(arguments.clone());
            Ok(SandboxLease {
                sandbox_id: "sb-1".to_owned(),
                state: SandboxState::Pending,
                latest_event_seq: 0,
                expires_in_seconds: 7200,
            })
        }

        async fn status(
            &self,
            _owner: &OwnerId,
            sandbox_id: &str,
        ) -> Result<SandboxStatus, RemoteSandboxError> {
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
            _sandbox_id: &str,
            _cursor: EventCursor,
        ) -> Result<SandboxEvents, RemoteSandboxError> {
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
            _sandbox_id: &str,
            message: &SandboxMessage,
        ) -> Result<MessageReceipt, RemoteSandboxError> {
            self.sends.lock().unwrap().push(message.body.clone());
            Ok(MessageReceipt {
                seq: 1,
                interrupt: false,
                pending_messages: 0,
            })
        }

        async fn cancel(
            &self,
            _owner: &OwnerId,
            sandbox_id: &str,
        ) -> Result<(), RemoteSandboxError> {
            self.cancels.lock().unwrap().push(sandbox_id.to_owned());
            Ok(())
        }
    }

    fn settings() -> RemoteSpawnSettings {
        RemoteSpawnSettings {
            profile: "tidebreak-remote".to_owned(),
            incarnation_cap: 2,
            spend_ceiling_microusd: None,
            session_spend_ceiling_microusd: None,
        }
    }

    #[test]
    fn configured_settings_use_operator_limits() {
        let mut config = tidebreak_core::Config::desktop("/data");
        config.runtime_concurrency_cap = 7;
        config.runtime_spawn_spend_ceiling_microusd = Some(9_000_000);
        config.runtime_session_spend_ceiling_microusd = None;

        let settings = configured_settings("remote-large".to_owned(), &config);
        assert_eq!(settings.profile, "remote-large");
        assert_eq!(settings.incarnation_cap, 7);
        assert_eq!(settings.spend_ceiling_microusd, Some(9_000_000));
        assert_eq!(settings.session_spend_ceiling_microusd, None);
    }

    async fn runtime_with_remote(
        root: &std::path::Path,
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
        .with_remote_sessions(RemoteSessions::new(fake.clone(), settings()));
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
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Idle);

        let outcome = runtime
            .submit_turn(&owner, session.id, "start".into(), None, None, Vec::new())
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
        assert_eq!(turn.status, CodeTurnStatus::Running);
        assert_eq!(
            fake.sends.lock().unwrap().as_slice(),
            &["and then".to_owned()]
        );
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
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(&owner, session.id, "start".into(), None, None, Vec::new())
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
            .submit_turn_from(&mut live, &workspace, &repo, &stale.message, Some(&stale))
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
            &tidebreak_core::CodeQueuedTurn {
                id: tidebreak_core::CodeTurnId::new(),
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
        let notices = |events: &[tidebreak_core::SequencedCodeEvent]| {
            events
                .iter()
                .filter(|row| matches!(row.event, tidebreak_core::CodeEvent::HarnessNotice { .. }))
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
            &tidebreak_core::CodeQueuedTurn {
                id: tidebreak_core::CodeTurnId::new(),
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
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(&owner, session.id, "start".into(), None, None, Vec::new())
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
        assert_eq!(reaped.lifecycle, CodeSessionLifecycle::Idle);
        assert!(reaped.fence_reason.is_none());
        assert_eq!(
            fake.cancels.lock().unwrap().as_slice(),
            &["sb-1".to_owned()]
        );
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
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(&owner, session.id, "start".into(), None, None, Vec::new())
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
                workspace.id,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        runtime
            .submit_turn(&owner, session.id, "start".into(), None, None, Vec::new())
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
        let resolved = runtime
            .external_get_or_create(
                &owner,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                Some("fix the flake".into()),
                HarnessKind::ClaudeCode,
                session_settings(),
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
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Idle);
        let workspace = runtime
            .get_workspace(&owner, session.workspace_id)
            .await
            .unwrap();
        assert!(workspace.is_remote());

        // The channel's retry answers with the same session.
        let again = runtime
            .external_get_or_create(
                &owner,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
            )
            .await
            .unwrap();
        let tidebreak_core::ExternalSessionResolution::Existing(hit) = again else {
            panic!("expected the existing binding");
        };
        assert_eq!(hit.session_id, binding.session_id);

        // Another grant's call on the same conversation refuses.
        let refused = runtime
            .external_get_or_create(
                &owner,
                tidebreak_core::CodeGrantId::new(),
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
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
        stored.lifecycle = CodeSessionLifecycle::Ended;
        assert!(tidebreak_core::db::code::save_session(&runtime.db, &stored)
            .await
            .unwrap());
        let ended = runtime
            .external_get_or_create(
                &owner,
                grant,
                "slack",
                "T1/C7/42.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
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

    /// External messages are idempotent on the event id: an idle session
    /// runs the message as a turn, a replay answers with that same turn,
    /// a busy session queues durably, and another grant cannot submit.
    #[tokio::test]
    async fn an_external_message_is_idempotent_and_queues_busy() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let grant = tidebreak_core::CodeGrantId::new();
        let resolved = runtime
            .external_get_or_create(
                &owner,
                grant,
                "slack",
                "T1/C9/77.1",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
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
                "start".into(),
                "Ev1",
                "1700000001.000100",
            )
            .await
            .unwrap();
        let ExternalMessageOutcome::NewTurn(turn) = first else {
            panic!("an idle session must run the message, got {first:?}");
        };
        assert_eq!(fake.spawns.lock().unwrap().len(), 1);

        // The channel redelivers Ev1: same turn, no second spawn or row.
        let replay = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                "start".into(),
                "Ev1",
                "1700000001.000100",
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
                "and then".into(),
                "Ev2",
                "1700000002.000100",
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
                "and then".into(),
                "Ev2",
                "1700000002.000100",
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
                "hijack".into(),
                "Ev3",
                "1700000003.000100",
            )
            .await;
        assert!(foreign.is_err(), "a foreign grant must refuse");

        // An ended session refuses instead of queueing into the void.
        let mut stored = runtime.get_session(&owner, session_id).await.unwrap();
        stored.lifecycle = CodeSessionLifecycle::Ended;
        assert!(tidebreak_core::db::code::save_session(&runtime.db, &stored)
            .await
            .unwrap());
        let ended = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                "still there?".into(),
                "Ev4",
                "1700000004.000100",
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
        let resolved = runtime
            .external_get_or_create(
                &owner,
                grant,
                "slack",
                "T1/C4/5.5",
                repo.id,
                None,
                HarnessKind::ClaudeCode,
                session_settings(),
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
                "start".into(),
                "Ev0",
                "1700000000.000100",
            )
            .await
            .unwrap();
        let queued_b = runtime
            .external_submit_message(
                &owner,
                grant,
                session_id,
                "message B".into(),
                "EvB",
                "1700000002.000100",
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
                "message A".into(),
                "EvA",
                "1700000001.000100",
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
            .get_workspace(&owner, live.workspace_id)
            .await
            .unwrap();
        let stored_repo = runtime.get_repo(&owner, workspace.repo_id).await.unwrap();
        let mut promoting = runtime.get_session(&owner, session_id).await.unwrap();
        let outcome = driver
            .submit_turn_from(
                &mut promoting,
                &workspace,
                &stored_repo,
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
                "message B".into(),
                "EvB",
                "1700000002.000100",
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
