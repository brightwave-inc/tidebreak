//! Reconcile approvals whose tool call resolved before anyone decided.
//!
//! An approval is parked on the engine's `tool_use_id`, and the same id comes
//! back on [`CodeEvent::ToolCompleted`]. When a completion arrives for a call
//! that still has a pending approval, nobody's decision can reach the engine
//! any more: the engine timed the call out, denied it itself, or ran it under
//! a rule of its own. Leaving the row `Pending` would list a request that can
//! never be acted on, and would let a later `code approve` report success for
//! a command the engine already failed.
//!
//! So the row moves to [`CodeApprovalState::Abandoned`] and the session
//! journals an `ApprovalResolved` carrying
//! [`ApprovalDecisionKind::Abandoned`]. The turn and session boundaries sweep
//! the same way, because a tool call that never reports completion must not
//! leave a row pending forever.

use tidebreak_core::db::code::{list_approvals, list_turns, save_approval};
use tidebreak_core::{
    ApprovalDecisionKind, Attention, AttentionSource, CodeApproval, CodeApprovalState, CodeEvent,
    CodeSessionId, CodeTurnStatus, DbStore, OwnerId,
};

use super::bus::CodeEventBus;

/// The engine-native call id an approval row was parked on.
///
/// Written as a sibling of the capped payload, so an oversized request still
/// carries it — the same field [`super::runtime::CodeRuntime::decide_approval`]
/// reads.
fn call_id_of(approval: &CodeApproval) -> Option<&str> {
    approval
        .harness_raw
        .get("call_id")
        .and_then(serde_json::Value::as_str)
        .filter(|call_id| !call_id.is_empty())
}

/// Abandon the pending approval parked on `call_id`, if there is one.
pub(crate) async fn abandon_for_call(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    call_id: &str,
) {
    if call_id.is_empty() {
        return;
    }
    let doomed: Vec<CodeApproval> = pending(db, owner, session_id)
        .await
        .into_iter()
        .filter(|approval| call_id_of(approval) == Some(call_id))
        .collect();
    abandon(db, bus, owner, session_id, spawn_epoch, doomed).await;
}

/// Abandon every pending approval whose turn has already reached a terminal
/// status. The turn-end sweep: an engine that drops a tool call without
/// reporting its completion still ends the turn.
pub(crate) async fn abandon_for_settled_turns(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
) {
    let waiting = pending(db, owner, session_id).await;
    if waiting.is_empty() {
        return;
    }
    let Ok(turns) = list_turns(db, owner, session_id).await else {
        return;
    };
    let settled: Vec<_> = turns
        .iter()
        .filter(|turn| turn.status != CodeTurnStatus::Running)
        .map(|turn| turn.id)
        .collect();
    let doomed: Vec<CodeApproval> = waiting
        .into_iter()
        .filter(|approval| settled.contains(&approval.turn_id))
        .collect();
    abandon(db, bus, owner, session_id, spawn_epoch, doomed).await;
}

/// Abandon every pending approval on the session, whatever its turn says.
/// The session-end sweep: no later decision can reach a stopped engine.
pub(crate) async fn abandon_for_ended_session(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
) {
    let doomed = pending(db, owner, session_id).await;
    abandon(db, bus, owner, session_id, spawn_epoch, doomed).await;
}

async fn pending(db: &DbStore, owner: &OwnerId, session_id: CodeSessionId) -> Vec<CodeApproval> {
    list_approvals(
        db,
        owner,
        Some(CodeApprovalState::Pending),
        Some(session_id),
    )
    .await
    .unwrap_or_default()
}

/// Mark each row abandoned and journal the resolution.
///
/// Best-effort throughout: this runs on event and lifecycle paths that must
/// not fail because a reconciliation write did. A row that loses the race
/// with a real decision simply fails the pending filter next time round.
async fn abandon(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
    doomed: Vec<CodeApproval>,
) {
    if doomed.is_empty() {
        return;
    }
    let now = chrono::Utc::now();
    for mut approval in doomed {
        approval.state = CodeApprovalState::Abandoned;
        approval.decided_at = Some(now);
        match save_approval(db, owner, &approval).await {
            Ok(true) => {}
            _ => continue,
        }
        // Boxed because the turn-end sweep is itself reached from the
        // journal path: without it the two futures would size each other.
        let _ = Box::pin(super::session_worker::journal_event(
            db,
            bus,
            owner,
            session_id,
            spawn_epoch,
            CodeEvent::ApprovalResolved {
                approval_id: approval.id,
                decision: ApprovalDecisionKind::Abandoned,
            },
        ))
        .await;
    }
    // Nothing is waiting on the user any more. Mirrors the decision path:
    // the turn boundary overwrites this with its own verdict a moment later.
    if pending(db, owner, session_id).await.is_empty() {
        let _ = super::attention::apply_attention(
            db,
            bus,
            owner,
            session_id,
            Attention::working(AttentionSource::Lifecycle),
            false,
        )
        .await;
    }
}
