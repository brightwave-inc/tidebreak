//! Server-side attention computation and digest publication.
//!
//! Every attention write goes through [`replace_attention`]. Digests are
//! cheap: they are assembled from session, workspace, and turn-count rows.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::warn;

use tidebreak_core::db::code::{
    count_attributed_prs_for_workspace, count_turns, get_session, get_workspace,
    latest_event_created_at, latest_turn, latest_watch_for_session, list_approvals,
    list_recent_events, list_sessions, list_sessions_by_lifecycle_all_owners,
    list_sessions_for_workspace, list_turns, replace_session_attention, save_session,
};
use tidebreak_core::{
    Attention, AttentionSource, AttentionState, CodeApprovalState, CodeEvent, CodeSession,
    CodeSessionActivity, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeSubagentStatus,
    CodeTurnStatus, DbStore, OwnerId, ToolDetail, WorkspaceId,
};

use super::bus::{CodeEventBus, CodeLiveUpdate, SessionDigest};
use super::trigger_target_at;

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

/// Persist general session fields and route attention through its targeted
/// row-locked write before publishing the stored digest.
pub(crate) async fn persist_session(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &CodeSession,
) -> Result<bool, tidebreak_core::AgentError> {
    let ok = save_session(db, session).await?;
    if ok {
        let _ =
            replace_session_attention(db, &session.owner, session.id, &session.attention, false)
                .await?;
        let Some(stored) = get_session(db, &session.owner, session.id).await? else {
            return Ok(false);
        };
        bus.set_maybe_stalled(
            stored.id,
            matches!(stored.attention.state, AttentionState::Stalled { .. }),
        );
        emit_digest(db, bus, &stored).await;
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
    let Some(changed) = replace_session_attention(db, owner, session_id, &next, from_user).await?
    else {
        return Ok(None);
    };
    let Some(session) = get_session(db, owner, session_id).await? else {
        return Ok(None);
    };
    bus.set_maybe_stalled(
        session.id,
        matches!(session.attention.state, AttentionState::Stalled { .. }),
    );
    emit_digest(db, bus, &session).await;
    Ok(Some(changed))
}

/// Apply attention created by one durable trigger delivery.
pub(crate) async fn apply_trigger_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    delivery_id: tidebreak_core::CodeTriggerDeliveryId,
    lease_token: uuid::Uuid,
    next: Attention,
) -> Result<(), tidebreak_core::AgentError> {
    let changed = tidebreak_core::db::code::accept_trigger_attention_delivery(
        db,
        owner,
        delivery_id,
        lease_token,
        session_id,
        &next,
        Utc::now(),
    )
    .await?;
    if changed {
        if let Some(session) = get_session(db, owner, session_id).await? {
            bus.set_maybe_stalled(
                session.id,
                matches!(session.attention.state, AttentionState::Stalled { .. }),
            );
            emit_digest(db, bus, &session).await;
        }
    }
    Ok(())
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
        // Nothing running, nothing waiting, nothing unreviewed. Reporting
        // `Working` here — as this arm used to — claims an engine is busy
        // when none is, and a session with no turns yet has never been busy
        // at all.
        _ => Ok(Attention::new(
            AttentionState::Idle,
            AttentionSource::Lifecycle,
        )),
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
    // The session is DoneUnreviewed, so no turn is running. Viewing it settles
    // it; it does not start an engine. This wrote `Working` because `Idle` did
    // not exist, which meant looking at a finished session made it claim to be
    // busy for as long as it lived.
    let _ = apply_attention(
        db,
        bus,
        owner,
        session_id,
        Attention::new(AttentionState::Idle, AttentionSource::Lifecycle),
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
    let Some(session) = get_session(db, owner, session_id).await? else {
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
    let _ = apply_attention(db, bus, owner, session_id, next, true).await?;
    get_session(db, owner, session_id)
        .await?
        .ok_or_else(|| tidebreak_core::AgentError::Store(format!("session {session_id} not found")))
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
    let workspace = match session.workspace_id {
        Some(workspace_id) => Some(
            get_workspace(db, &session.owner, workspace_id)
                .await?
                .ok_or_else(|| {
                    tidebreak_core::AgentError::Store(format!(
                        "workspace {workspace_id} missing for session {}",
                        session.id
                    ))
                })?,
        ),
        None => None,
    };
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
    // One indexed count (decision 77). Zero stays absent so clients that
    // predate the field and workspaces that never shipped read the same.
    let pr_count = match session.workspace_id {
        Some(workspace_id) => {
            count_attributed_prs_for_workspace(db, &session.owner, workspace_id).await?
        }
        None => 0,
    };
    let turns = list_turns(db, &session.owner, session.id).await?;
    // Keep this timestamp aligned with trigger delivery's ranking rule. The
    // interface uses it to name the same session that a fire would reach.
    let trigger_target_at =
        trigger_target_at(session.created_at, turns.last().map(|turn| turn.started_at));
    // The newest recapped turn speaks for the session: a turn the model
    // declined to recap, or one whose call is still in flight, leaves the
    // previous line standing rather than blanking the row mid-work.
    let recap = turns.into_iter().rev().find_map(|turn| turn.narrative);
    // A session with no workspace is titled by its conversation, which
    // nothing names yet; the client falls back to its own label.
    let (title, pr_state) = match workspace {
        Some(workspace) => (workspace.title, workspace.pr),
        None => (String::new(), None),
    };
    Ok(SessionDigest {
        workspace: session.workspace_id,
        session: session.id,
        kind: session.kind,
        harness_kind: session.harness_kind,
        lifecycle: session.lifecycle,
        attention: session.attention.clone(),
        title,
        turn_count,
        trigger_target_at,
        activity,
        pr_state,
        pr_count: (pr_count > 0).then_some(pr_count),
        watch_state: watch.as_ref().map(|watch| watch.state),
        watch_detail: watch.as_ref().and_then(|watch| watch.detail.clone()),
        watch_cycles: watch.as_ref().map(|watch| watch.cycles),
        subagents: if session.subagents.is_empty() {
            None
        } else {
            Some(session.subagents.clone())
        },
        recap,
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
            | CodeEvent::TurnRefused { .. }
            | CodeEvent::TurnFailed { .. }
            | CodeEvent::TurnInterrupted { .. } => break,
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
            workspace_id: Some(WorkspaceId::new()),
            kind: CodeSessionKind::Interactive,
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: tidebreak_core::PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: CodeSessionLifecycle::Idle,
            fence_reason: None,
            child_pid: None,
            child_process_identity: None,
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
                    output: None,
                    action: None,
                    result: None,
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
