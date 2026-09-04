//! Boot recovery for open local engine sessions.
//!
//! A recorded child pid is checked with the platform's non-mutating existence
//! probe. Dead → the open turn closes as Interrupted (journaled) and the
//! session is Idle. Alive → the session is fenced until an explicit reap. A
//! pid that was not recorded at spawn is never probed or signaled. A session
//! already fenced for a live orphan is checked again on the next boot. If its
//! exact recorded process has exited, recovery closes its open turn and clears
//! the fence.

#[cfg(unix)]
use std::io::ErrorKind;

use chrono::Utc;
use tidebreak_core::db::code::{
    clear_session_harness_resume_ref, get_open_turn, list_sessions_by_lifecycle_all_owners,
    reap_fenced_session, recover_interrupted_session, replace_session_attention, save_session,
    set_session_subagents,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CapLevel, CodeSubagentStatus, DbStore, FenceReason,
    HarnessKind, Session, SessionLifecycle, Store,
};

use super::attention::{emit_digest, replace_attention};
use super::bus::CodeEventBus;
use super::session_worker::settle_running_subagents;

/// What boot recovery did to one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    Interrupted {
        session: String,
    },
    /// You kept a waiting turn open so the next worker can resume it.
    ReattachedPark {
        session: String,
    },
    Fenced {
        session: String,
    },
}

/// Return the `durable_parks` flag this engine declares.
///
/// The internal engine keeps a checkpoint across a process restart.
/// External harnesses do not.
pub(crate) fn durable_parks_for(kind: HarnessKind) -> CapLevel {
    if kind.is_in_process() {
        CapLevel::Supported
    } else {
        CapLevel::Unsupported
    }
}

/// Probe result for a pid that was recorded at spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidLiveness {
    Alive,
    Dead,
}

/// Recover each open local engine session without touching a live process.
pub(crate) async fn recover_running_sessions(
    store: &DbStore,
    bus: &CodeEventBus,
) -> Result<Vec<RecoveryAction>, tidebreak_core::AgentError> {
    recover_running_sessions_with(store, bus, probe_recorded_process).await
}

pub(crate) async fn recover_running_sessions_with(
    store: &DbStore,
    bus: &CodeEventBus,
    probe: impl Fn(i64, Option<&str>) -> PidLiveness,
) -> Result<Vec<RecoveryAction>, tidebreak_core::AgentError> {
    recover_running_sessions_with_caps(store, bus, probe, |session| {
        durable_parks_for(session.harness_kind)
    })
    .await
}

pub(crate) async fn recover_running_sessions_with_caps(
    store: &DbStore,
    bus: &CodeEventBus,
    probe: impl Fn(i64, Option<&str>) -> PidLiveness,
    durable_parks: impl Fn(&Session) -> CapLevel,
) -> Result<Vec<RecoveryAction>, tidebreak_core::AgentError> {
    let running = list_sessions_by_lifecycle_all_owners(store, SessionLifecycle::Running).await?;
    let fenced = list_sessions_by_lifecycle_all_owners(store, SessionLifecycle::Fenced).await?;
    let mut actions = Vec::new();
    for session in running {
        // A remote session's engine lives in a sandbox, not a local child:
        // Running with no pid is its normal shape, and interrupting it here
        // would abandon a lease that is still spending. Its own lifecycle
        // (the pump, the stale-intent sweep, reap) settles it.
        if let Some(workspace) = super::session_workspace(store, &session).await? {
            if workspace.is_remote() {
                continue;
            }
        }
        let parks = durable_parks(&session);
        if let Some(action) = recover_one(store, bus, session, &probe, parks).await? {
            actions.push(action);
        }
    }
    for session in fenced {
        if !matches!(session.fence_reason, Some(FenceReason::OrphanAlive)) {
            continue;
        }
        if let Some(workspace) = super::session_workspace(store, &session).await? {
            if workspace.is_remote() {
                continue;
            }
        }
        let Some(pid) = session.child_pid else {
            continue;
        };
        if probe(pid, session.child_process_identity.as_deref()) == PidLiveness::Alive {
            continue;
        }
        let Some(recovered) = reap_fenced_session(
            store,
            &session.owner,
            session.id,
            session.spawn_epoch,
            session.child_pid,
            session.child_process_identity.as_deref(),
        )
        .await?
        else {
            continue;
        };
        for event in recovered.events {
            bus.publish(session.id, event);
        }
        emit_digest(store, bus, &recovered.session).await;
        actions.push(RecoveryAction::Interrupted {
            session: recovered.session.id.to_string(),
        });
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
    session: &mut Session,
    reason: FenceReason,
) -> Result<(), tidebreak_core::AgentError> {
    if matches!(reason, FenceReason::ResumeLost { .. }) {
        session.harness_resume_ref = None;
        if !clear_session_harness_resume_ref(store, &session.owner, session.id, session.spawn_epoch)
            .await?
        {
            return Err(tidebreak_core::AgentError::Store(format!(
                "code session {} disappeared while clearing its rejected resume ref",
                session.id
            )));
        }
    }
    session.lifecycle = SessionLifecycle::Fenced;
    settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
    session.fence_reason = Some(reason.clone());
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
    session: &Session,
) -> Result<bool, tidebreak_core::AgentError> {
    if !save_session(store, session).await? {
        return Ok(false);
    }
    let _ = replace_session_attention(store, &session.owner, session.id, &session.attention, false)
        .await?;
    if !set_session_subagents(store, &session.owner, session.id, &session.subagents).await? {
        return Err(tidebreak_core::AgentError::Store(format!(
            "code session {} disappeared while recovery settled subagents",
            session.id
        )));
    }
    let stored = tidebreak_core::db::code::get_session(store, &session.owner, session.id)
        .await?
        .ok_or_else(|| {
            tidebreak_core::AgentError::Store(format!(
                "code session {} disappeared while recovery persisted it",
                session.id
            ))
        })?;
    emit_digest(store, bus, &stored).await;
    Ok(true)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReapSessionError {
    #[error(transparent)]
    Store(#[from] tidebreak_core::AgentError),
    #[error("the fenced session has a pid without a recorded process identity")]
    MissingProcessIdentity,
    #[error("the recorded engine process did not exit before the reap timeout")]
    ProcessStillAlive,
    #[error("the recorded engine process could not be terminated: {0}")]
    ProcessTermination(String),
    #[error("the fenced session changed while its process was being reaped")]
    SessionChanged,
}

#[async_trait::async_trait]
trait ProcessReaper {
    async fn terminate(
        &self,
        pid: i64,
        identity: &str,
        timeout: std::time::Duration,
    ) -> std::io::Result<tidebreak_harness::RecordedProcessReap>;
}

struct SystemProcessReaper;

#[async_trait::async_trait]
impl ProcessReaper for SystemProcessReaper {
    async fn terminate(
        &self,
        pid: i64,
        identity: &str,
        timeout: std::time::Duration,
    ) -> std::io::Result<tidebreak_harness::RecordedProcessReap> {
        tidebreak_harness::terminate_recorded_process(pid, identity, timeout).await
    }
}

/// Resolve a fenced session only after its exact recorded process exits.
pub(crate) async fn reap_session(
    store: &DbStore,
    bus: &CodeEventBus,
    session: Session,
) -> Result<Session, ReapSessionError> {
    reap_session_with(
        store,
        bus,
        session,
        &SystemProcessReaper,
        std::time::Duration::from_secs(5),
    )
    .await
}

async fn reap_session_with(
    store: &DbStore,
    bus: &CodeEventBus,
    session: Session,
    reaper: &(impl ProcessReaper + Sync),
    timeout: std::time::Duration,
) -> Result<Session, ReapSessionError> {
    match (session.child_pid, session.child_process_identity.as_deref()) {
        (Some(pid), Some(identity)) => match reaper.terminate(pid, identity, timeout).await {
            Ok(tidebreak_harness::RecordedProcessReap::Exited) => {}
            Ok(tidebreak_harness::RecordedProcessReap::TimedOut) => {
                return Err(ReapSessionError::ProcessStillAlive);
            }
            Err(error) => {
                return Err(ReapSessionError::ProcessTermination(error.to_string()));
            }
        },
        (Some(pid), None) => match tidebreak_harness::current_process_identity(pid) {
            Ok(None) => {}
            Ok(Some(_)) => return Err(ReapSessionError::MissingProcessIdentity),
            Err(error) => {
                return Err(ReapSessionError::ProcessTermination(error.to_string()));
            }
        },
        (None, Some(_)) => {
            return Err(ReapSessionError::MissingProcessIdentity);
        }
        (None, None) => {
            if matches!(session.fence_reason, Some(FenceReason::OrphanAlive)) {
                return Err(ReapSessionError::MissingProcessIdentity);
            }
        }
    }

    let Some(recovered) = reap_fenced_session(
        store,
        &session.owner,
        session.id,
        session.spawn_epoch,
        session.child_pid,
        session.child_process_identity.as_deref(),
    )
    .await?
    else {
        return Err(ReapSessionError::SessionChanged);
    };
    for event in recovered.events {
        bus.publish(session.id, event);
    }
    emit_digest(store, bus, &recovered.session).await;
    Ok(recovered.session)
}

async fn recover_one(
    store: &DbStore,
    bus: &CodeEventBus,
    mut session: Session,
    probe: &impl Fn(i64, Option<&str>) -> PidLiveness,
    durable_parks: CapLevel,
) -> Result<Option<RecoveryAction>, tidebreak_core::AgentError> {
    let Some(pid) = session.child_pid else {
        // Internal-engine sessions have no child pid ever. Their adapter
        // declares `mid_turn_resume`, so boot recovery leaves the open turn
        // rather than closing it as Interrupted. Expire a live lease so the
        // next worker can reclaim it instead of treating the dead claim as
        // still running.
        if session.harness_kind == tidebreak_core::HarnessKind::Internal {
            expire_internal_turn_lease(store, &session).await?;
            return Ok(None);
        }
        // No recorded pid: treat as dead. Never invent a pid to probe.
        return dead_worker_action(store, bus, &session, durable_parks).await;
    };
    match probe(pid, session.child_process_identity.as_deref()) {
        PidLiveness::Dead => dead_worker_action(store, bus, &session, durable_parks).await,
        PidLiveness::Alive => {
            settle_running_subagents(&mut session.subagents, CodeSubagentStatus::Failed);
            session.lifecycle = SessionLifecycle::Fenced;
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

async fn expire_internal_turn_lease(
    store: &DbStore,
    session: &Session,
) -> Result<(), tidebreak_core::AgentError> {
    let Some(open) = get_open_turn(store, &session.owner, session.id).await? else {
        return Ok(());
    };
    let Some(run) = store.get_turn(tidebreak_core::TurnId(open.id.0)).await? else {
        return Ok(());
    };
    let Some(lease_token) = run.lease_token else {
        return Ok(());
    };
    let _ = store
        .expire_turn_lease(run.id, lease_token, Utc::now())
        .await?;
    Ok(())
}

/// Settle a session whose worker died with a turn still open. Remote sessions
/// use this when a sandbox ends beneath an unsettled turn.
pub(crate) async fn recover_dead_worker(
    store: &DbStore,
    bus: &CodeEventBus,
    session: &Session,
) -> Result<Option<Session>, tidebreak_core::AgentError> {
    recover_dead_worker_with(store, bus, session, durable_parks_for(session.harness_kind)).await
}

pub(crate) async fn recover_dead_worker_with(
    store: &DbStore,
    bus: &CodeEventBus,
    session: &Session,
    durable_parks: CapLevel,
) -> Result<Option<Session>, tidebreak_core::AgentError> {
    let Some(recovered) = recover_interrupted_session(
        store,
        &session.owner,
        session.id,
        session.spawn_epoch,
        durable_parks,
    )
    .await?
    else {
        return Ok(None);
    };
    for event in recovered.events {
        bus.publish(session.id, event);
    }
    emit_digest(store, bus, &recovered.session).await;
    Ok(Some(recovered.session))
}

async fn dead_worker_action(
    store: &DbStore,
    bus: &CodeEventBus,
    session: &Session,
    durable_parks: CapLevel,
) -> Result<Option<RecoveryAction>, tidebreak_core::AgentError> {
    let Some(recovered) = recover_dead_worker_with(store, bus, session, durable_parks).await?
    else {
        return Ok(None);
    };
    let action =
        match tidebreak_core::db::code::get_open_turn(store, &session.owner, session.id).await? {
            Some(turn) if turn.status == tidebreak_core::TurnStatus::Waiting => {
                RecoveryAction::ReattachedPark {
                    session: recovered.id.to_string(),
                }
            }
            _ => RecoveryAction::Interrupted {
                session: recovered.id.to_string(),
            },
        };
    Ok(Some(action))
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

/// Identity-aware liveness probe for a process recorded by a session worker.
///
/// A different creation identity proves the recorded process exited without
/// treating the replacement process as the orphan. Missing or unreadable
/// identities fail closed while the numeric pid remains live.
pub(crate) fn probe_recorded_process(pid: i64, expected_identity: Option<&str>) -> PidLiveness {
    let numeric = probe_recorded_pid(pid);
    if numeric == PidLiveness::Dead {
        return PidLiveness::Dead;
    }
    let Some(expected_identity) = expected_identity else {
        return PidLiveness::Alive;
    };
    match tidebreak_harness::current_process_identity(pid) {
        Ok(None) => PidLiveness::Dead,
        Ok(Some(observed)) if observed == expected_identity => PidLiveness::Alive,
        Ok(Some(_)) => PidLiveness::Dead,
        Err(_) => PidLiveness::Alive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tidebreak_core::db::code::{
        get_approval, get_session, get_turn, insert_approval, insert_repo, insert_session,
        insert_turn, insert_workspace, list_events, save_turn, set_session_subagents,
        MAX_REPLAY_EVENTS,
    };
    use tidebreak_core::{
        Approval, ApprovalDecisionKind, ApprovalId, ApprovalKind, ApprovalState, Attention,
        CodeRepo, CodeSubagentSummary, CodeWorkspace, CodeWorkspaceStatus, Event, HarnessKind,
        PermissionMode, RepoId, Session, SessionId, SessionKind, Turn, TurnId, TurnParkWait,
        TurnStatus, WorkspaceId,
    };
    use tidebreak_harness::HarnessAdapter;

    fn now() -> chrono::DateTime<Utc> {
        Utc::now()
    }

    async fn seed_running_subagent(store: &DbStore, session_id: SessionId) {
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

    async fn assert_subagent_failed(store: &DbStore, session_id: SessionId) {
        let session = get_session(store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.subagents[0].status, CodeSubagentStatus::Failed);
    }

    async fn seed_pending_approval(
        store: &DbStore,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> ApprovalId {
        let approval_id = ApprovalId::new();
        insert_approval(
            store,
            &tidebreak_core::OwnerId::local(),
            &Approval {
                id: approval_id,
                session_id,
                turn_id,
                kind: ApprovalKind::Other {
                    summary: "run command".into(),
                },
                harness_raw: serde_json::json!({"call_id":"toolu_recovery"}),
                native_call_id: Some("toolu_recovery".into()),
                server_capability: Some("cap_recovery".into()),
                request_sha256: Some("sha_recovery".into()),
                worker_epoch: Some(1),
                decision_claim: Some(uuid::Uuid::new_v4()),
                claimed_at: Some(now()),
                state: ApprovalState::Pending,
                feedback: None,
                requested_at: now(),
                decided_at: None,
                auto_judge_status: None,
            },
        )
        .await
        .unwrap();
        approval_id
    }

    #[derive(Clone, Copy)]
    enum StubReap {
        Exited,
        TimedOut,
        Refused,
    }

    struct StubReaper {
        expected_pid: i64,
        expected_identity: String,
        result: StubReap,
        calls: AtomicUsize,
    }

    impl StubReaper {
        fn new(expected_pid: i64, expected_identity: impl Into<String>, result: StubReap) -> Self {
            Self {
                expected_pid,
                expected_identity: expected_identity.into(),
                result,
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProcessReaper for StubReaper {
        async fn terminate(
            &self,
            pid: i64,
            identity: &str,
            _timeout: std::time::Duration,
        ) -> std::io::Result<tidebreak_harness::RecordedProcessReap> {
            assert_eq!(pid, self.expected_pid);
            assert_eq!(identity, self.expected_identity.as_str());
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.result {
                StubReap::Exited => Ok(tidebreak_harness::RecordedProcessReap::Exited),
                StubReap::TimedOut => Ok(tidebreak_harness::RecordedProcessReap::TimedOut),
                StubReap::Refused => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "termination refused",
                )),
            }
        }
    }

    async fn seeded_running(pid: Option<i64>) -> (tempfile::TempDir, DbStore, SessionId, TurnId) {
        let (directory, store, session_id) = seeded_session(pid, SessionLifecycle::Running).await;
        let turn_id = TurnId::new();
        insert_turn(
            &store,
            &tidebreak_core::OwnerId::local(),
            &Turn {
                id: turn_id,
                session_id,
                ordinal: 1,
                status: TurnStatus::Running,
                model: None,
                fast_mode: false,
                user_input: "hello".into(),
                user_input_blob_id: None,
                attachments: Vec::new(),
                checkpoint_ref: None,
                diffstat: None,
                usage: None,
                narrative: None,
                rewrite: None,
                started_at: now(),
                ended_at: None,
                park_ref: None,
                park_wait: None,
            },
        )
        .await
        .unwrap();
        (directory, store, session_id, turn_id)
    }

    async fn seeded_session(
        pid: Option<i64>,
        lifecycle: SessionLifecycle,
    ) -> (tempfile::TempDir, DbStore, SessionId) {
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
                origin_host: None,
                origin_owner: None,
                origin_name: None,
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
        let session_id = SessionId::new();
        insert_session(
            &store,
            &Session {
                id: session_id,
                owner: tidebreak_core::OwnerId::local(),
                workspace_id: Some(workspace_id),
                kind: SessionKind::Interactive,
                harness_kind: HarnessKind::ClaudeCode,
                harness_version: Some("2.1.233".into()),
                harness_resume_ref: None,
                permission_mode: PermissionMode::Plan,
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                lifecycle,
                fence_reason: None,
                child_pid: pid,
                child_process_identity: pid.map(|pid| format!("test:{pid}")),
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                subagents: Vec::new(),
                created_at: now(),
                execution_location: tidebreak_core::ExecutionLocation::Machine,
            },
        )
        .await
        .unwrap();
        (directory, store, session_id)
    }

    /// A remote session runs with no local pid on purpose: boot recovery
    /// must leave it alone rather than interrupt a turn whose sandbox is
    /// still working.
    #[tokio::test]
    async fn boot_recovery_leaves_remote_sessions_running() {
        let (_directory, store, session_id, _turn) = seeded_running(None).await;
        let owner = tidebreak_core::OwnerId::local();
        let session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        let mut workspace = tidebreak_core::db::code::get_workspace(
            &store,
            &owner,
            session.workspace_id.expect("workspace"),
        )
        .await
        .unwrap()
        .unwrap();
        workspace.worktree_path = String::new();
        tidebreak_core::db::code::save_workspace(&store, &workspace)
            .await
            .unwrap();

        let bus = CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Dead)
            .await
            .unwrap();
        assert!(actions.is_empty());
        let untouched = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(untouched.lifecycle, SessionLifecycle::Running);
    }

    async fn seeded_fenced_orphan(
        pid: i64,
    ) -> (
        tempfile::TempDir,
        DbStore,
        crate::code::bus::CodeEventBus,
        Session,
        TurnId,
        ApprovalId,
    ) {
        let (directory, store, session_id, turn_id) = seeded_running(Some(pid)).await;
        let approval_id = seed_pending_approval(&store, session_id, turn_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Alive)
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
        (directory, store, bus, session, turn_id, approval_id)
    }

    async fn assert_fenced_orphan_unchanged(
        store: &DbStore,
        pid: i64,
        identity: &str,
        epoch: i64,
        turn_id: TurnId,
        approval_id: ApprovalId,
    ) {
        let owner = tidebreak_core::OwnerId::local();
        let turn = get_turn(store, &owner, turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, TurnStatus::Running);
        let session = get_session(store, &owner, turn.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.lifecycle, SessionLifecycle::Fenced);
        assert_eq!(session.fence_reason, Some(FenceReason::OrphanAlive));
        assert_eq!(session.child_pid, Some(pid));
        assert_eq!(session.child_process_identity.as_deref(), Some(identity));
        assert_eq!(session.spawn_epoch, epoch);
        assert_eq!(
            get_approval(store, &owner, approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ApprovalState::Pending
        );
    }

    #[tokio::test]
    async fn dead_pid_closes_the_open_turn_as_interrupted() {
        let (_dir, store, session_id, turn_id) = seeded_running(Some(9_999_991)).await;
        seed_running_subagent(&store, session_id).await;
        let approval_id = seed_pending_approval(&store, session_id, turn_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Dead)
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
        assert_eq!(session.lifecycle, SessionLifecycle::Idle);
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
        assert_eq!(turn.status, TurnStatus::Interrupted);
        let approval = get_approval(&store, &tidebreak_core::OwnerId::local(), approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(approval.state, ApprovalState::Abandoned);
        assert!(approval.decision_claim.is_none());
        assert!(approval.decided_at.is_some());
        let events = list_events(
            &store,
            &tidebreak_core::OwnerId::local(),
            session_id,
            0,
            MAX_REPLAY_EVENTS,
        )
        .await
        .unwrap()
        .events;
        assert!(matches!(&events[0].event, Event::TurnInterrupted { .. }));
        assert!(matches!(
            &events[1].event,
            Event::ApprovalResolved {
                approval_id: resolved,
                decision: ApprovalDecisionKind::Abandoned,
            } if *resolved == approval_id
        ));
        assert_subagent_failed(&store, session_id).await;
    }

    #[tokio::test]
    async fn missing_pid_settles_running_subagents_as_failed() {
        let (_dir, store, session_id, turn_id) = seeded_running(None).await;
        seed_running_subagent(&store, session_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| {
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
            TurnStatus::Interrupted
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
        let decoy_identity = format!("test:{decoy_pid}");
        let (_dir, store, session_id, turn_id) = seeded_running(Some(decoy_pid)).await;
        seed_running_subagent(&store, session_id).await;
        let flag = signaled.clone();
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, move |pid, identity| {
            // Recovery must probe only the recorded pid, and only via the
            // injected probe — never by signaling the decoy itself.
            assert_eq!(pid, decoy_pid);
            assert_eq!(identity, Some(decoy_identity.as_str()));
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
        assert_eq!(session.lifecycle, SessionLifecycle::Fenced);
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
        assert_eq!(turn.status, TurnStatus::Running);
        assert_subagent_failed(&store, session_id).await;
        assert!(
            decoy.try_wait().ok().flatten().is_none(),
            "decoy must still be alive — recovery must not signal it"
        );
        assert!(signaled.load(Ordering::SeqCst));
        let _ = decoy.kill();
        let _ = decoy.wait();
    }

    #[tokio::test]
    async fn boot_recovery_reaps_a_previously_fenced_orphan_after_it_exits() {
        let pid = 4_241;
        let identity = format!("test:{pid}");
        let (_dir, store, bus, session, turn_id, approval_id) = seeded_fenced_orphan(pid).await;
        let session_id = session.id;

        let actions = recover_running_sessions_with(&store, &bus, |probed, observed_identity| {
            assert_eq!(probed, pid);
            assert_eq!(observed_identity, Some(identity.as_str()));
            PidLiveness::Dead
        })
        .await
        .unwrap();

        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Interrupted { session }] if session == &session_id.to_string()
        ));
        let recovered = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.lifecycle, SessionLifecycle::Idle);
        assert_eq!(recovered.child_pid, None);
        assert_eq!(recovered.child_process_identity, None);
        assert_eq!(recovered.fence_reason, None);
        assert_eq!(
            get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TurnStatus::Interrupted
        );
        assert_eq!(
            get_approval(&store, &tidebreak_core::OwnerId::local(), approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ApprovalState::Abandoned
        );
    }

    #[tokio::test]
    async fn successful_reap_settles_the_fence_before_advancing_the_epoch() {
        let pid = 4_242;
        let identity = format!("test:{pid}");
        let (_dir, store, bus, session, turn_id, approval_id) = seeded_fenced_orphan(pid).await;
        let old_epoch = session.spawn_epoch;
        let reaper = StubReaper::new(pid, &identity, StubReap::Exited);

        let reaped = reap_session_with(
            &store,
            &bus,
            session,
            &reaper,
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap();

        assert_eq!(reaper.calls.load(Ordering::SeqCst), 1);
        assert_eq!(reaped.lifecycle, SessionLifecycle::Idle);
        assert_eq!(reaped.spawn_epoch, old_epoch + 1);
        assert_eq!(reaped.child_pid, None);
        assert_eq!(reaped.child_process_identity, None);
        assert_eq!(reaped.fence_reason, None);
        assert_eq!(
            get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            TurnStatus::Interrupted
        );
        assert_eq!(
            get_approval(&store, &tidebreak_core::OwnerId::local(), approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ApprovalState::Abandoned
        );
    }

    #[tokio::test]
    async fn reap_timeout_preserves_the_live_orphan_fence() {
        let pid = 4_243;
        let identity = format!("test:{pid}");
        let (_dir, store, bus, session, turn_id, approval_id) = seeded_fenced_orphan(pid).await;
        let old_epoch = session.spawn_epoch;
        let reaper = StubReaper::new(pid, &identity, StubReap::TimedOut);

        let error = reap_session_with(
            &store,
            &bus,
            session,
            &reaper,
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ReapSessionError::ProcessStillAlive));
        assert_eq!(reaper.calls.load(Ordering::SeqCst), 1);
        assert_fenced_orphan_unchanged(&store, pid, &identity, old_epoch, turn_id, approval_id)
            .await;
    }

    #[tokio::test]
    async fn reap_refusal_preserves_the_live_orphan_fence() {
        let pid = 4_244;
        let identity = format!("test:{pid}");
        let (_dir, store, bus, session, turn_id, approval_id) = seeded_fenced_orphan(pid).await;
        let old_epoch = session.spawn_epoch;
        let reaper = StubReaper::new(pid, &identity, StubReap::Refused);

        let error = reap_session_with(
            &store,
            &bus,
            session,
            &reaper,
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ReapSessionError::ProcessTermination(_)));
        assert_eq!(reaper.calls.load(Ordering::SeqCst), 1);
        assert_fenced_orphan_unchanged(&store, pid, &identity, old_epoch, turn_id, approval_id)
            .await;
    }

    #[tokio::test]
    async fn reap_does_not_clear_a_fence_that_changed_during_process_settlement() {
        let pid = 4_245;
        let identity = format!("test:{pid}");
        let replacement_identity = format!("{identity}:replacement");
        let (_dir, store, bus, session, turn_id, approval_id) = seeded_fenced_orphan(pid).await;
        let old_epoch = session.spawn_epoch;
        let reaper = StubReaper::new(pid, &identity, StubReap::Exited);
        let mut changed = session.clone();
        changed.child_process_identity = Some(replacement_identity.clone());
        assert!(save_session(&store, &changed).await.unwrap());

        let error = reap_session_with(
            &store,
            &bus,
            session,
            &reaper,
            std::time::Duration::from_millis(1),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ReapSessionError::SessionChanged));
        assert_eq!(reaper.calls.load(Ordering::SeqCst), 1);
        assert_fenced_orphan_unchanged(
            &store,
            pid,
            &replacement_identity,
            old_epoch,
            turn_id,
            approval_id,
        )
        .await;
    }

    #[tokio::test]
    async fn runtime_fence_settles_running_subagents_as_failed() {
        let (_dir, store, session_id) = seeded_session(None, SessionLifecycle::Running).await;
        seed_running_subagent(&store, session_id).await;
        let mut session = get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
            .await
            .unwrap()
            .unwrap();
        session.harness_resume_ref = Some("rejected-ref".into());
        assert!(save_session(&store, &session).await.unwrap());
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
        assert_eq!(
            get_session(&store, &tidebreak_core::OwnerId::local(), session_id)
                .await
                .unwrap()
                .unwrap()
                .harness_resume_ref,
            None
        );
        assert_subagent_failed(&store, session_id).await;
    }

    #[tokio::test]
    async fn a_pid_the_worker_recorded_mid_turn_fences_recovery() {
        // The fence is only as good as the pid in the row. Adapters that spawn
        // one child per turn have no pid to report at turn boundaries, so this
        // drives a real worker turn and reads back what the worker itself
        // wrote — seeding a pid here would test nothing.
        let (_dir, store, session_id) = seeded_session(None, SessionLifecycle::Idle).await;
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
            session.harness_kind,
            false,
            None,
            session.subagents.clone(),
            None,
            None,
            None,
            None,
            crate::code::pr_refresh::HotPullRequests::default(),
        );
        let private_root = _dir.path().join("private");
        std::fs::create_dir(&private_root).unwrap();
        let private_root =
            crate::code::scratch::ScratchRoot::open_for_test(&private_root).expect("scratch root");
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
            owner: tidebreak_core::OwnerId::local(),
            session_id: tidebreak_core::SessionId::new(),
            worktree: _dir.path().join("wt"),
            allowed_read_roots: Vec::new(),
            permission_mode: PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            resume_ref: None,
            extra_argv: Vec::new(),
            extra_env: Vec::new(),
            relay_key_env: None,
            env: Vec::new(),
            approval: None,
            binary: Some(std::path::PathBuf::from("/scripted/engine")),
            sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
            browser: None,
        })
        .await
        .unwrap();
        let handle = crate::code::session_worker::spawn_session_worker(
            session,
            engine,
            sink,
            crate::code::session_worker::AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            std::sync::Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );
        let (reply, _turn) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::RunTurn {
                message: "hello".into(),
                attachments: Vec::new(),
                trigger_delivery: None,
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
        let actions = recover_running_sessions_with(&store, &bus, move |pid, identity| {
            assert_eq!(pid, child_pid);
            assert!(
                identity.is_some(),
                "the worker must record process identity"
            );
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
        let pinned = Attention::manual("hold");
        let changed = replace_session_attention(
            &store,
            &tidebreak_core::OwnerId::local(),
            session_id,
            &pinned,
            true,
        )
        .await
        .unwrap();
        assert_eq!(changed, Some(pinned));
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Alive)
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
        assert_eq!(session.lifecycle, SessionLifecycle::Fenced);
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
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Alive)
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
        assert_eq!(session.lifecycle, SessionLifecycle::Fenced);
    }

    #[tokio::test]
    async fn a_restart_does_not_interrupt_a_resumable_internal_turn() {
        let directory = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("t.db").display()
        ))
        .await
        .unwrap();
        let owner = tidebreak_core::OwnerId::local();
        let session_id = SessionId::new();
        insert_session(
            &store,
            &Session {
                id: session_id,
                owner: owner.clone(),
                workspace_id: None,
                kind: SessionKind::Interactive,
                harness_kind: HarnessKind::Internal,
                harness_version: Some("internal".into()),
                harness_resume_ref: None,
                permission_mode: PermissionMode::Ask,
                model: Some("test".into()),
                reasoning_effort: None,
                fast_mode: false,
                lifecycle: SessionLifecycle::Running,
                fence_reason: None,
                child_pid: None,
                child_process_identity: None,
                spawn_epoch: 1,
                attention: Attention::working(AttentionSource::Lifecycle),
                unrecognized_event_count: 0,
                subagents: Vec::new(),
                created_at: now(),
                execution_location: tidebreak_core::ExecutionLocation::Machine,
            },
        )
        .await
        .unwrap();
        let turn_id = TurnId::new();
        insert_turn(
            &store,
            &owner,
            &Turn {
                id: turn_id,
                session_id,
                ordinal: 1,
                status: TurnStatus::Running,
                model: Some("test".into()),
                fast_mode: false,
                user_input: "hello".into(),
                user_input_blob_id: None,
                attachments: Vec::new(),
                checkpoint_ref: None,
                diffstat: None,
                usage: None,
                narrative: None,
                rewrite: None,
                started_at: now(),
                ended_at: None,
                park_ref: None,
                park_wait: None,
            },
        )
        .await
        .unwrap();

        let bus = CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Dead)
            .await
            .unwrap();
        assert!(
            actions.is_empty(),
            "a resumable internal turn must not be interrupted on restart"
        );
        let session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.lifecycle, SessionLifecycle::Running);
        let turn = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, TurnStatus::Running);
    }

    #[test]
    fn probe_recognizes_self_and_rejects_invalid_pids() {
        // Our own pid is alive; the existence probe must not terminate us.
        let self_pid = i64::from(std::process::id());
        assert_eq!(probe_recorded_pid(self_pid), PidLiveness::Alive);
        assert_eq!(probe_recorded_pid(-1), PidLiveness::Dead);
        assert_eq!(probe_recorded_pid(0), PidLiveness::Dead);
    }

    async fn park_waiting_turn(store: &DbStore, turn_id: TurnId) {
        let owner = tidebreak_core::OwnerId::local();
        let mut turn = get_turn(store, &owner, turn_id).await.unwrap().unwrap();
        turn.status = TurnStatus::Waiting;
        turn.park_ref = Some("cp-1".into());
        turn.park_wait = Some(TurnParkWait::Approval {
            call_id: "call-1".into(),
        });
        assert!(save_turn(store, &owner, &turn).await.unwrap());
    }

    #[tokio::test]
    async fn recovery_interrupts_a_waiting_turn_without_durable_parks() {
        let (_dir, store, session_id, turn_id) = seeded_running(None).await;
        park_waiting_turn(&store, turn_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with(&store, &bus, |_, _| PidLiveness::Dead)
            .await
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::Interrupted { .. }]
        ));
        let turn = get_turn(&store, &tidebreak_core::OwnerId::local(), turn_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(turn.status, TurnStatus::Interrupted);
        let events = list_events(
            &store,
            &tidebreak_core::OwnerId::local(),
            session_id,
            0,
            MAX_REPLAY_EVENTS,
        )
        .await
        .unwrap()
        .events;
        assert!(matches!(&events[0].event, Event::TurnInterrupted { .. }));
    }

    #[tokio::test]
    async fn recovery_resumes_a_parked_turn_after_a_worker_restart() {
        let (directory, store, session_id, turn_id) = seeded_running(None).await;
        park_waiting_turn(&store, turn_id).await;
        let approval_id = seed_pending_approval(&store, session_id, turn_id).await;
        let bus = crate::code::bus::CodeEventBus::default();
        let actions = recover_running_sessions_with_caps(
            &store,
            &bus,
            |_, _| PidLiveness::Dead,
            |_| CapLevel::Supported,
        )
        .await
        .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [RecoveryAction::ReattachedPark { .. }]
        ));
        let owner = tidebreak_core::OwnerId::local();
        let turn = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
        assert_eq!(turn.status, TurnStatus::Waiting);
        assert_eq!(turn.park_ref.as_deref(), Some("cp-1"));
        assert_eq!(
            get_approval(&store, &owner, approval_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            ApprovalState::Pending
        );
        let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events;
        assert!(
            events
                .iter()
                .all(|event| !matches!(event.event, Event::TurnInterrupted { .. })),
            "a durable park must not journal an interrupt"
        );

        let store = Arc::new(store);
        let bus = Arc::new(bus);
        let session = get_session(&store, &owner, session_id)
            .await
            .unwrap()
            .unwrap();
        let sink = crate::code::session_worker::sink_for(
            store.clone(),
            bus.clone(),
            session.owner.clone(),
            session_id,
            session.spawn_epoch,
            session.harness_kind,
            false,
            None,
            session.subagents.clone(),
            None,
            None,
            None,
            None,
            crate::code::pr_refresh::HotPullRequests::default(),
        );
        let worktree = directory.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        let private = directory.path().join("private");
        std::fs::create_dir(&private).unwrap();
        let private_root =
            crate::code::scratch::ScratchRoot::open_for_test(&private).expect("scratch root");
        let adapter = crate::scripted_harness::ScriptedAdapter::new(vec![
            tidebreak_harness::HarnessEvent::TurnStarted,
            tidebreak_harness::HarnessEvent::ApprovalRequested {
                harness_ref: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                raw: serde_json::json!({ "tool_name": "Write" }),
                kind: None,
            },
            tidebreak_harness::HarnessEvent::AssistantMessage {
                text: "resumed after the restart".into(),
                parent_call_id: None,
            },
            tidebreak_harness::HarnessEvent::TurnCompleted {
                usage: tidebreak_core::TurnUsage::default(),
            },
        ])
        .with_unattended_approvals()
        .with_parked_turn(
            2,
            "cp-1",
            tidebreak_harness::ParkWait::Approval {
                call_id: "call-1".into(),
            },
        );
        let engine = adapter
            .launch(tidebreak_harness::SessionSpec {
                owner: tidebreak_core::OwnerId::local(),
                session_id: tidebreak_core::SessionId::new(),
                worktree,
                allowed_read_roots: Vec::new(),
                permission_mode: session.permission_mode,
                model: None,
                reasoning_effort: None,
                fast_mode: false,
                resume_ref: None,
                extra_argv: Vec::new(),
                extra_env: Vec::new(),
                relay_key_env: None,
                env: Vec::new(),
                approval: None,
                binary: Some(std::path::PathBuf::from("/scripted/engine")),
                sink: sink.clone() as Arc<dyn tidebreak_harness::HarnessEventSink>,
                browser: None,
            })
            .await
            .unwrap();
        let handle = crate::code::session_worker::spawn_session_worker(
            session,
            engine,
            sink,
            crate::code::session_worker::AttachmentStore {
                blobs: None,
                private_root,
                engine_reads_images: false,
            },
            Arc::new(tokio::sync::Mutex::new(())),
            tokio::sync::watch::channel(false).1,
        );

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let current = get_session(&store, &owner, session_id)
                    .await
                    .unwrap()
                    .unwrap();
                if current.lifecycle == SessionLifecycle::Running {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the recovered worker re-attaches the parked turn");

        let (decide_reply, decide_response) = tokio::sync::oneshot::channel();
        handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::Decide {
                approval: tidebreak_harness::HarnessApprovalRef::engine("call-1"),
                decision: Box::new(tidebreak_harness::ApprovalDecision::Approve),
                reply: decide_reply,
            })
            .await
            .unwrap();
        decide_response.await.unwrap().unwrap();

        let completed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let turn = get_turn(&store, &owner, turn_id).await.unwrap().unwrap();
                if turn.status == TurnStatus::Completed {
                    break turn;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the park resolves after resume");
        assert_eq!(completed.park_ref, None);
        assert_eq!(
            adapter.resumes().len(),
            1,
            "one resume for the recovered park"
        );
        let events = list_events(&store, &owner, session_id, 0, MAX_REPLAY_EVENTS)
            .await
            .unwrap()
            .events;
        assert!(
            events
                .iter()
                .any(|event| matches!(event.event, Event::TurnResumed { .. })),
            "the journal records the resume"
        );
        let _ = handle
            .commands
            .send(crate::code::session_worker::WorkerCommand::Shutdown)
            .await;
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
