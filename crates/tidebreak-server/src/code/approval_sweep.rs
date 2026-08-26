//! Reconcile approvals whose tool call resolved before anyone decided.
//!
//! An approval is parked on the engine's `tool_use_id`, and the same id comes
//! back on [`tidebreak_core::CodeEvent::ToolCompleted`]. When a completion arrives for a call
//! that still has a pending approval, nobody's decision can reach the engine
//! any more: the engine timed the call out, denied it itself, or ran it under
//! a rule of its own. Leaving the row `Pending` would list a request that can
//! never be acted on, and would let a later `code approve` report success for
//! a command the engine already failed.
//!
//! So the row moves to [`CodeApprovalState::Abandoned`] and the session
//! journals an `ApprovalResolved` carrying
//! [`tidebreak_core::ApprovalDecisionKind::Abandoned`]. The turn and session boundaries sweep
//! the same way, because a tool call that never reports completion must not
//! leave a row pending forever.

use tidebreak_core::db::code::{
    abandon_pending_approval, abandon_pending_approvals_for_stopped_session, get_session,
    list_approvals, list_turns,
};
use tidebreak_core::{
    CodeApproval, CodeApprovalState, CodeSessionId, CodeTurnStatus, DbStore, OwnerId,
};

use super::bus::CodeEventBus;

/// The engine-native call id an approval row was parked on.
///
/// Written as a sibling of the capped payload, so an oversized request still
/// carries it — the same field [`super::runtime::CodeRuntime::decide_approval`]
/// reads.
fn call_id_of(approval: &CodeApproval) -> Option<&str> {
    approval.native_call_id.as_deref().or_else(|| {
        approval
            .harness_raw
            .get("call_id")
            .and_then(serde_json::Value::as_str)
            .filter(|call_id| !call_id.is_empty())
    })
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
    let doomed: Vec<CodeApproval> = pending_for_epoch(db, owner, session_id, spawn_epoch)
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
    let waiting = pending_for_epoch(db, owner, session_id, spawn_epoch).await;
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

/// Settle every approval whose native waiter disappeared with the process.
pub(crate) async fn abandon_for_restart(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
) {
    let now = chrono::Utc::now();
    let abandoned =
        abandon_pending_approvals_for_stopped_session(db, owner, session_id, spawn_epoch, now)
            .await
            .unwrap_or_default();
    for settlement in abandoned {
        bus.publish(session_id, settlement.event);
    }
    refresh_attention_if_clear(db, bus, owner, session_id).await;
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

async fn pending_for_epoch(
    db: &DbStore,
    owner: &OwnerId,
    session_id: CodeSessionId,
    spawn_epoch: i64,
) -> Vec<CodeApproval> {
    pending(db, owner, session_id)
        .await
        .into_iter()
        .filter(|approval| approval.worker_epoch == Some(spawn_epoch))
        .collect()
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
    for approval in doomed {
        let worker_epoch = approval.worker_epoch.unwrap_or(spawn_epoch);
        match abandon_pending_approval(db, owner, approval.id, session_id, worker_epoch, now).await
        {
            Ok(Some(settlement)) => bus.publish(session_id, settlement.event),
            _ => continue,
        }
    }
    // Nothing is waiting on the user any more. Recompute instead of assuming
    // the session is working: this path also runs at turn, end, and recovery
    // boundaries.
    refresh_attention_if_clear(db, bus, owner, session_id).await;
}

async fn refresh_attention_if_clear(
    db: &DbStore,
    bus: &CodeEventBus,
    owner: &OwnerId,
    session_id: CodeSessionId,
) {
    if !pending(db, owner, session_id).await.is_empty() {
        return;
    }
    if let Ok(Some(session)) = get_session(db, owner, session_id).await {
        if let Ok(next) = super::attention::compute_attention(
            db,
            bus,
            &session,
            super::attention::ComputeOpts::default(),
        )
        .await
        {
            let _ =
                super::attention::apply_attention(db, bus, owner, session_id, next, false).await;
        }
    }
}
