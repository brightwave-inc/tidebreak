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
    preview_formatting_character, ApprovalState, Attention, AttentionSource, AttentionState,
    CodeSubagentStatus, DbStore, Event, OwnerId, Session, SessionActivity, SessionId, SessionKind,
    SessionLifecycle, ToolDetail, TurnStatus, WorkspaceId,
};

use super::bus::{CodeEventBus, CodeLiveUpdate, SessionDigest};
use super::trigger_target_at;

/// Running-but-silent threshold. A periodic sweep applies
/// [`AttentionState::Stalled`] past this many seconds.
pub const STALL_IDLE_SECS: u32 = 90;

/// How often the stall sweep walks running sessions.
pub const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(15);
const ACTIVITY_EVENT_WINDOW: u64 = 256;

/// Most characters of a tool subject a digest carries. A rail row shows one
/// line; anything longer is a script, not a label.
pub const ACTIVITY_DETAIL_MAX_CHARS: usize = 120;

/// The only function that may write attention onto a session value.
///
/// `from_user` is the explicit pin/clear path: it may replace anything,
/// including [`AttentionState::Manual`]. Automatic writes still go through
/// [`tidebreak_core::should_replace`].
pub fn replace_attention(session: &mut Session, next: Attention, from_user: bool) -> bool {
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
pub async fn persist_session(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &Session,
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
pub async fn apply_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: SessionId,
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
pub async fn apply_trigger_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: SessionId,
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
pub struct ComputeOpts {
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
pub async fn compute_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    session: &Session,
    opts: ComputeOpts,
) -> Result<Attention, tidebreak_core::AgentError> {
    if session.lifecycle == SessionLifecycle::Fenced {
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
        Some(ApprovalState::Pending),
        Some(session.id),
    )
    .await?;
    if !pending.is_empty() {
        return Ok(Attention::needs_you(
            "an approval is waiting",
            AttentionSource::Structured,
        ));
    }
    if session.lifecycle == SessionLifecycle::Running {
        let last = last_activity_at(db, bus, session).await?;
        let idle = opts.now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle >= opts.stall_idle_secs && !parked_on_background(db, session).await? {
            return Ok(Attention::new(
                AttentionState::Stalled { idle_secs: idle },
                AttentionSource::Heuristic,
            ));
        }
        return Ok(Attention::working(AttentionSource::Lifecycle));
    }
    match latest_turn(db, &session.owner, session.id).await? {
        Some(turn) if turn.status == TurnStatus::Failed => Ok(Attention::needs_you(
            "the engine turn failed",
            AttentionSource::Lifecycle,
        )),
        Some(turn) if turn.status == TurnStatus::Interrupted => Ok(Attention::needs_you(
            "the turn was interrupted",
            AttentionSource::Lifecycle,
        )),
        Some(turn) if turn.status == TurnStatus::Completed && !opts.reviewed => Ok(Attention::new(
            AttentionState::DoneUnreviewed,
            AttentionSource::Lifecycle,
        )),
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
pub async fn mark_viewed(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: SessionId,
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
pub async fn user_set_attention(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: SessionId,
    clear: bool,
    note: Option<String>,
) -> Result<Session, tidebreak_core::AgentError> {
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
pub async fn sweep_stalled(
    db: &DbStore,
    bus: &CodeEventBus,
    idle_secs: u32,
) -> Result<(), tidebreak_core::AgentError> {
    let now = Utc::now();
    let running = list_sessions_by_lifecycle_all_owners(db, SessionLifecycle::Running).await?;
    for session in running {
        let last = last_activity_at(db, bus, &session).await?;
        let idle = now.signed_duration_since(last).num_seconds().max(0) as u32;
        if idle < idle_secs {
            continue;
        }
        // A monitor or a subagent is silent by design: the engine is
        // waiting, not stuck. Calling that stalled would contradict the
        // "Monitoring" the same row says beside it.
        if parked_on_background(db, &session).await? {
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
pub async fn note_activity(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<(), tidebreak_core::AgentError> {
    if !bus.maybe_stalled(session_id) {
        return Ok(());
    }
    let Some(session) = get_session(db, owner, session_id).await? else {
        return Ok(());
    };
    let stalled = matches!(session.attention.state, AttentionState::Stalled { .. });
    bus.set_maybe_stalled(session_id, stalled);
    if session.lifecycle != SessionLifecycle::Running {
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

pub async fn emit_digest(db: &DbStore, bus: &CodeEventBus, session: &Session) {
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

pub async fn emit_workspace_digests(
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

/// The owner's live session digests, restated on every `/updates`
/// connection. Scoped: a subscriber never learns that another owner's session
/// exists.
pub async fn list_accessible_digests(
    db: &DbStore,
    principal: &OwnerId,
) -> Result<Vec<SessionDigest>, tidebreak_core::AgentError> {
    let mut out = Vec::new();
    for session in tidebreak_core::db::code::list_accessible_sessions(db, principal).await? {
        if session.lifecycle != SessionLifecycle::Ended {
            out.push(build_digest(db, &session).await?);
        }
    }
    Ok(out)
}

pub async fn list_digests(
    db: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<SessionDigest>, tidebreak_core::AgentError> {
    let mut out = Vec::new();
    for session in list_sessions(db, owner).await? {
        if session.lifecycle == SessionLifecycle::Ended {
            continue;
        }
        out.push(build_digest(db, &session).await?);
    }
    Ok(out)
}

async fn build_digest(
    db: &DbStore,
    session: &Session,
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
    let watch = if session.kind == SessionKind::Watch {
        latest_watch_for_session(db, &session.owner, session.id).await?
    } else {
        None
    };
    let (activity, activity_detail) = if session.lifecycle == SessionLifecycle::Running {
        let events =
            list_recent_events(db, &session.owner, session.id, ACTIVITY_EVENT_WINDOW).await?;
        let (activity, detail) = session_activity(session, &events);
        (Some(activity), detail)
    } else {
        (None, None)
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
    // Claude Code supplies its own closing recap. Do not carry an older
    // Tidebreak fallback forward after a Claude turn finishes.
    let recap = if session.harness_kind == tidebreak_core::HarnessKind::ClaudeCode {
        None
    } else {
        turns.into_iter().rev().find_map(|turn| turn.narrative)
    };
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
        activity_detail,
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
        memory_proposal_count: memory_proposal_count(db, session).await,
    })
}

async fn memory_proposal_count(db: &DbStore, session: &Session) -> Option<u64> {
    use tidebreak_core::{MemoryBackend, MemoryEvidence, MemoryListFilter, MemoryStatus};
    let records = db
        .list(
            &session.owner,
            MemoryListFilter {
                scope: None,
                statuses: vec![MemoryStatus::Proposed],
                kinds: Vec::new(),
            },
        )
        .await
        .ok()?;
    // Only what this session's own turns produced: the origin names the
    // session and the evidence cites its journal, so a record that merely
    // mentions the session id elsewhere does not inflate the chip.
    let count = records
        .into_iter()
        .filter(|record| {
            record.provenance.origin.code_session_id == Some(session.id)
                && record.provenance.evidence.iter().any(|evidence| {
                    matches!(
                        evidence,
                        MemoryEvidence::Event { session_id, .. } if *session_id == session.id
                    )
                })
        })
        .count() as u64;
    (count > 0).then_some(count)
}

/// Whether a running session is parked on work that is silent by design: a
/// monitor tool, or one or more harness subagents.
async fn parked_on_background(
    db: &DbStore,
    session: &Session,
) -> Result<bool, tidebreak_core::AgentError> {
    let events = list_recent_events(db, &session.owner, session.id, ACTIVITY_EVENT_WINDOW).await?;
    Ok(matches!(
        session_activity(session, &events).0,
        SessionActivity::Monitor | SessionActivity::Subagents
    ))
}

/// What the live turn is occupied with, and the subject of the tool it is
/// waiting on when that tool named one.
fn session_activity(
    session: &Session,
    events: &[tidebreak_core::code::SequencedEvent],
) -> (SessionActivity, Option<String>) {
    if session
        .subagents
        .iter()
        .any(|entry| entry.status == CodeSubagentStatus::Running)
    {
        return (SessionActivity::Subagents, None);
    }

    let mut completed = HashSet::new();
    for sequenced in events {
        match &sequenced.event {
            Event::ToolCompleted {
                call_id,
                parent_call_id: None,
                ..
            } => {
                completed.insert(call_id.as_str());
            }
            Event::ToolStarted {
                call_id,
                name,
                detail,
                parent_call_id: None,
            } if !completed.contains(call_id.as_str()) => {
                return (classify_activity(name, detail), activity_detail(detail));
            }
            Event::TurnStarted { .. }
            | Event::TurnCompleted { .. }
            | Event::TurnRefused { .. }
            | Event::TurnFailed { .. }
            | Event::TurnInterrupted { .. } => break,
            _ => {}
        }
    }
    (SessionActivity::Agent, None)
}

/// One line naming what the tool is doing, or nothing when the detail has no
/// subject yet (an engine can open a call before its arguments stream in).
///
/// The subject is harness text, so this clamps it the way every other
/// one-line projection does: the first line, minus any character that could
/// redraw or reorder it, and nothing at all when that leaves nothing. The
/// desktop rejects a digest whose detail carries such a character, and a
/// rejected digest blanks the whole rail.
fn activity_detail(detail: &ToolDetail) -> Option<String> {
    let subject = detail.subject().trim();
    let first_line: String = subject
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|character| !preview_formatting_character(*character))
        .collect();
    let first_line = first_line.trim();
    if first_line.is_empty() {
        return None;
    }
    let mut chars = first_line.chars();
    let bounded: String = chars.by_ref().take(ACTIVITY_DETAIL_MAX_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{bounded}\u{2026}")
    } else {
        bounded
    })
}

fn classify_activity(name: &str, detail: &ToolDetail) -> SessionActivity {
    let normalized = name.to_ascii_lowercase().replace(['_', '-'], "");
    if normalized == "task" {
        return SessionActivity::Subagents;
    }
    if normalized.contains("output")
        || normalized.contains("monitor")
        || normalized.starts_with("wait")
    {
        return SessionActivity::Monitor;
    }
    match detail {
        ToolDetail::Command { .. } => SessionActivity::Shell,
        ToolDetail::FileEdit { .. } | ToolDetail::FileRead { .. } => SessionActivity::File,
        ToolDetail::Search { .. } => SessionActivity::Search,
        ToolDetail::Other { .. } => SessionActivity::Tool,
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
    session: &Session,
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
pub struct StallSweepGuard(Option<tokio::task::JoinHandle<()>>);

impl StallSweepGuard {
    pub fn spawn(db: std::sync::Arc<DbStore>, bus: std::sync::Arc<CodeEventBus>) -> Self {
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
    use tidebreak_core::code::SequencedEvent;
    use tidebreak_core::{
        should_replace, CodeSubagentSummary, FenceReason, SessionKind, ToolOutcome, WorkspaceId,
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

    fn session_with(attention: Attention) -> Session {
        Session {
            id: SessionId::new(),
            owner: tidebreak_core::OwnerId::local(),
            workspace_id: Some(WorkspaceId::new()),
            kind: SessionKind::Interactive,
            harness_kind: tidebreak_core::HarnessKind::ClaudeCode,
            harness_version: None,
            harness_resume_ref: None,
            permission_mode: tidebreak_core::PermissionMode::Plan,
            model: None,
            reasoning_effort: None,
            fast_mode: false,
            lifecycle: SessionLifecycle::Idle,
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

    fn sequenced(seq: i64, event: Event) -> SequencedEvent {
        SequencedEvent { seq, event }
    }

    fn started(name: &str, detail: ToolDetail) -> Event {
        Event::ToolStarted {
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
            (SessionActivity::Shell, Some("cargo test".to_owned()))
        );
    }

    #[test]
    fn activity_detail_keeps_one_bounded_line() {
        let script = format!("{}\nrm -rf target", "x".repeat(200));
        let detail = activity_detail(&ToolDetail::Command {
            cmd: script,
            cwd: "/workspace".into(),
        })
        .expect("a command names its subject");
        assert_eq!(detail.chars().count(), ACTIVITY_DETAIL_MAX_CHARS + 1);
        assert!(detail.ends_with('\u{2026}'));
        assert!(!detail.contains("rm -rf"));

        assert_eq!(
            activity_detail(&ToolDetail::Other {
                summary: "   ".into()
            }),
            None
        );
    }

    #[test]
    fn activity_detail_strips_what_a_line_cannot_carry() {
        // A tab, an escape, and a bidi override: the desktop rejects a digest
        // carrying any of them, so the clamp removes them before the wire.
        let detail = activity_detail(&ToolDetail::Command {
            cmd: "printf\t'\u{1b}[31m'\u{202e}&& cargo test".into(),
            cwd: "/workspace".into(),
        })
        .expect("the command still names its subject");
        assert_eq!(detail, "printf'[31m'&& cargo test");
        assert!(!detail.chars().any(preview_formatting_character));

        assert_eq!(
            activity_detail(&ToolDetail::Other {
                summary: "\u{202e}\t\u{7f}".into()
            }),
            None,
            "a subject that clamps away to nothing is omitted, not sent empty"
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
            (
                SessionActivity::Monitor,
                Some("waiting for a background command".to_owned())
            )
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
            (SessionActivity::Subagents, None)
        );
    }

    #[test]
    fn completed_tool_returns_to_agent_activity() {
        let session = session_with(auto_working());
        let events = [
            sequenced(
                3,
                Event::ToolCompleted {
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
            (SessionActivity::Agent, None)
        );
    }
}
