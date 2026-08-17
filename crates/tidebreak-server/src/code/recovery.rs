//! Boot recovery for sessions recorded as running.
//!
//! A recorded child pid is probed with signal 0 only. Dead → the open turn
//! closes as Interrupted (journaled) and the session is Idle. Alive → the
//! session is fenced until an explicit reap. A pid that was not recorded at
//! spawn is never signaled.

use std::io::ErrorKind;

use chrono::Utc;
use tidebreak_core::db::code::{
    append_event, get_open_turn, list_sessions_by_lifecycle, save_turn,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeEvent, CodeSession, CodeSessionLifecycle,
    CodeTurnStatus, DbStore, FenceReason,
};

use super::attention::{persist_session, replace_attention};
use super::bus::CodeEventBus;

/// What boot recovery did to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    Interrupted { session: String },
    Fenced { session: String },
}

/// Probe result for a pid that was recorded at spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidLiveness {
    Alive,
    Dead,
}

/// Scan every `Running` session and apply the conservative recovery rule.
pub(crate) async fn recover_running_sessions(
    store: &DbStore,
    bus: &CodeEventBus,
) -> Result<Vec<RecoveryAction>, tidebreak_core::AgentError> {
    recover_running_sessions_with(store, bus, probe_recorded_pid).await
}

pub(crate) async fn recover_running_sessions_with(
    store: &DbStore,
    bus: &CodeEventBus,
    probe: impl Fn(i64) -> PidLiveness,
) -> Result<Vec<RecoveryAction>, tidebreak_core::AgentError> {
    let running = list_sessions_by_lifecycle(store, CodeSessionLifecycle::Running).await?;
    let mut actions = Vec::new();
    for session in running {
        if let Some(action) = recover_one(store, bus, session, &probe).await? {
            actions.push(action);
        }
    }
    Ok(actions)
}

/// Fence a live session, the runtime counterpart of the boot-time fence.
///
/// A [`FenceReason::ResumeLost`] also drops the stored resume ref: the engine
/// has already refused it, so the reap this fence asks for must re-attach
/// with a fresh engine session rather than resume the same dead ref.
pub(crate) async fn fence_session(
    store: &DbStore,
    bus: &CodeEventBus,
    session: &mut CodeSession,
    reason: FenceReason,
) -> Result<(), tidebreak_core::AgentError> {
    if matches!(reason, FenceReason::ResumeLost { .. }) {
        session.harness_resume_ref = None;
    }
    session.lifecycle = CodeSessionLifecycle::Fenced;
    session.fence_reason = Some(reason.clone());
    session.child_pid = None;
    replace_attention(
        session,
        Attention::new(
            AttentionState::Fenced { reason },
            AttentionSource::Lifecycle,
        ),
        false,
    );
    persist_session(store, bus, session).await?;
    Ok(())
}

/// Resolve a fenced session after an explicit user reap. Never signals.
pub(crate) async fn reap_session(
    store: &DbStore,
    bus: &CodeEventBus,
    mut session: CodeSession,
) -> Result<CodeSession, tidebreak_core::AgentError> {
    session.lifecycle = CodeSessionLifecycle::Idle;
    session.fence_reason = None;
    session.child_pid = None;
    replace_attention(
        &mut session,
        Attention::working(AttentionSource::Lifecycle),
        false,
    );
    persist_session(store, bus, &session).await?;
    Ok(session)
}

async fn recover_one(
    store: &DbStore,
    bus: &CodeEventBus,
    mut session: CodeSession,
    probe: &impl Fn(i64) -> PidLiveness,
) -> Result<Option<RecoveryAction>, tidebreak_core::AgentError> {
    let Some(pid) = session.child_pid else {
        // No recorded pid: treat as dead. Never invent a pid to probe.
        interrupt_open_turn(store, &session).await?;
        session.lifecycle = CodeSessionLifecycle::Idle;
        session.child_pid = None;
        replace_attention(
            &mut session,
            Attention::needs_you(
                "session recovered after the engine process exited",
                AttentionSource::Lifecycle,
            ),
            false,
        );
        persist_session(store, bus, &session).await?;
        return Ok(Some(RecoveryAction::Interrupted {
            session: session.id.to_string(),
        }));
    };
    match probe(pid) {
        PidLiveness::Dead => {
            interrupt_open_turn(store, &session).await?;
            session.lifecycle = CodeSessionLifecycle::Idle;
            session.child_pid = None;
            replace_attention(
                &mut session,
                Attention::needs_you(
                    "session recovered after the engine process exited",
                    AttentionSource::Lifecycle,
                ),
                false,
            );
            persist_session(store, bus, &session).await?;
            Ok(Some(RecoveryAction::Interrupted {
                session: session.id.to_string(),
            }))
        }
        PidLiveness::Alive => {
            session.lifecycle = CodeSessionLifecycle::Fenced;
            session.fence_reason = Some(FenceReason::OrphanAlive);
            replace_attention(
                &mut session,
                Attention::new(
                    AttentionState::Fenced {
                        reason: FenceReason::OrphanAlive,
                    },
                    AttentionSource::Lifecycle,
                ),
                false,
            );
            persist_session(store, bus, &session).await?;
            Ok(Some(RecoveryAction::Fenced {
                session: session.id.to_string(),
            }))
        }
    }
}

async fn interrupt_open_turn(
    store: &DbStore,
    session: &CodeSession,
) -> Result<(), tidebreak_core::AgentError> {
    if let Some(mut turn) = get_open_turn(store, session.id).await? {
        turn.status = CodeTurnStatus::Interrupted;
        turn.ended_at = Some(Utc::now());
        save_turn(store, &turn).await?;
        match append_event(
            store,
            session.id,
            session.spawn_epoch,
            &CodeEvent::TurnInterrupted,
        )
        .await
        {
            Ok(_) => {}
            Err(tidebreak_core::db::code::CodeJournalError::StaleSpawnEpoch { .. }) => {}
            Err(tidebreak_core::db::code::CodeJournalError::SessionNotFound { .. }) => {}
            Err(tidebreak_core::db::code::CodeJournalError::Store(err)) => return Err(err),
        }
    }
    Ok(())
}

/// Signal-0 probe of a pid that was recorded at spawn.
///
/// `EPERM` counts as alive. This function is the only place recovery sends a
/// signal, and it sends only signal 0 (existence check).
pub(crate) fn probe_recorded_pid(pid: i64) -> PidLiveness {
    if pid <= 0 {
        return PidLiveness::Dead;
    }
    #[cfg(unix)]
    {
        let raw = pid as libc::pid_t;
        // SAFETY: signal 0 never delivers; it only checks whether the pid exists
        // and whether this process may signal it. The pid was recorded from a
        // child this session spawned.
        let result = unsafe { libc::kill(raw, 0) };
        if result == 0 {
            return PidLiveness::Alive;
        }
        match std::io::Error::last_os_error().kind() {
            ErrorKind::PermissionDenied => PidLiveness::Alive,
            ErrorKind::NotFound => PidLiveness::Dead,
            _ => {
                let errno = std::io::Error::last_os_error().raw_os_error();
                if errno == Some(libc::EPERM) {
                    PidLiveness::Alive
                } else if errno == Some(libc::ESRCH) {
                    PidLiveness::Dead
                } else {
                    // Ambiguity is treated as alive so we fence rather than kill.
                    PidLiveness::Alive
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        PidLiveness::Alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tidebreak_core::db::code::{
        get_session, get_turn, insert_repo, insert_session, insert_turn, insert_workspace,
    };
    use tidebreak_core::{
        Attention, CodePermissionMode, CodeRepo, CodeSessionId, CodeTurn, CodeTurnId,
        CodeWorkspace, CodeWorkspaceStatus, HarnessKind, RepoId, WorkspaceId,
    };
    use tidebreak_harness::HarnessAdapter;

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    async fn seeded_running(
        pid: Option<i64>,
    ) -> (tempfile::TempDir, DbStore, CodeSessionId, CodeTurnId) {
        let (directory, store, session_id) =
            seeded_session(pid, CodeSessionLifecycle::Running).await;
        let turn_id = CodeTurnId::new();
        insert_turn(
            &store,
            &CodeTurn {
                id: turn_id,
                session_id,
                ordinal: 1,
                status: CodeTurnStatus::Running,
                user_input: "hello".into(),
                user_input_blob_id: None,
                checkpoint_ref: None,
                diffstat: None,
                usage: None,
                narrative: None,
                started_at: now(),
                ended_at: None,
            },
        )
        .await
        .unwrap();
        (directory, store, session_id, turn_id)
    }

    async fn seeded_session(
        pid: Option<i64>,
        lifecycle: CodeSessionLifecycle,
    ) -> (tempfile::TempDir, DbStore, CodeSessionId) {
        let directory = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("t.db").display()
        ))
        .await
        .unwrap();
        let repo_id = RepoId::new();
        insert_repo(
            &store,
            &CodeRepo {
                id: repo_id,
                root_path: directory.path().join("repo").display().to_string(),
                display_name: "example".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: now(),
            },
        )
        .await
        .unwrap();
        let workspace_id = WorkspaceId::new();
        insert_workspace(
            &store,
            &CodeWorkspace {
                id: workspace_id,
                repo_id,
                title: "first".into(),
                worktree_path: directory.path().join("wt").display().to_string(),
                branch_name: "tidebreak/first".into(),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: now(),
                archived_at: None,
            },
        )
        .await
        .unwrap();
        let session_id = CodeSessionId::new();
        insert_session(
            &store,
            &CodeSession {
                id: session_id,
                workspace_id,
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: Some("2.1.233".into()),
                harness_resume_ref: None,
                permission_mode: CodePermissionMode::Plan,
                lifecycle,
                fence_reason: None,
                child_pid: pid,
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                created_at: now(),
            },
        )
        .await
        .unwrap();
        (directory, store, session_id)
    }

    #[tokio::test]
    async fn dead_pid_closes_the_open_turn_as_interrupted() {
        let (_dir, store, session_id, turn_id) = seeded_running(Some(9_999_991)).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_| PidLiveness::Dead)
            .await
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Interrupted { .. }]
        ));
        let session = get_session(&store, session_id).await.unwrap().unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Idle);
        assert!(matches!(
            session.attention.state,
            AttentionState::NeedsYou {
                source: AttentionSource::Lifecycle,
                ..
            }
        ));
        let turn = get_turn(&store, turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Interrupted);
    }

    #[tokio::test]
    async fn live_recorded_pid_fences_and_does_not_signal_a_decoy() {
        let signaled = Arc::new(AtomicBool::new(false));
        let mut decoy = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn decoy");
        let decoy_pid = i64::from(decoy.id());
        let (_dir, store, session_id, turn_id) = seeded_running(Some(decoy_pid)).await;
        let flag = signaled.clone();
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, move |pid| {
            // Recovery must probe only the recorded pid, and only via the
            // injected probe — never by signaling the decoy itself.
            assert_eq!(pid, decoy_pid);
            flag.store(true, Ordering::SeqCst);
            PidLiveness::Alive
        })
        .await
        .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Fenced { .. }]
        ));
        let session = get_session(&store, session_id).await.unwrap().unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Fenced);
        assert_eq!(session.fence_reason, Some(FenceReason::OrphanAlive));
        assert!(matches!(
            session.attention.state,
            AttentionState::Fenced {
                reason: FenceReason::OrphanAlive
            }
        ));
        let turn = get_turn(&store, turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Running);
        assert!(
            decoy.try_wait().ok().flatten().is_none(),
            "decoy must still be alive — recovery must not signal it"
        );
        let _ = signaled;
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[tokio::test]
    async fn a_pid_the_worker_recorded_mid_turn_fences_recovery() {
        // The fence is only as good as the pid in the row. Adapters that spawn
        // one child per turn have no pid to report at turn boundaries, so this
        // drives a real worker turn and reads back what the worker itself
        // wrote — seeding a pid here would test nothing.
        let (_dir, store, session_id) = seeded_session(None, CodeSessionLifecycle::Idle).await;
        let store = Arc::new(store);
        let bus = Arc::new(crate::code::bus::CodeEventBus::default());
        let session = get_session(&store, session_id).await.unwrap().unwrap();
        let sink = crate::code::session_worker::sink_for(
            store.clone(),
            bus.clone(),
            session_id,
            session.spawn_epoch,
            None,
        );
        let child_pid = i64::from(std::process::id());
        let engine = crate::scripted_harness::ScriptedAdapter::new(vec![
            tidebreak_harness::HarnessEvent::TurnStarted,
            tidebreak_harness::HarnessEvent::AssistantDelta {
                text: "working".into(),
            },
        ])
        .with_child_pid(child_pid)
        // Long enough that the turn is unambiguously still in flight while
        // recovery runs; the test never waits for it.
        .with_delay(std::time::Duration::from_secs(30))
        .launch(tidebreak_harness::SessionSpec {
            worktree: _dir.path().join("wt"),
            permission_mode: CodePermissionMode::Plan,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: std::path::PathBuf::from("/scripted/engine"),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
        })
        .await
        .unwrap();
        let handle = crate::code::session_worker::spawn_session_worker(
            store.clone(),
            bus.clone(),
            session,
            engine,
            sink,
        );
        let (reply, _turn) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::RunTurn {
                message: "hello".into(),
                reply,
            })
            .await
            .unwrap();

        let recorded = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = get_session(&store, session_id).await.unwrap().unwrap();
                if let Some(pid) = current.child_pid {
                    return pid;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the worker must record the pid while the turn is in flight");
        assert_eq!(recorded, child_pid);

        let probed = Arc::new(AtomicBool::new(false));
        let flag = probed.clone();
        let actions = recover_running_sessions_with(&store, &bus, move |pid| {
            assert_eq!(pid, child_pid);
            flag.store(true, Ordering::SeqCst);
            PidLiveness::Alive
        })
        .await
        .unwrap();
        assert!(
            probed.load(Ordering::SeqCst),
            "recovery must probe the recorded pid rather than assume the engine died"
        );
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Fenced { .. }]
        ));
    }

    #[tokio::test]
    async fn fenced_recovery_does_not_overwrite_a_manual_pin() {
        let (_dir, store, session_id, _) = seeded_running(Some(1)).await;
        let mut session = get_session(&store, session_id).await.unwrap().unwrap();
        session.attention = Attention::manual("hold");
        tidebreak_core::db::code::save_session(&store, &session)
            .await
            .unwrap();
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_| PidLiveness::Alive)
            .await
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Fenced { .. }]
        ));
        let session = get_session(&store, session_id).await.unwrap().unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Fenced);
        assert!(
            matches!(session.attention.state, AttentionState::Manual { .. }),
            "Fenced recovery must go through should_replace: {:?}",
            session.attention
        );
    }

    #[tokio::test]
    async fn eperm_probe_counts_as_alive() {
        let (_dir, store, session_id, _) = seeded_running(Some(1)).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_| PidLiveness::Alive)
            .await
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Fenced { .. }]
        ));
        let session = get_session(&store, session_id).await.unwrap().unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Fenced);
    }

    #[test]
    fn probe_never_kills_and_treats_eperm_as_alive() {
        // Our own pid is alive; signal 0 must not terminate us.
        let self_pid = i64::from(std::process::id());
        assert_eq!(probe_recorded_pid(self_pid), PidLiveness::Alive);
        assert_eq!(probe_recorded_pid(-1), PidLiveness::Dead);
        assert_eq!(probe_recorded_pid(0), PidLiveness::Dead);
    }
}
