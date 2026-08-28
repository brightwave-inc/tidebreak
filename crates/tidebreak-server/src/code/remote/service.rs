//! Wire remote sessions into the running server: the configured transport,
//! the per-session pump tasks, and the periodic sweep that reconciles both.
//!
//! The sweep is the only scheduler. Every tick it closes stale intents,
//! makes sure each incarnation that still owes events has a pump task, and
//! promotes the queue head of any idle remote session. Pump tasks hold the
//! long event wait; everything else is a cheap store read, so a crashed or
//! finished task is simply respawned on the next tick from durable state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tracing::warn;

use tidebreak_core::db::code::{get_session, pumpable_incarnations_all_owners};
use tidebreak_core::{CodeSessionId, CodeSessionLifecycle, DbStore, OwnerId};

use super::super::runtime::CodeRuntime;
use super::driver::{sweep_stale_intents, RemoteDriver, RemoteSpawnSettings};
use super::SandboxProvisioner;

/// How often the sweep reconciles pump tasks and promotes queue heads.
const REMOTE_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The held wait a pump task asks the events read to hold.
const PUMP_HELD_WAIT_SECONDS: u16 = 20;

/// How long a pump task sleeps after a transport fault before retrying.
const PUMP_FAULT_BACKOFF: std::time::Duration = std::time::Duration::from_secs(5);

/// Spawn settings a deployment starts with until they are operator-tunable:
/// three concurrent sandboxes per owner, $5 per spawn, $20 per session. The
/// per-spawn ceiling bounds one runaway incarnation; the per-session ledger
/// bounds their sum, since reincarnation multiplies the former.
pub(crate) fn default_settings(profile: String) -> RemoteSpawnSettings {
    RemoteSpawnSettings {
        profile,
        incarnation_cap: 3,
        spend_ceiling_microusd: Some(5_000_000),
        session_spend_ceiling_microusd: Some(20_000_000),
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
    /// spawns missing ones; nothing else writes here.
    pumps: Mutex<HashMap<CodeSessionId, tokio::task::JoinHandle<()>>>,
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
        })
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
                pump_session(runtime, remote, owner, session).await;
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
async fn pump_session(
    runtime: Weak<CodeRuntime>,
    remote: Arc<RemoteSessions>,
    owner: OwnerId,
    session_id: CodeSessionId,
) {
    loop {
        let Some(runtime) = runtime.upgrade() else {
            return;
        };
        let Ok(Some(mut session)) = get_session(runtime.db(), &owner, session_id).await else {
            return;
        };
        if matches!(
            session.lifecycle,
            CodeSessionLifecycle::Fenced | CodeSessionLifecycle::Ended
        ) {
            return;
        }
        let driver = remote.driver(runtime.db(), runtime.bus());
        match driver.pump(&mut session, PUMP_HELD_WAIT_SECONDS).await {
            Ok(report) => {
                if report.sign_in_required {
                    // Nothing drains until the owner signs in; the sweep
                    // brings the task back to try again.
                    return;
                }
                if report.incarnation_stopped || report.fenced.is_some() {
                    return;
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
            let mut ticker = tokio::time::interval(REMOTE_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Some(runtime) = runtime.upgrade() else {
                    return;
                };
                sweep_remote(&runtime).await;
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

/// One sweep tick: expire stale intents, reconcile pump tasks against the
/// incarnations that owe events, and promote idle remote queue heads.
pub(crate) async fn sweep_remote(runtime: &Arc<CodeRuntime>) {
    let Some(remote) = runtime.remote_sessions() else {
        return;
    };
    if let Err(error) = sweep_stale_intents(runtime.db(), runtime.bus(), chrono::Utc::now()).await {
        warn!(%error, "the stale-intent sweep failed");
    }
    match pumpable_incarnations_all_owners(runtime.db()).await {
        Ok(rows) => {
            for row in rows {
                if row.sandbox_id.is_some() {
                    remote.ensure_pump(runtime, row.owner.clone(), row.session_id);
                }
            }
        }
        Err(error) => warn!(%error, "could not list pumpable incarnations"),
    }
    if let Err(error) = runtime.promote_remote_queue_heads().await {
        warn!(error = ?error, "remote queue promotion failed");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;

    use tidebreak_core::db::code::{insert_repo, latest_turn};
    use tidebreak_core::{
        CodeRepo, CodeSessionLifecycle, CodeTurnStatus, FenceReason, HarnessKind, OwnerId,
        PermissionMode, RepoId,
    };

    use super::super::super::runtime::{NewSessionSettings, SubmitTurnOutcome};
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
        insert_repo(runtime.db(), &repo).await.unwrap();
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

    /// The runtime carries a turn to a sandbox and back with no local
    /// harness: create, submit (spawn), pump (settle), promote the queued
    /// follow-up at idle.
    #[tokio::test]
    async fn a_remote_session_carries_a_turn_without_a_local_harness() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, fake, owner, repo) = runtime_with_remote(dir.path()).await;
        let remote = runtime.remote_sessions().unwrap();

        let workspace = runtime
            .create_remote_workspace(&owner, repo.id, Some("remote".into()))
            .await
            .unwrap();
        assert!(workspace.is_remote());
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
            assert_eq!(spawns.len(), 1);
            assert_eq!(spawns[0].repository_ref.as_deref(), Some("main"));
            assert_eq!(
                spawns[0].repository.as_deref(),
                Some("https://github.com/acme/tools")
            );
        }

        // A send while the turn runs parks as a durable queue row.
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

        // The pump settles turn 1; promotion then delivers the head to the
        // still-live sandbox as an inbox message.
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
        remote
            .driver(runtime.db(), runtime.bus())
            .pump(&mut live, 0)
            .await
            .unwrap();
        runtime.promote_remote_queue_heads().await.unwrap();

        let (queued_rows, _) = runtime.list_queued_turns(&owner, session.id).await.unwrap();
        assert!(queued_rows.is_empty());
        let turn = latest_turn(runtime.db(), &owner, session.id)
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
            runtime.db(),
            runtime.bus(),
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

    /// A repository with no recorded origin cannot back a remote workspace:
    /// the sandbox would have nothing to clone.
    #[tokio::test]
    async fn a_remote_workspace_requires_a_recorded_origin() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, _fake, owner, _repo) = runtime_with_remote(dir.path()).await;
        let mut local_only = CodeRepo {
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
        local_only.origin_host = None;
        insert_repo(runtime.db(), &local_only).await.unwrap();
        let refused = runtime
            .create_remote_workspace(&owner, local_only.id, None)
            .await;
        assert!(refused.is_err());
    }
}
