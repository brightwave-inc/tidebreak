//! Boot recovery for sessions recorded as running.
//!
//! A recorded child pid is checked with the platform's non-mutating existence
//! probe. Dead → the open turn closes as Interrupted (journaled) and the
//! session is Idle. Alive → the session is fenced until an explicit reap. A
//! pid that was not recorded at spawn is never probed or signaled.

#[cfg(unix)]
use std::io::ErrorKind;

use chrono::Utc;
use tidebreak_core::db::code::{
    append_event, get_open_turn, list_sessions_by_lifecycle_all_owners, save_session, save_turn,
    set_session_subagents,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeEvent, CodeSession, CodeSessionLifecycle,
    CodeSubagentStatus, CodeTurnStatus, DbStore, FenceReason,
};

use super::attention::{emit_digest, persist_session, replace_attention};
use super::bus::CodeEventBus;
use super::session_worker::settle_running_subagents;

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
    let running =
        list_sessions_by_lifecycle_all_owners(store, CodeSessionLifecycle::Running).await?;
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
    settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
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
    persist_recovery_session(store, bus, session).await?;
    Ok(())
}

/// Recovery owns both the lifecycle transition and the matching subagent
/// settlement. A normal full-row session save deliberately leaves subagents
/// untouched so stale worker copies cannot clobber the event sink's targeted
/// writes, so this path performs the two guarded writes explicitly and only
/// publishes the digest after both have landed.
async fn persist_recovery_session(
    store: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
) -> Result<bool, tidebreak_core::AgentError> {
    if !save_session(store, session).await? {
        return Ok(false);
    }
    if !set_session_subagents(store, &session.owner, session.id, &session.subagents).await? {
        return Err(tidebreak_core::AgentError::Store(format!(
            "code session {} disappeared while recovery settled subagents",
            session.id
        )));
    }
    emit_digest(store, bus, session).await;
    Ok(true)
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
    // A reap resolves the fence and leaves the session idle. It does not start
    // an engine, so the attention that follows is the resting state, not
    // `Working` — which is what this said while `Idle` did not exist.
    replace_attention(
        &mut session,
        Attention::new(AttentionState::Idle, AttentionSource::Lifecycle),
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
        settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
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
        persist_recovery_session(store, bus, &session).await?;
        return Ok(Some(RecoveryAction::Interrupted {
            session: session.id.to_string(),
        }));
    };
    match probe(pid) {
        PidLiveness::Dead => {
            interrupt_open_turn(store, &session).await?;
            settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
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
            persist_recovery_session(store, bus, &session).await?;
            Ok(Some(RecoveryAction::Interrupted {
                session: session.id.to_string(),
            }))
        }
        PidLiveness::Alive => {
            settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
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
            persist_recovery_session(store, bus, &session).await?;
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
    if let Some(mut turn) = get_open_turn(store, &session.owner, session.id).await? {
        turn.status = CodeTurnStatus::Interrupted;
        turn.ended_at = Some(Utc::now());
        save_turn(store, &session.owner, &turn).await?;
        match append_event(
            store,
            &session.owner,
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

/// Existence probe of a pid that was recorded at spawn.
///
/// Unix uses signal 0 and counts `EPERM` as alive. Windows opens the process
/// for limited query access and reads its exit code; access denial and other
/// ambiguous failures count as alive so recovery fences rather than risks
/// reusing an orphaned session.
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
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
        };
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        let Ok(pid) = u32::try_from(pid) else {
            return PidLiveness::Dead;
        };
        // SAFETY: `pid` is a recorded numeric process id. The returned handle
        // is checked for null and closed on every successful open.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return match std::io::Error::last_os_error().raw_os_error() {
                Some(code) if code as u32 == ERROR_ACCESS_DENIED => PidLiveness::Alive,
                Some(code) if code as u32 == ERROR_INVALID_PARAMETER => PidLiveness::Dead,
                _ => PidLiveness::Alive,
            };
        }
        let mut exit_code = 0_u32;
        // SAFETY: `process` is a live handle returned by `OpenProcess`, and
        // `exit_code` points to writable storage for the duration of the call.
        let queried = unsafe { GetExitCodeProcess(process, &mut exit_code) };
        // SAFETY: ownership of the handle is local to this function and this
        // is its single close, after the final query.
        let _ = unsafe { CloseHandle(process) };
        if queried == 0 {
            PidLiveness::Alive
        } else if exit_code == STILL_ACTIVE as u32 {
            PidLiveness::Alive
        } else {
            PidLiveness::Dead
        }
    }
    #[cfg(not(any(unix, windows)))]
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
        set_session_subagents,
    };
    use tidebreak_core::{
        Attention, CodeRepo, CodeSessionId, CodeSessionKind, CodeSubagentSummary, CodeTurn,
        CodeTurnId, CodeWorkspace, CodeWorkspaceStatus, HarnessKind, PermissionMode, RepoId,
        WorkspaceId,
    };
    use tidebreak_harness::HarnessAdapter;

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    async fn seed_running_subagent(store: &DbStore, session_id: CodeSessionId) {
        set_session_subagents(
            store,
            &tidebreak_core::OwnerId::local(),
            session_id,
            &[CodeSubagentSummary {
                call_id: "task-running".into(),
                name: "Still working".into(),
                status: CodeSubagentStatus::Running,
            }],
        )
        .await
        .unwrap();
    }

    async fn assert_subagent_failed(store: &DbStore, session_id: CodeSessionId) {
        let session = get_session(store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.subagents[0].status, CodeSubagentStatus::Failed);
    }

    async fn seeded_running(
        pid: Option<i64>,
    ) -> (tempfile::TempDir, DbStore, CodeSessionId, CodeTurnId) {
        let (directory, store, session_id) =
            seeded_session(pid, CodeSessionLifecycle::Running).await;
        let turn_id = CodeTurnId::new();
        insert_turn(
            &store,
            &tidebreak_core::OwnerId::local(),
            &CodeTurn {
                id: turn_id,
                session_id,
                ordinal: 1,
                status: CodeTurnStatus::Running,
                user_input: "hello".into(),
                user_input_blob_id: None,
                attachments: Vec::new(),
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
                owner: tidebreak_core::OwnerId::local(),
                root_path: directory.path().join("repo").display().to_string(),
                display_name: "example".into(),
                default_base_ref: "main".into(),
                branch_prefix: "tidebreak/".into(),
                setup_script: None,
                archive_script: None,
                quick_actions: Vec::new(),
                created_at: now(),
                removed_at: None,
                cloned_from: None,
            },
        )
        .await
        .unwrap();
        let workspace_id = WorkspaceId::new();
        insert_workspace(
            &store,
            &CodeWorkspace {
                id: workspace_id,
                owner: tidebreak_core::OwnerId::local(),
                repo_id,
                title: "first".into(),
                worktree_path: directory.path().join("wt").display().to_string(),
                branch_name: "tidebreak/first".into(),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
        )
        .await
        .unwrap();
        let session_id = CodeSessionId::new();
        insert_session(
            &store,
            &CodeSession {
                id: session_id,
                owner: tidebreak_core::OwnerId::local(),
                workspace_id,
                kind: CodeSessionKind::Interactive,
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: Some("2.1.233".into()),
                harness_resume_ref: None,
                permission_mode: PermissionMode::Plan,
                model: None,
                reasoning_effort: None,
                lifecycle,
                fence_reason: None,
                child_pid: pid,
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                subagents: Vec::new(),
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
        seed_running_subagent(&store, session_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_| PidLiveness::Dead)
            .await
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Interrupted { .. }]
        ));
        let session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Idle);
        assert!(matches!(
            session.attention.state,
            AttentionState::NeedsYou {
                source: AttentionSource::Lifecycle,
                ..
            }
        ));
        let turn = get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Interrupted);
        assert_subagent_failed(&store, session_id).await;
    }

    #[tokio::test]
    async fn missing_pid_settles_running_subagents_as_failed() {
        let (_dir, store, session_id, turn_id) = seeded_running(None).await;
        seed_running_subagent(&store, session_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_| {
            panic!("a missing pid must not be probed")
        })
        .await
        .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Interrupted { .. }]
        ));
        assert_eq!(
            get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            CodeTurnStatus::Interrupted
        );
        assert_subagent_failed(&store, session_id).await;
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
        seed_running_subagent(&store, session_id).await;
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
        let session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Fenced);
        assert_eq!(session.fence_reason, Some(FenceReason::OrphanAlive));
        assert!(matches!(
            session.attention.state,
            AttentionState::Fenced {
                reason: FenceReason::OrphanAlive
            }
        ));
        let turn = get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, CodeTurnStatus::Running);
        assert_subagent_failed(&store, session_id).await;
        assert!(
            decoy.try_wait().ok().flatten().is_none(),
            "decoy must still be alive — recovery must not signal it"
        );
        let _ = signaled;
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[tokio::test]
    async fn runtime_fence_settles_running_subagents_as_failed() {
        let (_dir, store, session_id) = seeded_session(None, CodeSessionLifecycle::Running).await;
        seed_running_subagent(&store, session_id).await;
        let mut session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        let bus = crate::code::bus::CodeEventBus::default();
        fence_session(
            &store,
            &bus,
            &mut session,
            FenceReason::ResumeLost {
                detail: "resume token rejected".into(),
            },
        )
        .await
        .unwrap();
        assert_subagent_failed(&store, session_id).await;
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
        let session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        let sink = crate::code::session_worker::sink_for(
            store.clone(),
            bus.clone(),
            session.owner.clone(),
            session_id,
            session.spawn_epoch,
            None,
            session.subagents.clone(),
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
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            env: Vec::new(),
            approval: None,
            binary: std::path::PathBuf::from("/scripted/engine"),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
        })
        .await
        .unwrap();
        let handle = crate::code::session_worker::spawn_session_worker(
            session,
            engine,
            sink,
            None,
            std::sync::Arc::new(tokio::sync::Mutex::new(())),
        );
        let (reply, _turn) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::RunTurn {
                message: "hello".into(),
                model: None,
                reasoning_effort: None,
                attachments: Vec::new(),
                reply,
            })
            .await
            .unwrap();

        let recorded = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
                    .await
                    .unwrap()
                    .unwrap();
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
        let mut session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
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
        let session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
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
        let session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.lifecycle, CodeSessionLifecycle::Fenced);
    }

    #[test]
    fn probe_recognizes_self_and_rejects_invalid_pids() {
        // Our own pid is alive; the existence probe must not terminate us.
        let self_pid = i64::from(std::process::id());
        assert_eq!(probe_recorded_pid(self_pid), PidLiveness::Alive);
        assert_eq!(probe_recorded_pid(-1), PidLiveness::Dead);
        assert_eq!(probe_recorded_pid(0), PidLiveness::Dead);
    }

    #[cfg(windows)]
    #[test]
    fn windows_probe_distinguishes_a_live_process_from_an_exited_one() {
        let mut child = std::process::Command::new("powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .spawn()
            .expect("spawn live process");
        let live_pid = i64::from(child.id());
        assert_eq!(probe_recorded_pid(live_pid), PidLiveness::Alive);
        child.kill().expect("terminate test process");
        child.wait().expect("reap test process");
        assert_eq!(probe_recorded_pid(live_pid), PidLiveness::Dead);
    }
}
