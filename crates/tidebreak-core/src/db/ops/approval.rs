use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::approval::{
    ApprovalDecision, ApprovalRequest, AutoJudgeStatus, GrantScope, InternalToolApprovalRequest,
    StandingGrant, ToolApproval, ToolApprovalKind, ToolApprovalStatus,
};
use crate::code::{
    ApprovalDecisionKind, CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState,
    CodeSessionId, CodeTurnId,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, TurnId};
use crate::model::{OwnerId, ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::preview::ToolActionPreview;
use crate::storage::{
    DecideToolApprovalOutcome, JournaledToolApprovalOutcome, JudgeVerdictOutcome,
    RequestToolApprovalOutcome,
};
use crate::tool::ApprovalClass;

use super::super::{entities, store_err, DbStore};
use super::code::approval::{
    approval_from_row, find_approval_row_on, insert_approval_on, set_auto_judge_status_on,
    settle_approval_on_locked, ApprovalClaim, ApprovalSettlement,
};
use super::conversation::internal_sessions;
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

/// The reason a consent card reports when its turn ended before anyone
/// decided it. The row is `abandoned`; the chat surface reads a rejection.
const ABANDONED_REASON: &str = "turn ended before approval";

/// The reason a consent card reports when the turn's cancellation revoked it.
const CANCELLED_REASON: &str = "turn cancellation revoked approval";

/// Return a durable grant that authorizes this exact canonical call.
///
/// Stored grants are untrusted input on recovery: a malformed row or one whose
/// frozen kind no longer matches the tool is ignored, never widened into
/// consent. The caller holds the chat write lock, so this read and the
/// subsequent approval transition cannot race a grant write for the same chat.
async fn matching_standing_grant<C>(
    conn: &C,
    chat_id: ChatId,
    project_id: Option<crate::id::ProjectId>,
    tool_name: &str,
    kind: ToolApprovalKind,
    arguments: &serde_json::Value,
) -> Result<Option<uuid::Uuid>>
where
    C: ConnectionTrait,
{
    if !kind.is_standing_grantable() {
        return Ok(None);
    }
    // Either level can authorize this call, so both are read and the decision
    // is made per grant by `covers`. A projectless chat can only ever be
    // covered by its own grants.
    let mut reachable =
        sea_orm::Condition::any().add(entities::standing_tool_grant::Column::ChatId.eq(chat_id.0));
    if let Some(project_id) = project_id {
        reachable =
            reachable.add(entities::standing_tool_grant::Column::ProjectId.eq(project_id.0));
    }
    let grants = entities::standing_tool_grant::Entity::find()
        .filter(reachable)
        .filter(entities::standing_tool_grant::Column::ToolName.eq(tool_name))
        .filter(entities::standing_tool_grant::Column::ApprovalKind.eq(kind.standing_grant_key()))
        .order_by_desc(entities::standing_tool_grant::Column::GrantedAt)
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in grants {
        let source_call_id = row.source_call_id;
        let Some(stored_kind) = ToolApprovalKind::from_standing_grant_key(&row.approval_kind)
        else {
            continue;
        };
        let Ok(scope) = serde_json::from_value::<GrantScope>(row.scope) else {
            continue;
        };
        let Some(level) = grant_level_from_row(row.chat_id, row.project_id) else {
            continue;
        };
        let Some(grant) =
            StandingGrant::scoped(level, row.tool_name, stored_kind, scope, row.granted_at)
        else {
            continue;
        };
        if grant.covers(chat_id, project_id, tool_name, kind, arguments) {
            return Ok(Some(source_call_id));
        }
    }
    Ok(None)
}

/// The conversation row an approval hangs off, read inside the caller's
/// transaction: its owner (every code-side row carries one), the worker
/// epoch a card is stamped with, and the project a grant is matched against.
pub(in crate::db::ops) async fn session_row<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<entities::code_session::Model>
where
    C: ConnectionTrait,
{
    entities::code_session::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("chat {chat_id} does not exist")))
}

/// Recover the level a stored grant was written at.
///
/// A row that names neither a chat nor a project — or both — is not a level
/// this build can honor, so it is skipped rather than guessed at, on the same
/// terms as an unparseable scope.
fn grant_level_from_row(
    chat_id: Option<uuid::Uuid>,
    project_id: Option<uuid::Uuid>,
) -> Option<crate::approval::GrantLevel> {
    match (chat_id, project_id) {
        (Some(chat_id), None) => Some(crate::approval::GrantLevel::Chat {
            chat_id: ChatId(chat_id),
        }),
        (None, Some(project_id)) => Some(crate::approval::GrantLevel::Project {
            project_id: crate::id::ProjectId(project_id),
        }),
        _ => None,
    }
}

/// The grants `owner` may see and withdraw: those whose level points at one
/// of the owner's own chats or projects.
///
/// Ownership is derived from the level rather than stored on the grant row: a
/// grant's chat or project carries an invariant owner, and the foreign keys
/// delete the grant with its parent, so the derivation cannot dangle or drift.
fn owned_grants_condition(owner: &OwnerId) -> sea_orm::Condition {
    let owned_chats = sea_orm::sea_query::Query::select()
        .column(entities::code_session::Column::Id)
        .from(entities::code_session::Entity)
        .and_where(entities::code_session::Column::Owner.eq(owner.as_str()))
        .cond_where(internal_sessions())
        .to_owned();
    let owned_projects = sea_orm::sea_query::Query::select()
        .column(entities::project::Column::Id)
        .from(entities::project::Entity)
        .and_where(entities::project::Column::Owner.eq(owner.as_str()))
        .to_owned();
    sea_orm::Condition::any()
        .add(entities::standing_tool_grant::Column::ChatId.in_subquery(owned_chats))
        .add(entities::standing_tool_grant::Column::ProjectId.in_subquery(owned_projects))
}

/// Every durable standing grant, newest first, hydrated on the same terms as
/// [`matching_standing_grant`]: a row whose kind, scope, or grantability no
/// longer parses is skipped rather than surfaced or widened. With an `owner`,
/// only grants reachable through that owner's chats and projects are listed.
pub(in crate::db) async fn list_standing_grants(
    store: &DbStore,
    owner: Option<&OwnerId>,
) -> Result<Vec<crate::approval::StandingGrantRecord>> {
    let mut query = entities::standing_tool_grant::Entity::find();
    if let Some(owner) = owner {
        query = query.filter(owned_grants_condition(owner));
    }
    let rows = query
        .order_by_desc(entities::standing_tool_grant::Column::GrantedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let stored_kind = ToolApprovalKind::from_standing_grant_key(&row.approval_kind)?;
            let scope = serde_json::from_value::<GrantScope>(row.scope).ok()?;
            let level = grant_level_from_row(row.chat_id, row.project_id)?;
            let grant =
                StandingGrant::scoped(level, row.tool_name, stored_kind, scope, row.granted_at)?;
            Some(crate::approval::StandingGrantRecord {
                source_call_id: CallId(row.source_call_id),
                grant,
            })
        })
        .collect())
}

/// Delete one standing grant by its source approval. Idempotent: deleting a
/// grant that is already gone reports `false` rather than erroring. With an
/// `owner`, another owner's grant is left standing and reports `false`,
/// indistinguishable from absent.
pub(in crate::db) async fn revoke_standing_grant(
    store: &DbStore,
    source_call_id: CallId,
    owner: Option<&OwnerId>,
) -> Result<bool> {
    let mut delete = entities::standing_tool_grant::Entity::delete_many()
        .filter(entities::standing_tool_grant::Column::SourceCallId.eq(source_call_id.0));
    if let Some(owner) = owner {
        delete = delete.filter(owned_grants_condition(owner));
    }
    let result = delete.exec(&store.conn).await.map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Whether the judge may own this call, decided on the arguments the call is
/// actually parked on rather than on what the caller believed about them.
///
/// A request that does not qualify simply loses its judge and becomes an
/// ordinary card. Refusing the whole registration would fail a turn over an
/// optimization, and the thing worth preventing — an ineligible command
/// reaching the model — is prevented either way.
fn judge_may_own(request: &ApprovalRequest, arguments: &serde_json::Value) -> bool {
    crate::approval::is_auto_judge_candidate(request.kind, &request.tool_name, arguments)
}

/// The approval row the internal engine mints for one consent card: the
/// row's id is the call id, the kind is the exact preview and the grant
/// ladder, and the raw payload is the engine's own request. A call a
/// standing grant covers mints the row already approved, naming the grant.
fn tool_use_approval(
    session: &entities::code_session::Model,
    call: &entities::tool_call::Model,
    request: &ApprovalRequest,
    grant_scopes: Vec<GrantScope>,
    requested_at: DateTime<Utc>,
    auto_judge_status: Option<AutoJudgeStatus>,
    granted_by: Option<CallId>,
) -> Result<CodeApproval> {
    // The model's own narration never reaches a consent card (decision
    // 0018): the card renders the literal action, and a call that could
    // describe itself to a decision could describe itself favourably.
    let kind = match ToolActionPreview::build(&call.name, &call.arguments) {
        Some(preview) => CodeApprovalKind::ToolUse {
            preview: preview.without_summary(),
            offered_grants: grant_scopes,
        },
        None => CodeApprovalKind::Other {
            summary: crate::chat_journal::bounded(&call.name, crate::code::MAX_TOOL_SUMMARY_CHARS),
        },
    };
    Ok(CodeApproval {
        id: CodeApprovalId(call.id),
        session_id: CodeSessionId(session.id),
        turn_id: CodeTurnId(call.turn_id),
        kind,
        harness_raw: InternalToolApprovalRequest {
            tool_name: call.name.clone(),
            kind: request.kind,
            granted_by,
        }
        .to_raw()?,
        native_call_id: Some(CallId(call.id).to_string()),
        server_capability: None,
        request_sha256: None,
        worker_epoch: Some(session.spawn_epoch),
        decision_claim: None,
        claimed_at: None,
        state: if granted_by.is_some() {
            CodeApprovalState::Approved
        } else {
            CodeApprovalState::Pending
        },
        feedback: None,
        requested_at,
        decided_at: granted_by.map(|_| requested_at),
        auto_judge_status,
    })
}

/// Mint the row a standing grant decided, already approved, and read it
/// back as the agent loop sees it. No journal row: the reader was never
/// asked, and the journal records only what they were.
async fn grant_approval_on<C>(
    conn: &C,
    session: &entities::code_session::Model,
    call: &entities::tool_call::Model,
    request: &ApprovalRequest,
    requested_at: DateTime<Utc>,
    source_call_id: uuid::Uuid,
) -> Result<ToolApproval>
where
    C: ConnectionTrait,
{
    let approval = tool_use_approval(
        session,
        call,
        request,
        Vec::new(),
        requested_at,
        None,
        Some(CallId(source_call_id)),
    )?;
    let owner = OwnerId::new(&session.owner)?;
    insert_approval_on(conn, &owner, &approval).await?;
    let row = find_approval_row_on(conn, approval.id)
        .await?
        .ok_or_else(|| AgentError::Store("inserted approval disappeared".into()))?;
    tool_approval_from_rows(&row, call)
}

pub(in crate::db) async fn request_and_append_event(
    store: &DbStore,
    request: &ApprovalRequest,
    lease_token: uuid::Uuid,
    event_ordinal: i32,
    _requested_at: DateTime<Utc>,
) -> Result<JournaledToolApprovalOutcome> {
    validate_request(request)?;
    if lease_token.is_nil() || !(1..i32::MAX).contains(&event_ordinal) {
        return Err(AgentError::Store(
            "approval journal identity must be valid".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_turn_write_lock(&transaction, request.turn_id).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::Unavailable,
            required_event: None,
        });
    }
    // Lease and request ordering use the database's statement-time clock so
    // host clock skew cannot reject a valid claimed operation.
    let database_now = super::agent_run::database_now(&transaction).await?;
    let call = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let grant_scopes = GrantScope::mintable_ladder_for(request.kind, &call.name, &call.arguments);
    let event = AgentEvent::ApprovalRequired {
        auto_judging: request.auto_judge,
        call_id: request.call_id,
        tool_name: request.tool_name.clone(),
        class: request.class,
        kind: request.kind,
        grant_scopes: grant_scopes.clone(),
        preview: request.preview.clone(),
    };
    let turn = entities::turn_run::Entity::find_by_id(request.turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked turn exists");
    // Immutable receipt recovery precedes every mutable liveness check. A
    // caller that lost the commit acknowledgement can recover the exact event
    // even after cancellation, terminalization, or lease expiry won.
    if call.chat_id == request.chat_id.0
        && call.turn_id == request.turn_id.0
        && call.name == request.tool_name
        && call.execution == ToolCallExecution::Server.as_str()
    {
        if let Some(row) = find_approval_row_on(&transaction, CodeApprovalId(call.id)).await? {
            let approval = tool_approval_from_rows(&row, &call)?;
            if approval.class != request.class || approval.kind != request.kind {
                transaction.commit().await.map_err(store_err)?;
                return Ok(JournaledToolApprovalOutcome {
                    outcome: RequestToolApprovalOutcome::IdentityConflict,
                    required_event: None,
                });
            }
            let required_event =
                exact_required_event(&transaction, request, lease_token, event_ordinal, &event)
                    .await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(JournaledToolApprovalOutcome {
                outcome: RequestToolApprovalOutcome::Existing(approval),
                required_event,
            });
        }
    }
    let claim = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    let claim_is_live = claim.as_ref().is_some_and(|claim| {
        claim.turn_id == request.turn_id.0
            && turn.attempt_count == claim.attempt_count
            && turn.claim_count == claim.claim_count
            && turn.lease_token == Some(lease_token)
            && turn
                .lease_expires_at
                .is_some_and(|expiry| expiry > database_now)
    });
    if call.chat_id != request.chat_id.0
        || call.turn_id != request.turn_id.0
        || call.name != request.tool_name
        || call.execution != ToolCallExecution::Server.as_str()
        || call.status != ToolCallStatus::Pending.as_str()
        || turn.chat_id != request.chat_id.0
        || turn.status != TurnRunStatus::Running.as_str()
        || !claim_is_live
        || request.kind != ToolApprovalKind::for_call(&request.tool_name, request.class)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::IdentityConflict,
            required_event: None,
        });
    }
    let requested_at = database_now.max(turn.updated_at).max(call.created_at);
    let session = session_row(&transaction, request.chat_id).await?;
    if let Some(source_call_id) = matching_standing_grant(
        &transaction,
        request.chat_id,
        session.project_id.map(crate::id::ProjectId),
        &call.name,
        request.kind,
        &call.arguments,
    )
    .await?
    {
        let approval = grant_approval_on(
            &transaction,
            &session,
            &call,
            request,
            requested_at,
            source_call_id,
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::Granted(approval),
            required_event: None,
        });
    }
    let seq = super::conversation::append_event_on(
        &transaction,
        request.chat_id,
        Some(request.turn_id),
        Some(lease_token),
        Some(event_ordinal),
        None,
        &event,
    )
    .await?;
    let auto_judge_status = (request.auto_judge && judge_may_own(request, &call.arguments))
        .then_some(AutoJudgeStatus::Judging);
    let approval = tool_use_approval(
        &session,
        &call,
        request,
        grant_scopes,
        requested_at,
        auto_judge_status,
        None,
    )?;
    let owner = OwnerId::new(&session.owner)?;
    insert_approval_on(&transaction, &owner, &approval).await?;
    let row = find_approval_row_on(&transaction, approval.id)
        .await?
        .ok_or_else(|| AgentError::Store("inserted approval disappeared".into()))?;
    let approval = tool_approval_from_rows(&row, &call)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(JournaledToolApprovalOutcome {
        outcome: RequestToolApprovalOutcome::Requested(approval),
        required_event: Some(SequencedEvent { seq, event }),
    })
}

/// The exact `ApprovalRequired` receipt this attempt committed, found by the
/// attempt's lease and ordinal (one row per pair). A receipt another attempt
/// wrote is not this attempt's, and reads as none; a row at this attempt's
/// slot that is not the expected event is a broken receipt.
async fn exact_required_event<C>(
    conn: &C,
    request: &ApprovalRequest,
    lease_token: uuid::Uuid,
    event_ordinal: i32,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>>
where
    C: sea_orm::ConnectionTrait,
{
    let Some(stored) = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::LeaseToken.eq(lease_token))
        .filter(entities::code_event::Column::AttemptEventOrdinal.eq(event_ordinal))
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let payload = crate::chat_journal::decode_chat_event_required(stored.event)?;
    if stored.session_id != request.chat_id.0
        || stored.turn_id != Some(request.turn_id.0)
        || stored.terminal
        || payload != *expected
    {
        return Err(AgentError::Store(
            "approval event receipt does not match its request".into(),
        ));
    }
    Ok(Some(SequencedEvent {
        seq: stored.seq,
        event: payload,
    }))
}

/// Settle one approval row inside the caller's transaction, which holds the
/// chat (session) lock, and append its `ApprovalResolved`.
pub(in crate::db::ops) async fn settle_row_on<C>(
    conn: &C,
    row: &entities::code_approval::Model,
    claim: ApprovalClaim,
    decision: ApprovalDecisionKind,
    decided_at: DateTime<Utc>,
) -> Result<Option<ApprovalSettlement>>
where
    C: ConnectionTrait,
{
    let owner = OwnerId::new(&row.owner)?;
    let worker_epoch = row
        .worker_epoch
        .ok_or_else(|| AgentError::Store(format!("approval {} has no worker epoch", row.id)))?;
    settle_approval_on_locked(
        conn,
        &owner,
        CodeApprovalId(row.id),
        CodeSessionId(row.session_id),
        worker_epoch,
        claim,
        decision,
        decided_at,
    )
    .await
}

/// Close every unresolved server call when its turn becomes terminal, and
/// abandon every card still open on the turn: no waiter or recovery card can
/// survive the terminal transition.
pub(in crate::db::ops) async fn close_pending_for_terminal_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    now: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::TurnId.eq(turn_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Server.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?;
    for call in calls {
        if !acquire_tool_call_write_lock(conn, CallId(call.id)).await? {
            return Err(AgentError::Store(format!(
                "pending tool call {} disappeared during turn terminalization",
                CallId(call.id)
            )));
        }
        let resolved_at = now.max(call.created_at);
        let mut active: entities::tool_call::ActiveModel = call.clone().into();
        active.status = Set(ToolCallStatus::Cancelled.as_str().into());
        active.result = Set(Some("turn ended before tool completion".into()));
        active.resolved_at = Set(Some(resolved_at));
        active.update(conn).await.map_err(store_err)?;
    }
    let rows = pending_rows_for_turn(conn, turn_id).await?;
    for row in rows {
        let decided_at = now.max(row.requested_at);
        settle_row_on(
            conn,
            &row,
            claim_of(&row),
            ApprovalDecisionKind::Abandoned,
            decided_at,
        )
        .await?;
    }
    Ok(())
}

/// Revoke unresolved consent as soon as cancellation is accepted while
/// leaving the live tool call for the worker's ordinary quiescence path.
pub(in crate::db::ops) async fn reject_pending_for_cancelling_turn_on<C>(
    conn: &C,
    turn_id: TurnId,
    now: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let rows = pending_rows_for_turn(conn, turn_id).await?;
    for row in rows {
        if InternalToolApprovalRequest::from_raw(&row.harness_raw).is_none() {
            // A parked continuation closes with its call through the
            // client-wait state machine, not here.
            continue;
        }
        if !acquire_tool_call_write_lock(conn, CallId(row.id)).await? {
            return Err(AgentError::Store(format!(
                "pending approval {} disappeared during cancellation",
                CallId(row.id)
            )));
        }
        let decided_at = now.max(row.requested_at);
        settle_row_on(
            conn,
            &row,
            claim_of(&row),
            ApprovalDecisionKind::Deny {
                feedback: Some(CANCELLED_REASON.into()),
            },
            decided_at,
        )
        .await?;
    }
    Ok(())
}

/// Abandon the card still open on `call_id`, if any, because the call
/// itself ended: a tool that resolved before a decision, a parked
/// continuation cancellation closed.
pub(in crate::db::ops) async fn abandon_pending_for_call_on<C>(
    conn: &C,
    call_id: CallId,
    now: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let Some(row) = find_approval_row_on(conn, CodeApprovalId(call_id.0)).await? else {
        return Ok(());
    };
    if row.state != CodeApprovalState::Pending.as_str() {
        return Ok(());
    }
    let decided_at = now.max(row.requested_at);
    settle_row_on(
        conn,
        &row,
        claim_of(&row),
        ApprovalDecisionKind::Abandoned,
        decided_at,
    )
    .await?;
    Ok(())
}

pub(in crate::db::ops) fn claim_of(row: &entities::code_approval::Model) -> ApprovalClaim {
    match row.decision_claim {
        Some(claim) => ApprovalClaim::Exact(claim),
        None => ApprovalClaim::Unclaimed,
    }
}

async fn pending_rows_for_turn<C>(
    conn: &C,
    turn_id: TurnId,
) -> Result<Vec<entities::code_approval::Model>>
where
    C: ConnectionTrait,
{
    entities::code_approval::Entity::find()
        .filter(entities::code_approval::Column::TurnId.eq(turn_id.0))
        .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
        .order_by_asc(entities::code_approval::Column::RequestedAt)
        .order_by_asc(entities::code_approval::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)
}

pub(in crate::db) async fn request(
    store: &DbStore,
    request: &ApprovalRequest,
    requested_at: DateTime<Utc>,
) -> Result<RequestToolApprovalOutcome> {
    validate_request(request)?;
    let requested_at = canonical_db_timestamp(requested_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, request.call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(request.call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != request.chat_id.0
        || existing.turn_id != request.turn_id.0
        || existing.name != request.tool_name
        || existing.execution != ToolCallExecution::Server.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::IdentityConflict);
    }
    if let Some(row) = find_approval_row_on(&transaction, CodeApprovalId(existing.id)).await? {
        let approval = tool_approval_from_rows(&row, &existing)?;
        transaction.commit().await.map_err(store_err)?;
        return if approval.class == request.class && approval.kind == request.kind {
            Ok(RequestToolApprovalOutcome::Existing(approval))
        } else {
            Ok(RequestToolApprovalOutcome::IdentityConflict)
        };
    }
    if request.kind != ToolApprovalKind::for_call(&request.tool_name, request.class) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::IdentityConflict);
    }
    let session = session_row(&transaction, request.chat_id).await?;
    if let Some(source_call_id) = matching_standing_grant(
        &transaction,
        request.chat_id,
        session.project_id.map(crate::id::ProjectId),
        &existing.name,
        request.kind,
        &existing.arguments,
    )
    .await?
    {
        let approval = grant_approval_on(
            &transaction,
            &session,
            &existing,
            request,
            requested_at,
            source_call_id,
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::Granted(approval));
    }
    let grant_scopes =
        GrantScope::mintable_ladder_for(request.kind, &existing.name, &existing.arguments);
    let auto_judge_status = (request.auto_judge && judge_may_own(request, &existing.arguments))
        .then_some(AutoJudgeStatus::Judging);
    let approval = tool_use_approval(
        &session,
        &existing,
        request,
        grant_scopes,
        requested_at,
        auto_judge_status,
        None,
    )?;
    let owner = OwnerId::new(&session.owner)?;
    insert_approval_on(&transaction, &owner, &approval).await?;
    let row = find_approval_row_on(&transaction, approval.id)
        .await?
        .ok_or_else(|| AgentError::Store("inserted approval disappeared".into()))?;
    let approval = tool_approval_from_rows(&row, &existing)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(RequestToolApprovalOutcome::Requested(approval))
}

/// The row's decision in the code vocabulary.
fn decision_kind(decision: &ApprovalDecision) -> ApprovalDecisionKind {
    match decision {
        ApprovalDecision::Approve => ApprovalDecisionKind::Approve,
        ApprovalDecision::Reject { reason } => ApprovalDecisionKind::Deny {
            feedback: Some(reason.clone()),
        },
    }
}

pub(in crate::db) async fn decide(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    decision: &ApprovalDecision,
    _decided_at: DateTime<Utc>,
) -> Result<DecideToolApprovalOutcome> {
    validate_decision(decision)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some((row, existing, current)) =
        pending_tool_approval(&transaction, chat_id, call_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    };
    if current.status != ToolApprovalStatus::Pending {
        transaction.commit().await.map_err(store_err)?;
        return if current.status == decision.status()
            && current.reason.as_deref() == decision.reason()
        {
            Ok(DecideToolApprovalOutcome::Existing(current))
        } else {
            Ok(DecideToolApprovalOutcome::DecisionConflict)
        };
    }
    if row.decision_claim.is_some()
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.execution != ToolCallExecution::Server.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    // The wall clock can move backwards between request and decision. Preserve
    // the immutable ordering invariant rather than stranding an otherwise
    // valid decision on the table check.
    let decided_at = super::agent_run::database_now(&transaction)
        .await?
        .max(current.requested_at);
    // A human decision releases a judge that has not answered yet, so the
    // verdict CAS no-ops and the decision is never relabeled as automatic. A
    // terminal `declined` marker stays: it is history, not ownership.
    if current.auto_judge_status == Some(AutoJudgeStatus::Judging) {
        set_auto_judge_status_on(&transaction, CodeApprovalId(row.id), None).await?;
    }
    let settlement = settle_row_on(
        &transaction,
        &row,
        ApprovalClaim::Unclaimed,
        decision_kind(decision),
        decided_at,
    )
    .await?
    .ok_or_else(|| AgentError::Store(format!("approval {call_id} could not be settled")))?;
    let approval = decided_tool_approval(&transaction, call_id, &existing).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecideToolApprovalOutcome::Decided {
        approval,
        resolution: Box::new(settlement.event),
    })
}

/// Decide a pending approval and create the selected standing grant under the
/// same chat and call locks. The check that the scope covers this exact
/// canonical call is repeated here, inside the transaction, so a stale route
/// read cannot turn a different call or a completed one-shot decision into
/// standing authority.
pub(in crate::db) async fn decide_with_grant(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    decision: &ApprovalDecision,
    grant: &StandingGrant,
    _decided_at: DateTime<Utc>,
) -> Result<DecideToolApprovalOutcome> {
    if !matches!(decision, ApprovalDecision::Approve) {
        return Err(AgentError::Store(
            "only an approval may create a standing grant".into(),
        ));
    }
    validate_decision(decision)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some((row, existing, current)) =
        pending_tool_approval(&transaction, chat_id, call_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    };
    if current.status != ToolApprovalStatus::Pending {
        let outcome = if current.status == decision.status()
            && current.reason.as_deref() == decision.reason()
            && stored_grant_matches(&transaction, call_id, grant).await?
        {
            DecideToolApprovalOutcome::Existing(current)
        } else {
            // A matching decision after its original transaction committed is
            // not permission to add or replace a grant. It might be a stale
            // UI retry, and accepting it could widen a call already executed.
            DecideToolApprovalOutcome::DecisionConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    let session = session_row(&transaction, chat_id).await?;
    if row.decision_claim.is_some()
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.execution != ToolCallExecution::Server.as_str()
        || !grant.covers(
            chat_id,
            session.project_id.map(crate::id::ProjectId),
            &existing.name,
            current.kind,
            &existing.arguments,
        )
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let decided_at = super::agent_run::database_now(&transaction)
        .await?
        .max(current.requested_at);
    // The settlement below writes the grant beside the row's terminal
    // state; nothing is minted ahead of the decision.
    // Same release as the plain human decision: an unanswered judge loses
    // ownership so its verdict CAS no-ops.
    if current.auto_judge_status == Some(AutoJudgeStatus::Judging) {
        set_auto_judge_status_on(&transaction, CodeApprovalId(row.id), None).await?;
    }
    let settlement = settle_row_on(
        &transaction,
        &row,
        ApprovalClaim::Unclaimed,
        ApprovalDecisionKind::ApprovedWithGrant {
            scope: grant.scope().clone(),
        },
        decided_at,
    )
    .await?
    .ok_or_else(|| AgentError::Store(format!("approval {call_id} could not be settled")))?;
    let approval = decided_tool_approval(&transaction, call_id, &existing).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecideToolApprovalOutcome::Decided {
        approval,
        resolution: Box::new(settlement.event),
    })
}

/// Lock the chat and the call and load the card parked on it. `None` when
/// the call has no card, or the card belongs to another chat.
async fn pending_tool_approval<C>(
    conn: &C,
    chat_id: ChatId,
    call_id: CallId,
) -> Result<
    Option<(
        entities::code_approval::Model,
        entities::tool_call::Model,
        ToolApproval,
    )>,
>
where
    C: ConnectionTrait,
{
    if !acquire_chat_write_lock(conn, chat_id).await?
        || !acquire_tool_call_write_lock(conn, call_id).await?
    {
        return Ok(None);
    }
    let existing = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let Some(row) = find_approval_row_on(conn, CodeApprovalId(call_id.0)).await? else {
        return Ok(None);
    };
    if row.session_id != chat_id.0 || existing.chat_id != chat_id.0 {
        return Ok(None);
    }
    if InternalToolApprovalRequest::from_raw(&row.harness_raw).is_none() {
        return Ok(None);
    }
    let current = tool_approval_from_rows(&row, &existing)?;
    Ok(Some((row, existing, current)))
}

async fn decided_tool_approval<C>(
    conn: &C,
    call_id: CallId,
    call: &entities::tool_call::Model,
) -> Result<ToolApproval>
where
    C: ConnectionTrait,
{
    let row = find_approval_row_on(conn, CodeApprovalId(call_id.0))
        .await?
        .ok_or_else(|| AgentError::Store(format!("decided approval {call_id} disappeared")))?;
    tool_approval_from_rows(&row, call)
}

async fn stored_grant_matches<C>(
    conn: &C,
    source_call_id: CallId,
    grant: &StandingGrant,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let Some(stored) = entities::standing_tool_grant::Entity::find_by_id(source_call_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    let scope = serde_json::to_value(grant.scope())
        .map_err(|error| AgentError::Store(format!("invalid standing grant scope: {error}")))?;
    Ok(
        grant_level_from_row(stored.chat_id, stored.project_id) == Some(grant.level())
            && stored.tool_name == grant.tool_name()
            && stored.approval_kind == grant.kind().standing_grant_key()
            && stored.scope == scope,
    )
}

pub(in crate::db) async fn get(store: &DbStore, call_id: CallId) -> Result<Option<ToolApproval>> {
    let Some(row) = find_approval_row_on(&store.conn, CodeApprovalId(call_id.0)).await? else {
        return Ok(None);
    };
    if InternalToolApprovalRequest::from_raw(&row.harness_raw).is_none() {
        return Ok(None);
    }
    let Some(call) = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    tool_approval_from_rows(&row, &call).map(Some)
}

pub(in crate::db) async fn list_pending(
    store: &DbStore,
    chat_id: ChatId,
    limit: u64,
) -> Result<Vec<ToolApproval>> {
    if limit == 0 || limit > 100 {
        return Err(AgentError::Store(
            "pending approval limit must be between 1 and 100".into(),
        ));
    }
    let rows = entities::code_approval::Entity::find()
        .filter(entities::code_approval::Column::SessionId.eq(chat_id.0))
        .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
        .order_by_asc(entities::code_approval::Column::RequestedAt)
        .order_by_asc(entities::code_approval::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    tool_approvals_from_rows(store, rows, limit).await
}

/// A bounded page of calls the Auto-mode judge currently owns, oldest first.
pub(in crate::db) async fn list_judging(store: &DbStore, limit: u64) -> Result<Vec<ToolApproval>> {
    let rows = entities::code_approval::Entity::find()
        .filter(
            entities::code_approval::Column::AutoJudgeStatus.eq(AutoJudgeStatus::Judging.as_str()),
        )
        .filter(entities::code_approval::Column::State.eq(CodeApprovalState::Pending.as_str()))
        .order_by_asc(entities::code_approval::Column::RequestedAt)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    tool_approvals_from_rows(store, rows, limit).await
}

/// The consent cards among `rows`, joined to the calls they are parked on.
/// A park (questions, a plan) carries no tool request and is skipped.
async fn tool_approvals_from_rows(
    store: &DbStore,
    rows: Vec<entities::code_approval::Model>,
    limit: u64,
) -> Result<Vec<ToolApproval>> {
    let mut approvals = Vec::new();
    for row in rows {
        if approvals.len() as u64 >= limit {
            break;
        }
        if InternalToolApprovalRequest::from_raw(&row.harness_raw).is_none() {
            continue;
        }
        let call = entities::tool_call::Entity::find_by_id(row.id)
            .one(&store.conn)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!(
                    "approval {} is parked on a call that does not exist",
                    row.id
                ))
            })?;
        approvals.push(tool_approval_from_rows(&row, &call)?);
    }
    Ok(approvals)
}

/// Land the Auto-mode judge's verdict on one parked call.
///
/// Approval is a compare-and-set on the `(pending, judging)` pair, so a human
/// decision that already landed always wins and can never be relabeled as
/// automatic. A decline moves only the marker; the call stays pending for the
/// human card, and the marker never returns to `judging`.
pub(in crate::db) async fn resolve_from_judge(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    approved: bool,
) -> Result<JudgeVerdictOutcome> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some((row, existing, current)) =
        pending_tool_approval(&transaction, chat_id, call_id).await?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JudgeVerdictOutcome::NotOwned);
    };
    if existing.status != ToolCallStatus::Pending.as_str()
        || current.status != ToolApprovalStatus::Pending
        || current.auto_judge_status != Some(AutoJudgeStatus::Judging)
        || row.decision_claim.is_some()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(JudgeVerdictOutcome::NotOwned);
    }
    if !approved {
        set_auto_judge_status_on(
            &transaction,
            CodeApprovalId(row.id),
            Some(AutoJudgeStatus::Declined),
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(JudgeVerdictOutcome::Declined);
    }
    let decided_at = super::agent_run::database_now(&transaction)
        .await?
        .max(current.requested_at);
    set_auto_judge_status_on(
        &transaction,
        CodeApprovalId(row.id),
        Some(AutoJudgeStatus::Approved),
    )
    .await?;
    let settlement = settle_row_on(
        &transaction,
        &row,
        ApprovalClaim::Unclaimed,
        ApprovalDecisionKind::Approve,
        decided_at,
    )
    .await?
    .ok_or_else(|| AgentError::Store(format!("approval {call_id} could not be settled")))?;
    transaction.commit().await.map_err(store_err)?;
    Ok(JudgeVerdictOutcome::Approved(Box::new(settlement.event)))
}

fn validate_request(request: &ApprovalRequest) -> Result<()> {
    if request.call_id.0.is_nil()
        || request.chat_id.0.is_nil()
        || request.turn_id.0.is_nil()
        || request.tool_name.is_empty()
        || request.tool_name.len() > crate::model::ToolCallRecord::MAX_LABEL_LEN
        // ReadOnly never parks: only the classes the permission mode can gate
        // may request approval.
        || request.class == ApprovalClass::ReadOnly
        // The judge may only ever own a judgeable kind. Enforced here, below
        // every caller, so no path can put `exec` or MCP in front of the LLM.
        || (request.auto_judge && !request.kind.is_auto_judgeable())
    {
        return Err(AgentError::Store("invalid tool approval request".into()));
    }
    Ok(())
}

fn validate_decision(decision: &ApprovalDecision) -> Result<()> {
    if decision
        .reason()
        .is_some_and(|reason| !ToolApproval::valid_reason(reason))
    {
        return Err(AgentError::Store("invalid tool approval decision".into()));
    }
    Ok(())
}

/// The consent card as the chat surface reads it, from the approval row and
/// the call it is parked on.
///
/// The preview is rebuilt from the arguments the call is durably parked on
/// rather than read from the row, so a recovered card can never describe a
/// different action from the one that will run. The class is a function of
/// the kind, as it always was in storage.
pub(in crate::db) fn tool_approval_from_rows(
    row: &entities::code_approval::Model,
    call: &entities::tool_call::Model,
) -> Result<ToolApproval> {
    let request = InternalToolApprovalRequest::from_raw(&row.harness_raw)
        .ok_or_else(|| AgentError::Store(format!("approval {} is not a consent card", row.id)))?;
    if request.tool_name != call.name {
        return Err(AgentError::Store(format!(
            "approval {} names tool {} but its call is {}",
            row.id, request.tool_name, call.name
        )));
    }
    let approval = approval_from_row(row.clone())?;
    if request.granted_by.is_some() && approval.state != CodeApprovalState::Approved {
        return Err(AgentError::Store(
            "non-approved tool call names a standing grant source".into(),
        ));
    }
    let (status, reason) = match approval.state {
        CodeApprovalState::Pending => (ToolApprovalStatus::Pending, None),
        CodeApprovalState::Approved => (ToolApprovalStatus::Approved, None),
        CodeApprovalState::Denied => (
            ToolApprovalStatus::Rejected,
            Some(
                approval
                    .feedback
                    .clone()
                    .unwrap_or_else(|| ToolApproval::DEFAULT_REJECT_REASON.into()),
            ),
        ),
        CodeApprovalState::Abandoned => {
            (ToolApprovalStatus::Rejected, Some(ABANDONED_REASON.into()))
        }
    };
    let kind = request.kind;
    let class = if kind == ToolApprovalKind::WorkspaceMayModifyFiles {
        ApprovalClass::Workspace
    } else {
        ApprovalClass::Sensitive
    };
    Ok(ToolApproval {
        call_id: CallId(row.id),
        chat_id: ChatId(row.session_id),
        turn_id: TurnId(row.turn_id),
        tool_name: call.name.clone(),
        class,
        kind,
        preview: ToolActionPreview::build(&call.name, &call.arguments),
        action_is_exact: ToolActionPreview::describes_exactly(&call.name, &call.arguments),
        approved_by_standing_grant: request.granted_by.is_some(),
        auto_judge_status: approval.auto_judge_status,
        status,
        reason,
        requested_at: approval.requested_at,
        decided_at: approval.decided_at,
    })
}
