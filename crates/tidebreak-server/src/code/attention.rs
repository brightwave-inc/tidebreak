//! Server-side attention computation and digest publication.
//!
//! Every attention write goes through [`replace_attention`]. Digests are
//! cheap: they are assembled from session, workspace, and turn-count rows.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::warn;

use tidebreak_core::db::code::{
    count_turns, get_session, get_workspace, latest_event_created_at, latest_turn,
    latest_watch_for_session, list_approvals, list_recent_events, list_sessions,
    list_sessions_by_lifecycle_all_owners, list_sessions_for_workspace, save_session,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeApprovalState, CodeEvent, CodeSession,
    CodeSessionActivity, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentStatus,
    CodeTurnStatus, DbStore, OwnerId, ToolDetail, WorkspaceId,
};

use super::bus::{CodeEventBus, CodeLiveUpdate, SessionDigest};

/// Running-but-silent threshold. A periodic sweep applies
/// [`AttentionState::Stalled`] past this many seconds.
pub(crate) const STALL_IDLE_SECS: u32 = 90;

/// How often the stall sweep walks running sessions.
pub(crate) const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const ACTIVITY_EVENT_WINDOW: u64 = 256;

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
        bus.set_maybe_stalled(
            session.id,
            matches!(session.attention.state, AttentionState::Stalled { .. }),
        );
        emit_digest(db, bus, session).await;
    }
    Ok(ok)
}

/// Load, gate, persist, and publish. For callers that do not already hold
/// the row (the stall sweep, the events-socket view signal, the user route).
pub(crate) async fn apply_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    next: Attention,
    from_user: bool,
) -> Result<Option<Attention>, tidebreak_core::AgentError> {
    let Some(mut session) = get_session(db, owner, session_id).await? else {
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
    bus: &CodeEventBus,
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
    let pending = list_approvals(
        db,
        &session.owner,
        Some(CodeApprovalState::Pending),
        Some(session.id),
    )
    .await?;
    if !pending.is_empty() {
        return Ok(Attention::needs_you(
            "an approval is waiting",
            AttentionSource::Structured,
        ));
    }
    if session.lifecycle == CodeSessionLifecycle::Running {
        let last = last_activity_at(db, bus, session).await?;
        let idle = opts.now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle >= opts.stall_idle_secs {
            return Ok(Attention::new(
                AttentionState::Stalled { idle_secs: idle },
                AttentionSource::Heuristic,
            ));
        }
        return Ok(Attention::working(AttentionSource::Lifecycle));
    }
    match latest_turn(db, &session.owner, session.id).await? {
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
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<(), tidebreak_core::AgentError> {
    let Some(session) = get_session(db, owner, session_id).await? else {
        return Ok(());
    };
    if !matches!(session.attention.state, AttentionState::DoneUnreviewed) {
        return Ok(());
    }
    let _ = apply_attention(
        db,
        bus,
        owner,
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
    owner: &OwnerId,
    session_id: CodeSessionId,
    clear: bool,
    note: Option<String>,
) -> Result<CodeSession, tidebreak_core::AgentError> {
    let Some(mut session) = get_session(db, owner, session_id).await? else {
        return Err(tidebreak_core::AgentError::Store(format!(
            "session {session_id} not found"
        )));
    };
    let next = if clear {
        compute_attention(
            db,
            bus,
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
    let running = list_sessions_by_lifecycle_all_owners(db, CodeSessionLifecycle::Running).await?;
    for session in running {
        let last = last_activity_at(db, bus, &session).await?;
        let idle = now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle < idle_secs {
            continue;
        }
        let next = Attention::new(
            AttentionState::Stalled { idle_secs: idle },
            AttentionSource::Heuristic,
        );
        let _ = apply_attention(db, bus, &session.owner, session.id, next, false).await?;
    }
    Ok(())
}

/// Activity on a running session clears a stall.
///
/// Called for every event a session produces, so the cheap answer comes
/// first: the bus remembers whether this session might be stalled, and the
/// common case — a session that is plainly working — returns without reading
/// the row at all. The hint starts pessimistic and every read corrects it,
/// so a wrong guess costs one query, never a missed clear.
pub(crate) async fn note_activity(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
) -> Result<(), tidebreak_core::AgentError> {
    if !bus.maybe_stalled(session_id) {
        return Ok(());
    }
    let Some(session) = get_session(db, owner, session_id).await? else {
        return Ok(());
    };
    let stalled = matches!(session.attention.state, AttentionState::Stalled { .. });
    bus.set_maybe_stalled(session_id, stalled);
    if session.lifecycle != CodeSessionLifecycle::Running {
        return Ok(());
    }
    if !stalled {
        return Ok(());
    }
    let _ = apply_attention(
        db,
        bus,
        owner,
        session_id,
        Attention::working(AttentionSource::Lifecycle),
        false,
    )
    .await?;
    Ok(())
}

pub(crate) async fn emit_digest(db: &DbStore, bus: &CodeEventBus, session: &CodeSession) {
    match build_digest(db, session).await {
        Ok(digest) => {
            bus.publish_update(&session.owner, CodeLiveUpdate::Digest(Box::new(digest)));
        }
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
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) {
    match list_sessions_for_workspace(db, owner, workspace_id).await {
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

/// The owner's live session digests, restated on every `/code/updates`
/// connect. Scoped: a subscriber never learns that another owner's session
/// exists.
pub(crate) async fn list_digests(
    db: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<SessionDigest>, tidebreak_core::AgentError> {
    let mut out = Vec::new();
    for session in list_sessions(db, owner).await? {
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
    let workspace = get_workspace(db, &session.owner, session.workspace_id)
        .await?
        .ok_or_else(|| {
            tidebreak_core::AgentError::Store(format!(
                "workspace {} missing for session {}",
                session.workspace_id, session.id
            ))
        })?;
    let turn_count = count_turns(db, &session.owner, session.id).await?;
    // A watch session's lifecycle undersells it ("running" for hours), so its
    // digest carries the watch's own state. The row is small and local — this
    // never reaches the host the way the PR snapshot does.
    let watch = if session.kind == CodeSessionKind::Watch {
        latest_watch_for_session(db, &session.owner, session.id).await?
    } else {
        None
    };
    let activity = if session.lifecycle == CodeSessionLifecycle::Running {
        let events =
            list_recent_events(db, &session.owner, session.id, ACTIVITY_EVENT_WINDOW).await?;
        Some(session_activity(session, &events))
    } else {
        None
    };
    Ok(SessionDigest {
        workspace: session.workspace_id,
        session: session.id,
        kind: session.kind,
        lifecycle: session.lifecycle,
        attention: session.attention.clone(),
        title: workspace.title,
        turn_count,
        activity,
        pr_state: workspace.pr,
        watch_state: watch.as_ref().map(|watch| watch.state),
        watch_detail: watch.as_ref().and_then(|watch| watch.detail.clone()),
        watch_cycles: watch.as_ref().map(|watch| watch.cycles),
        subagents: if session.subagents.is_empty() {
            None
        } else {
            Some(session.subagents.clone())
        },
    })
}

fn session_activity(
    session: &CodeSession,
    events: &[tidebreak_core::SequencedCodeEvent],
) -> CodeSessionActivity {
    if session
        .subagents
        .iter()
        .any(|entry| entry.status == CodeSubagentStatus::Running)
    {
        return CodeSessionActivity::Subagents;
    }

    let mut completed = HashSet::new();
    for sequenced in events {
        match &sequenced.event {
            CodeEvent::ToolCompleted {
                call_id,
                parent_call_id: None,
                ..
            } => {
                completed.insert(call_id.as_str());
            }
            CodeEvent::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id: None,
            } if !completed.contains(call_id.as_str()) => {
                return classify_activity(name, detail);
            }
            CodeEvent::TurnStarted { .. }
            | CodeEvent::TurnCompleted { .. }
            | CodeEvent::TurnFailed { .. }
            | CodeEvent::TurnInterrupted => break,
            _ => {}
        }
    }
    CodeSessionActivity::Agent
}

fn classify_activity(name: &str, detail: &ToolDetail) -> CodeSessionActivity {
    let normalized = name.to_ascii_lowercase().replace(['_', '-'], "");
    if normalized == "task" {
        return CodeSessionActivity::Subagents;
    }
    if normalized.contains("output")
        || normalized.contains("monitor")
        || normalized.starts_with("wait")
    {
        return CodeSessionActivity::Monitor;
    }
    match detail {
        ToolDetail::Command { .. } => CodeSessionActivity::Shell,
        ToolDetail::FileEdit { .. } | ToolDetail::FileRead { .. } => CodeSessionActivity::File,
        ToolDetail::Search { .. } => CodeSessionActivity::Search,
        ToolDetail::Other { .. } => CodeSessionActivity::Tool,
    }
}

/// When this session last showed a sign of life.
///
/// The journal answers for anything durable. It cannot answer for assistant
/// deltas, which stream and are never written down, so the live bus is asked
/// first: a session pouring out a long answer and touching nothing else is
/// working, not silent, and the stall sweep must not call it stalled.
async fn last_activity_at(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
) -> Result<DateTime<Utc>, tidebreak_core::AgentError> {
    let journaled = match latest_event_created_at(db, &session.owner, session.id).await? {
        Some(at) => at,
        None => match latest_turn(db, &session.owner, session.id).await? {
            Some(turn) => turn.started_at,
            None => session.created_at,
        },
    };
    Ok(match bus.last_activity(session.id) {
        Some(live) => journaled.max(live),
        None => journaled,
    })
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
    use tidebreak_core::{
        should_replace, CodeSessionKind, CodeSubagentSummary, FenceReason, SequencedCodeEvent,
        ToolOutcome, WorkspaceId,
    };

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
            owner: tidebreak_core::OwnerId::local(),
            workspace_id: WorkspaceId::new(),
            kind: CodeSessionKind::Interactive,
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: tidebreak_core::CodePermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            spawn_epoch: 1,
            attention,
            unrecognized_event_count: 0,
            subagents: Vec::new(),
            created_at: Utc::now(),
        }
    }

    fn sequenced(seq: i64, event: CodeEvent) -> SequencedCodeEvent {
        SequencedCodeEvent { seq, event }
    }

    fn started(name: &str, detail: ToolDetail) -> CodeEvent {
        CodeEvent::ToolStarted {
            call_id: "tool-1".into(),
            name: name.into(),
            detail,
            parent_call_id: None,
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

    #[test]
    fn running_command_is_shell_activity() {
        let session = session_with(auto_working());
        let events = [sequenced(
            2,
            started(
                "Bash",
                ToolDetail::Command {
                    cmd: "cargo test".into(),
                    cwd: "/workspace".into(),
                },
            ),
        )];

        assert_eq!(
            session_activity(&session, &events),
            CodeSessionActivity::Shell
        );
    }

    #[test]
    fn output_or_wait_tool_is_monitor_activity() {
        let session = session_with(auto_working());
        let events = [sequenced(
            2,
            started(
                "TaskOutput",
                ToolDetail::Other {
                    summary: "waiting for a background command".into(),
                },
            ),
        )];

        assert_eq!(
            session_activity(&session, &events),
            CodeSessionActivity::Monitor
        );
    }

    #[test]
    fn running_subagent_is_subagent_activity() {
        let mut session = session_with(auto_working());
        session.subagents.push(CodeSubagentSummary {
            call_id: "task-1".into(),
            name: "Inspect the parser".into(),
            status: CodeSubagentStatus::Running,
        });

        assert_eq!(
            session_activity(&session, &[]),
            CodeSessionActivity::Subagents
        );
    }

    #[test]
    fn completed_tool_returns_to_agent_activity() {
        let session = session_with(auto_working());
        let events = [
            sequenced(
                3,
                CodeEvent::ToolCompleted {
                    call_id: "tool-1".into(),
                    outcome: ToolOutcome::Succeeded,
                    preview: "done".into(),
                    detail: None,
                    parent_call_id: None,
                },
            ),
            sequenced(
                2,
                started(
                    "Bash",
                    ToolDetail::Command {
                        cmd: "cargo test".into(),
                        cwd: "/workspace".into(),
                    },
                ),
            ),
        ];

        assert_eq!(
            session_activity(&session, &events),
            CodeSessionActivity::Agent
        );
    }
}
