use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::approval::{
    ApprovalDecision, ApprovalRequest, AutoJudgeStatus, GrantScope, StandingGrant, ToolApproval,
    ToolApprovalKind, ToolApprovalStatus,
};
use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, TurnId};
use crate::model::{OwnerId, ToolCallExecution, ToolCallStatus, TurnRunStatus};
use crate::preview::ToolActionPreview;
use crate::storage::{
    DecideToolApprovalOutcome, JournaledToolApprovalOutcome, RequestToolApprovalOutcome,
};
use crate::tool::ApprovalClass;

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock};

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

/// The project a chat is filed under, read inside the caller's transaction so
/// a grant is matched against the same membership the write will see.
async fn chat_project_id<C>(conn: &C, chat_id: ChatId) -> Result<Option<crate::id::ProjectId>>
where
    C: ConnectionTrait,
{
    Ok(entities::chat::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .and_then(|chat| chat.project_id)
        .map(crate::id::ProjectId))
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
        .column(entities::chat::Column::Id)
        .from(entities::chat::Entity)
        .and_where(entities::chat::Column::Owner.eq(owner.as_str()))
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

/// The `approval_class` column spelling for a gated call.
///
/// Existing databases constrain the column to `"sensitive"`, from before
/// Workspace-class calls could park. Every gated class stores that legacy
/// spelling; recovery re-derives the real class from the recovered kind, the
/// same way the folded kind spellings are re-derived from the tool name.
fn stored_approval_class(_class: ApprovalClass) -> &'static str {
    ApprovalClass::Sensitive.as_str()
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
    let event = AgentEvent::ApprovalRequired {
        auto_judging: request.auto_judge,
        call_id: request.call_id,
        tool_name: request.tool_name.clone(),
        class: request.class,
        kind: request.kind,
        grant_scopes: GrantScope::mintable_ladder_for(request.kind, &call.name, &call.arguments),
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
        && call.approval_status.is_some()
    {
        let approval = approval_from_model(&call)?;
        if approval.class != request.class || approval.kind != request.kind {
            transaction.commit().await.map_err(store_err)?;
            return Ok(JournaledToolApprovalOutcome {
                outcome: RequestToolApprovalOutcome::IdentityConflict,
                required_event: None,
            });
        }
        let required_event = match call.approval_event_seq {
            Some(seq) => {
                exact_required_event(
                    &transaction,
                    request,
                    lease_token,
                    event_ordinal,
                    seq,
                    &event,
                )
                .await?
            }
            None => None,
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledToolApprovalOutcome {
            outcome: RequestToolApprovalOutcome::Existing(approval),
            required_event,
        });
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
    if let Some(source_call_id) = matching_standing_grant(
        &transaction,
        request.chat_id,
        chat_project_id(&transaction, request.chat_id).await?,
        &call.name,
        request.kind,
        &call.arguments,
    )
    .await?
    {
        let mut active: entities::tool_call::ActiveModel = call.into();
        active.approval_status = Set(Some(ToolApprovalStatus::Approved.as_str().into()));
        active.approval_class = Set(Some(stored_approval_class(request.class).into()));
        active.approval_kind = Set(Some(request.kind.as_str().into()));
        active.approval_requested_at = Set(Some(requested_at));
        active.approval_decided_at = Set(Some(requested_at));
        active.approval_grant_source_call_id = Set(Some(source_call_id));
        let approval = approval_from_model(&active.update(&transaction).await.map_err(store_err)?)?;
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
    let arguments_for_judge = call.arguments.clone();
    let mut active: entities::tool_call::ActiveModel = call.into();
    active.approval_status = Set(Some(ToolApprovalStatus::Pending.as_str().into()));
    active.approval_class = Set(Some(stored_approval_class(request.class).into()));
    active.approval_kind = Set(Some(request.kind.as_str().into()));
    active.approval_requested_at = Set(Some(requested_at));
    active.approval_event_seq = Set(Some(seq));
    if request.auto_judge && judge_may_own(request, &arguments_for_judge) {
        active.auto_judge_status = Set(Some(AutoJudgeStatus::Judging.as_str().into()));
    }
    let approval = approval_from_model(&active.update(&transaction).await.map_err(store_err)?)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(JournaledToolApprovalOutcome {
        outcome: RequestToolApprovalOutcome::Requested(approval),
        required_event: Some(SequencedEvent { seq, event }),
    })
}

async fn exact_required_event<C>(
    conn: &C,
    request: &ApprovalRequest,
    lease_token: uuid::Uuid,
    event_ordinal: i32,
    seq: i64,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>>
where
    C: sea_orm::ConnectionTrait,
{
    let stored = entities::event::Entity::find_by_id((request.chat_id.0, seq))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("approval event receipt is missing".into()))?;
    let payload = serde_json::from_value::<AgentEvent>(stored.payload)?;
    if stored.turn_id != Some(request.turn_id.0) || stored.terminal || payload != *expected {
        return Err(AgentError::Store(
            "approval event receipt does not match its request".into(),
        ));
    }
    Ok((stored.lease_token == Some(lease_token)
        && stored.attempt_event_ordinal == Some(event_ordinal))
    .then_some(SequencedEvent {
        seq,
        event: payload,
    }))
}

/// Close every unresolved server call when its turn becomes terminal. Approval
/// and tool state move together so no waiter or recovery card can survive the
/// terminal transition.
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
        if call.approval_status.as_deref() == Some(ToolApprovalStatus::Pending.as_str()) {
            let requested_at = call.approval_requested_at.ok_or_else(|| {
                AgentError::Store("pending approval is missing requested_at".into())
            })?;
            active.approval_status = Set(Some(ToolApprovalStatus::Rejected.as_str().into()));
            active.approval_reason = Set(Some("turn ended before approval".into()));
            active.approval_decided_at = Set(Some(resolved_at.max(requested_at)));
        }
        active.update(conn).await.map_err(store_err)?;
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
    let calls = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::TurnId.eq(turn_id.0))
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::Id)
        .all(conn)
        .await
        .map_err(store_err)?;
    for call in calls {
        if !acquire_tool_call_write_lock(conn, CallId(call.id)).await? {
            return Err(AgentError::Store(format!(
                "pending approval {} disappeared during cancellation",
                CallId(call.id)
            )));
        }
        let requested_at = call
            .approval_requested_at
            .ok_or_else(|| AgentError::Store("pending approval is missing requested_at".into()))?;
        let mut active: entities::tool_call::ActiveModel = call.into();
        active.approval_status = Set(Some(ToolApprovalStatus::Rejected.as_str().into()));
        active.approval_reason = Set(Some("turn cancellation revoked approval".into()));
        active.approval_decided_at = Set(Some(now.max(requested_at)));
        active.update(conn).await.map_err(store_err)?;
    }
    Ok(())
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
    if existing.approval_status.is_some() {
        let approval = approval_from_model(&existing)?;
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
    if let Some(source_call_id) = matching_standing_grant(
        &transaction,
        request.chat_id,
        chat_project_id(&transaction, request.chat_id).await?,
        &existing.name,
        request.kind,
        &existing.arguments,
    )
    .await?
    {
        let mut active: entities::tool_call::ActiveModel = existing.into();
        active.approval_status = Set(Some(ToolApprovalStatus::Approved.as_str().into()));
        active.approval_class = Set(Some(stored_approval_class(request.class).into()));
        active.approval_kind = Set(Some(request.kind.as_str().into()));
        active.approval_requested_at = Set(Some(requested_at));
        active.approval_decided_at = Set(Some(requested_at));
        active.approval_grant_source_call_id = Set(Some(source_call_id));
        let approval = approval_from_model(&active.update(&transaction).await.map_err(store_err)?)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(RequestToolApprovalOutcome::Granted(approval));
    }
    let arguments_for_judge = existing.arguments.clone();
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.approval_status = Set(Some(ToolApprovalStatus::Pending.as_str().into()));
    active.approval_class = Set(Some(stored_approval_class(request.class).into()));
    active.approval_kind = Set(Some(request.kind.as_str().into()));
    active.approval_requested_at = Set(Some(requested_at));
    if request.auto_judge && judge_may_own(request, &arguments_for_judge) {
        active.auto_judge_status = Set(Some(AutoJudgeStatus::Judging.as_str().into()));
    }
    let inserted = active.update(&transaction).await.map_err(store_err)?;
    let approval = approval_from_model(&inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(RequestToolApprovalOutcome::Requested(approval))
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
    if !acquire_chat_write_lock(&transaction, chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0 || existing.approval_status.is_none() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let current = approval_from_model(&existing)?;
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
    if existing.status != ToolCallStatus::Pending.as_str()
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
    let judge_owned = existing.auto_judge_status.as_deref() == Some("judging");
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.approval_status = Set(Some(decision.status().as_str().into()));
    active.approval_reason = Set(decision.reason().map(str::to_owned));
    active.approval_decided_at = Set(Some(decided_at));
    active.approval_grant_source_call_id = Set(None);
    // A human decision releases a judge that has not answered yet, so the
    // verdict CAS no-ops and the decision is never relabeled as automatic. A
    // terminal `declined` marker stays: it is history, not ownership.
    if judge_owned {
        active.auto_judge_status = Set(None);
    }
    let decided = active.update(&transaction).await.map_err(store_err)?;
    let approval = approval_from_model(&decided)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecideToolApprovalOutcome::Decided(approval))
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
    if !acquire_chat_write_lock(&transaction, chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0 || existing.approval_status.is_none() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(DecideToolApprovalOutcome::Unavailable);
    }
    let current = approval_from_model(&existing)?;
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
    if existing.status != ToolCallStatus::Pending.as_str()
        || existing.execution != ToolCallExecution::Server.as_str()
        || !grant.covers(
            chat_id,
            chat_project_id(&transaction, chat_id).await?,
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
    insert_standing_grant(&transaction, call_id, grant, decided_at).await?;
    let judge_owned = existing.auto_judge_status.as_deref() == Some("judging");
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.approval_status = Set(Some(ToolApprovalStatus::Approved.as_str().into()));
    active.approval_reason = Set(None);
    active.approval_decided_at = Set(Some(decided_at));
    active.approval_grant_source_call_id = Set(None);
    // Same release as the plain human decision: an unanswered judge loses
    // ownership so its verdict CAS no-ops.
    if judge_owned {
        active.auto_judge_status = Set(None);
    }
    let decided = active.update(&transaction).await.map_err(store_err)?;
    let approval = approval_from_model(&decided)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(DecideToolApprovalOutcome::Decided(approval))
}

async fn insert_standing_grant<C>(
    conn: &C,
    source_call_id: CallId,
    grant: &StandingGrant,
    granted_at: DateTime<Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let scope = serde_json::to_value(grant.scope())
        .map_err(|error| AgentError::Store(format!("invalid standing grant scope: {error}")))?;
    entities::standing_tool_grant::Entity::insert(entities::standing_tool_grant::ActiveModel {
        source_call_id: Set(source_call_id.0),
        chat_id: Set(match grant.level() {
            crate::approval::GrantLevel::Chat { chat_id } => Some(chat_id.0),
            crate::approval::GrantLevel::Project { .. } => None,
        }),
        project_id: Set(match grant.level() {
            crate::approval::GrantLevel::Chat { .. } => None,
            crate::approval::GrantLevel::Project { project_id } => Some(project_id.0),
        }),
        tool_name: Set(grant.tool_name().to_owned()),
        approval_kind: Set(grant.kind().standing_grant_key().into()),
        scope: Set(scope),
        granted_at: Set(granted_at),
    })
    .exec_without_returning(conn)
    .await
    .map_err(store_err)?;
    Ok(())
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
    entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .filter(|row| row.approval_status.is_some())
        .as_ref()
        .map(approval_from_model)
        .transpose()
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
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::ApprovalRequestedAt)
        .order_by_asc(entities::tool_call::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .iter()
        .map(approval_from_model)
        .collect()
}

/// A bounded page of calls the Auto-mode judge currently owns, oldest first.
pub(in crate::db) async fn list_judging(store: &DbStore, limit: u64) -> Result<Vec<ToolApproval>> {
    let rows = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::AutoJudgeStatus.eq(AutoJudgeStatus::Judging.as_str()))
        .filter(
            entities::tool_call::Column::ApprovalStatus.eq(ToolApprovalStatus::Pending.as_str()),
        )
        .order_by_asc(entities::tool_call::Column::ApprovalRequestedAt)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    rows.iter().map(approval_from_model).collect()
}

/// Land the Auto-mode judge's verdict on one parked call.
///
/// Approval is a compare-and-set on the `(pending, judging)` pair, so a human
/// decision that already landed always wins and can never be relabeled as
/// automatic. A decline moves only the marker; the call stays pending for the
/// human card, and the marker never returns to `judging`. `false` means the
/// judge no longer owned the call.
pub(in crate::db) async fn resolve_from_judge(
    store: &DbStore,
    chat_id: ChatId,
    call_id: CallId,
    approved: bool,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await?
        || !acquire_tool_call_write_lock(&transaction, call_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let existing = entities::tool_call::Entity::find_by_id(call_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.approval_status.as_deref() != Some(ToolApprovalStatus::Pending.as_str())
        || existing.auto_judge_status.as_deref() != Some(AutoJudgeStatus::Judging.as_str())
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let requested_at = existing.approval_requested_at;
    let mut active: entities::tool_call::ActiveModel = existing.into();
    if approved {
        let decided_at = super::agent_run::database_now(&transaction)
            .await?
            .max(requested_at.unwrap_or_default());
        active.approval_status = Set(Some(ToolApprovalStatus::Approved.as_str().into()));
        active.approval_decided_at = Set(Some(decided_at));
        active.auto_judge_status = Set(Some(AutoJudgeStatus::Approved.as_str().into()));
    } else {
        active.auto_judge_status = Set(Some(AutoJudgeStatus::Declined.as_str().into()));
    }
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
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

fn approval_from_model(model: &entities::tool_call::Model) -> Result<ToolApproval> {
    let status = match model.approval_status.as_deref() {
        Some("pending") => ToolApprovalStatus::Pending,
        Some("approved") => ToolApprovalStatus::Approved,
        Some("rejected") => ToolApprovalStatus::Rejected,
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval status".into(),
            ))
        }
    };
    let kind = match model.approval_kind.as_deref() {
        Some("search_may_share_query_and_excerpts") if model.name.starts_with("mcp__") => {
            ToolApprovalKind::ExternalMcpMayCallServer
        }
        Some("search_may_share_query_and_excerpts") if model.name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        // The page-fetch kind folds into the search spelling the same way; the
        // missing recovery arm made a restarted card consent to "sharing a
        // query" while fetching a URL, and stored any "always allow" under the
        // search key where no later web_extract call could find it.
        Some("search_may_share_query_and_excerpts") if model.name == "web_extract" => {
            ToolApprovalKind::WebExtractMayFetchUrl
        }
        Some("search_may_share_query_and_excerpts") => {
            ToolApprovalKind::SearchMayShareQueryAndExcerpts
        }
        Some("web_search_may_share_query") if model.name == "web_search" => {
            ToolApprovalKind::WebSearchMayShareQuery
        }
        Some("exec_may_run_networked_command") => ToolApprovalKind::ExecMayRunNetworkedCommand,
        // The stored spelling folds workspace edits into the legacy vocabulary
        // (the column has a closed constraint); the tool name recovers the
        // kind. A workspace tool the name table does not know stays a true
        // `Unsupported`: rejectable-only, never silently approvable.
        Some("unsupported") => match ToolApprovalKind::for_tool_name(&model.name) {
            ToolApprovalKind::WorkspaceMayModifyFiles => ToolApprovalKind::WorkspaceMayModifyFiles,
            ToolApprovalKind::DelegateMayRunBackgroundAgent => {
                ToolApprovalKind::DelegateMayRunBackgroundAgent
            }
            _ => ToolApprovalKind::Unsupported,
        },
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval kind".into(),
            ))
        }
    };
    let class = match model.approval_class.as_deref() {
        // `"sensitive"` is the only stored spelling (the column constraint
        // predates gated Workspace calls); the recovered kind says which
        // class actually parked.
        Some("sensitive") if kind == ToolApprovalKind::WorkspaceMayModifyFiles => {
            ApprovalClass::Workspace
        }
        Some("sensitive") => ApprovalClass::Sensitive,
        _ => {
            return Err(AgentError::Store(
                "invalid durable tool approval class".into(),
            ))
        }
    };
    let auto_judge_status = match model.auto_judge_status.as_deref() {
        None => None,
        Some("judging") => Some(crate::approval::AutoJudgeStatus::Judging),
        Some("approved") => Some(crate::approval::AutoJudgeStatus::Approved),
        Some("declined") => Some(crate::approval::AutoJudgeStatus::Declined),
        _ => {
            return Err(AgentError::Store(
                "invalid durable auto-judge status".into(),
            ))
        }
    };
    let approved_by_standing_grant = model.approval_grant_source_call_id.is_some();
    if approved_by_standing_grant && status != ToolApprovalStatus::Approved {
        return Err(AgentError::Store(
            "non-approved tool call names a standing grant source".into(),
        ));
    }
    Ok(ToolApproval {
        call_id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        tool_name: model.name.clone(),
        class,
        kind,
        // Rebuilt from the arguments the call is durably parked on rather than
        // stored separately, so a recovered card can never describe a different
        // action from the one that will run.
        preview: ToolActionPreview::build(&model.name, &model.arguments),
        action_is_exact: ToolActionPreview::describes_exactly(&model.name, &model.arguments),
        approved_by_standing_grant,
        auto_judge_status,
        status,
        reason: model.approval_reason.clone(),
        requested_at: model
            .approval_requested_at
            .ok_or_else(|| AgentError::Store("approval is missing requested_at".into()))?,
        decided_at: model.approval_decided_at,
    })
}
