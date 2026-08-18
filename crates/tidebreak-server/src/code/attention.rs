//! Server-side attention computation and digest publication.
//!
//! Every attention write goes through [`replace_attention`]. Digests are
//! cheap: they are assembled from session, workspace, and turn-count rows.

use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::warn;

use tidebreak_core::db::code::{
    count_turns, get_session, get_workspace, latest_event_created_at, latest_turn, list_approvals,
    list_sessions, list_sessions_by_lifecycle, list_sessions_for_workspace, save_session,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeApprovalState, CodeSession, CodeSessionId,
    CodeSessionLifecycle, CodeTurnStatus, DbStore, WorkspaceId,
};

use super::bus::{CodeEventBus, CodeLiveUpdate, SessionDigest};

/// Running-but-silent threshold. A periodic sweep applies
/// [`AttentionState::Stalled`] past this many seconds.
pub(crate) const STALL_IDLE_SECS: u32 = 90;

/// How often the stall sweep walks running sessions.
pub(crate) const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// The only function that may write attention onto a session value.
///
/// `from_user` is the explicit pin/clear path: it may replace anything,
/// including [`AttentionState::Manual`]. Automatic writes still go through
/// [`tidebreak_core::should_replace`].
pub(crate) fn replace_attention(
    session: &mut CodeSession,
    next: Attention,
    from_user: bool,
) -> bool {
    if !from_user && !tidebreak_core::should_replace(&session.attention, &next) {
        return false;
    }
    if session.attention == next {
        return false;
    }
    session.attention = next;
    true
}

/// Persist the session row and publish its digest. The write path for
/// lifecycle and attention mutations that already hold the row.
pub(crate) async fn persist_session(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
) -> Result<bool, tidebreak_core::AgentError> {
    let ok = save_session(db, session).await?;
    if ok {
        emit_digest(db, bus, session).await;
    }
    Ok(ok)
}

/// Load, gate, persist, and publish. For callers that do not already hold
/// the row (the stall sweep, the events-socket view signal, the user route).
pub(crate) async fn apply_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
    next: Attention,
    from_user: bool,
) -> Result<Option<Attention>, tidebreak_core::AgentError> {
    let Some(mut session) = get_session(db, session_id).await? else {
        return Ok(None);
    };
    if !replace_attention(&mut session, next, from_user) {
        return Ok(None);
    }
    persist_session(db, bus, &session).await?;
    Ok(Some(session.attention))
}

/// Options for [`compute_attention`].
pub(crate) struct ComputeOpts {
    /// Treat a completed turn as already reviewed (clear / view).
    pub reviewed: bool,
    pub now: DateTime<Utc>,
    pub stall_idle_secs: u32,
}

impl Default for ComputeOpts {
    fn default() -> Self {
        Self {
            reviewed: false,
            now: Utc::now(),
            stall_idle_secs: STALL_IDLE_SECS,
        }
    }
}

/// Attention implied by current rows. Used to restore state after a user
/// clear, never to second-guess a live Manual pin.
pub(crate) async fn compute_attention(
    db: &DbStore,
    session: &CodeSession,
    opts: ComputeOpts,
) -> Result<Attention, tidebreak_core::AgentError> {
    if session.lifecycle == CodeSessionLifecycle::Fenced {
        if let Some(reason) = session.fence_reason.clone() {
            return Ok(Attention::new(
                AttentionState::Fenced { reason },
                AttentionSource::Lifecycle,
            ));
        }
    }
    let pending = list_approvals(db, Some(CodeApprovalState::Pending), Some(session.id)).await?;
    if !pending.is_empty() {
        return Ok(Attention::needs_you(
            "an approval is waiting",
            AttentionSource::Structured,
        ));
    }
    if session.lifecycle == CodeSessionLifecycle::Running {
        let last = last_activity_at(db, session).await?;
        let idle = opts.now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle >= opts.stall_idle_secs {
            return Ok(Attention::new(
                AttentionState::Stalled { idle_secs: idle },
                AttentionSource::Heuristic,
            ));
        }
        return Ok(Attention::working(AttentionSource::Lifecycle));
    }
    match latest_turn(db, session.id).await? {
        Some(turn) if turn.status == CodeTurnStatus::Failed => Ok(Attention::needs_you(
            "the engine turn failed",
            AttentionSource::Lifecycle,
        )),
        Some(turn) if turn.status == CodeTurnStatus::Interrupted => Ok(Attention::needs_you(
            "the turn was interrupted",
            AttentionSource::Lifecycle,
        )),
        Some(turn) if turn.status == CodeTurnStatus::Completed && !opts.reviewed => Ok(
            Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle),
        ),
        _ => Ok(Attention::working(AttentionSource::Lifecycle)),
    }
}

/// Mark a session as viewed: a connected events socket clears
/// [`AttentionState::DoneUnreviewed`].
pub(crate) async fn mark_viewed(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
) -> Result<(), tidebreak_core::AgentError> {
    let Some(session) = get_session(db, session_id).await? else {
        return Ok(());
    };
    if !matches!(session.attention.state, AttentionState::DoneUnreviewed) {
        return Ok(());
    }
    let _ = apply_attention(
        db,
        bus,
        session_id,
        Attention::working(AttentionSource::Lifecycle),
        false,
    )
    .await?;
    Ok(())
}

/// User pin (`note`) or clear (restore computed, treating the session as
/// reviewed so DoneUnreviewed does not bounce back).
pub(crate) async fn user_set_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
    clear: bool,
    note: Option<String>,
) -> Result<CodeSession, tidebreak_core::AgentError> {
    let Some(mut session) = get_session(db, session_id).await? else {
        return Err(tidebreak_core::AgentError::Store(format!(
            "session {session_id} not found"
        )));
    };
    let next = if clear {
        compute_attention(
            db,
            &session,
            ComputeOpts {
                reviewed: true,
                ..ComputeOpts::default()
            },
        )
        .await?
    } else {
        Attention::manual(note.unwrap_or_default())
    };
    replace_attention(&mut session, next, true);
    persist_session(db, bus, &session).await?;
    Ok(session)
}

/// Walk running sessions and apply [`AttentionState::Stalled`] when silent.
pub(crate) async fn sweep_stalled(
    db: &DbStore,
    bus: &CodeEventBus,
    idle_secs: u32,
) -> Result<(), tidebreak_core::AgentError> {
    let now = Utc::now();
    let running = list_sessions_by_lifecycle(db, CodeSessionLifecycle::Running).await?;
    for session in running {
        let last = last_activity_at(db, &session).await?;
        let idle = now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle < idle_secs {
            continue;
        }
        let next = Attention::new(
            AttentionState::Stalled { idle_secs: idle },
            AttentionSource::Heuristic,
        );
        let _ = apply_attention(db, bus, session.id, next, false).await?;
    }
    Ok(())
}

/// Activity on a running session clears a stall.
pub(crate) async fn note_activity(
    db: &DbStore,
    bus: &CodeEventBus,
    session_id: CodeSessionId,
) -> Result<(), tidebreak_core::AgentError> {
    let Some(session) = get_session(db, session_id).await? else {
        return Ok(());
    };
    if session.lifecycle != CodeSessionLifecycle::Running {
        return Ok(());
    }
    if !matches!(session.attention.state, AttentionState::Stalled { .. }) {
        return Ok(());
    }
    let _ = apply_attention(
        db,
        bus,
        session_id,
        Attention::working(AttentionSource::Lifecycle),
        false,
    )
    .await?;
    Ok(())
}

pub(crate) async fn emit_digest(db: &DbStore, bus: &CodeEventBus, session: &CodeSession) {
    match build_digest(db, session).await {
        Ok(digest) => bus.publish_update(CodeLiveUpdate::Digest(digest)),
        Err(err) => warn!(
            session = %session.id,
            error = %err,
            "failed to build a code-session digest"
        ),
    }
}

pub(crate) async fn emit_workspace_digests(
    db: &DbStore,
    bus: &CodeEventBus,
    workspace_id: WorkspaceId,
) {
    match list_sessions_for_workspace(db, workspace_id).await {
        Ok(sessions) => {
            for session in sessions {
                emit_digest(db, bus, &session).await;
            }
        }
        Err(err) => warn!(
            workspace = %workspace_id,
            error = %err,
            "failed to list sessions for a workspace digest"
        ),
    }
}

pub(crate) async fn list_digests(
    db: &DbStore,
) -> Result<Vec<SessionDigest>, tidebreak_core::AgentError> {
    let mut out = Vec::new();
    for session in list_sessions(db).await? {
        if session.lifecycle == CodeSessionLifecycle::Ended {
            continue;
        }
        out.push(build_digest(db, &session).await?);
    }
    Ok(out)
}

async fn build_digest(
    db: &DbStore,
    session: &CodeSession,
) -> Result<SessionDigest, tidebreak_core::AgentError> {
    let workspace = get_workspace(db, session.workspace_id)
        .await?
        .ok_or_else(|| {
            tidebreak_core::AgentError::Store(format!(
                "workspace {} missing for session {}",
                session.workspace_id, session.id
            ))
        })?;
    let turn_count = count_turns(db, session.id).await?;
    Ok(SessionDigest {
        workspace: session.workspace_id,
        session: session.id,
        lifecycle: session.lifecycle,
        attention: session.attention.clone(),
        title: workspace.title,
        turn_count,
        pr_state: workspace.pr,
    })
}

async fn last_activity_at(
    db: &DbStore,
    session: &CodeSession,
) -> Result<DateTime<Utc>, tidebreak_core::AgentError> {
    if let Some(at) = latest_event_created_at(db, session.id).await? {
        return Ok(at);
    }
    if let Some(turn) = latest_turn(db, session.id).await? {
        return Ok(turn.started_at);
    }
    Ok(session.created_at)
}

/// Abort the stall sweep when the runtime is dropped.
pub(crate) struct StallSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl StallSweepGuard {
    pub(crate) fn spawn(db: std::sync::Arc<DbStore>, bus: std::sync::Arc<CodeEventBus>) -> Self {
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(STALL_SWEEP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if let Err(err) = sweep_stalled(&db, &bus, STALL_IDLE_SECS).await {
                    warn!(error = %err, "code-mode stall sweep failed");
                }
            }
        });
        Self(Some(handle))
    }
}

impl Drop for StallSweepGuard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tidebreak_core::{should_replace, FenceReason, WorkspaceId};

    fn auto_working() -> Attention {
        Attention::working(AttentionSource::Lifecycle)
    }

    fn structured_need() -> Attention {
        Attention::needs_you("approve this", AttentionSource::Structured)
    }

    fn heuristic_stall() -> Attention {
        Attention::new(
            AttentionState::Stalled { idle_secs: 30 },
            AttentionSource::Heuristic,
        )
    }

    fn session_with(attention: Attention) -> CodeSession {
        CodeSession {
            id: CodeSessionId::new(),
            workspace_id: WorkspaceId::new(),
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: tidebreak_core::CodePermissionMode::Plan,
            model: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 1,
            attention,
            unrecognized_event_count: 0,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn chokepoint_never_lets_automatic_replace_manual() {
        let mut session = session_with(Attention::manual("hold"));
        for next in [
            auto_working(),
            Attention::working(AttentionSource::Structured),
            heuristic_stall(),
            structured_need(),
            Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle),
            Attention::new(
                AttentionState::Fenced {
                    reason: FenceReason::OrphanAlive,
                },
                AttentionSource::Lifecycle,
            ),
        ] {
            assert!(
                !replace_attention(&mut session, next.clone(), false),
                "automatic {:?} must not replace Manual",
                next.source
            );
            assert!(matches!(
                session.attention.state,
                AttentionState::Manual { .. }
            ));
        }
        assert!(replace_attention(
            &mut session,
            Attention::manual("updated"),
            true
        ));
        assert!(replace_attention(&mut session, auto_working(), true));
        assert_eq!(session.attention.state, AttentionState::Working);
    }

    #[test]
    fn chokepoint_holds_structured_need_against_heuristic() {
        let mut session = session_with(structured_need());
        assert!(!replace_attention(&mut session, heuristic_stall(), false));
        assert!(!replace_attention(
            &mut session,
            Attention::needs_you("maybe idle?", AttentionSource::Heuristic),
            false
        ));
        assert!(replace_attention(&mut session, auto_working(), false));
    }

    #[test]
    fn chokepoint_agrees_with_should_replace() {
        let currents = [
            auto_working(),
            structured_need(),
            heuristic_stall(),
            Attention::manual("pin"),
            Attention::new(AttentionState::DoneUnreviewed, AttentionSource::Lifecycle),
        ];
        let nexts = [
            auto_working(),
            structured_need(),
            heuristic_stall(),
            Attention::manual("other"),
            Attention::working(AttentionSource::User),
        ];
        for current in &currents {
            for next in &nexts {
                let mut session = session_with(current.clone());
                let applied = replace_attention(&mut session, next.clone(), false);
                let expected = should_replace(current, next) && current != next;
                assert_eq!(applied, expected, "current={current:?} next={next:?}");
            }
        }
    }
}
