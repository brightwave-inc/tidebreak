use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{CallId, ChatId};
use crate::model::{ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus};
use crate::storage::{
    AcceptClaimedToolCallOutcome, AcceptToolCallOutcome, ClaimClientToolCallOutcome,
    ClientToolCallClaim, HeartbeatClientToolCallOutcome, JournaledClientToolCallOutcome,
    ResolveToolCallOutcome, TurnLeaseFence,
};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{
    acquire_chat_write_lock, acquire_tool_call_write_lock, acquire_turn_write_lock,
    next_tool_history_order_on,
};

pub(in crate::db) async fn accept_tool_call(
    store: &DbStore,
    call: &ToolCallRecord,
) -> Result<AcceptToolCallOutcome> {
    validate_accept(call)?;
    let created_at = canonical_db_timestamp(call.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, call.chat_id).await? {
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            call.chat_id
        )));
    }
    if let Some(existing) = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = if immutable_request_matches(&existing, call, created_at) {
            AcceptToolCallOutcome::Existing(tool_call_from_model(existing)?)
        } else {
            AcceptToolCallOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }

    let history_order = next_tool_history_order_on(&transaction, call.chat_id).await?;
    let inserted = entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(call.turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        history_order: Set(history_order),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        result_preview: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        approval_status: Set(None),
        approval_class: Set(None),
        approval_kind: Set(None),
        approval_reason: Set(None),
        approval_requested_at: Set(None),
        approval_decided_at: Set(None),
        approval_event_seq: Set(None),
        approval_grant_source_call_id: Set(None),
        auto_judge_status: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        turn_lease_token: Set(None),
        resolution_turn_lease_token: Set(None),
        created_at: Set(created_at),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = tool_call_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptToolCallOutcome::Accepted(inserted))
}

pub(in crate::db) async fn accept_claimed_tool_call(
    store: &DbStore,
    call: &ToolCallRecord,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
) -> Result<AcceptClaimedToolCallOutcome> {
    validate_accept(call)?;
    if call.execution != ToolCallExecution::Server || lease_token.is_nil() {
        return Err(AgentError::Store(
            "claimed tool acceptance requires a server call and non-nil lease".into(),
        ));
    }
    let created_at = canonical_db_timestamp(call.created_at)?;
    let now = canonical_db_timestamp(now)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, call.chat_id).await?
        || !acquire_turn_write_lock(&transaction, call.turn_id).await?
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptClaimedToolCallOutcome::LeaseLost);
    }
    if let Some(existing) = entities::tool_call::Entity::find_by_id(call.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let outcome = if immutable_request_matches(&existing, call, created_at)
            && existing.turn_lease_token == Some(lease_token)
        {
            AcceptClaimedToolCallOutcome::Existing(tool_call_from_model(existing)?)
        } else {
            AcceptClaimedToolCallOutcome::IdentityConflict
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if super::turn::turn_lease_is_current_on(&transaction, call.turn_id, lease_token, now).await?
        != TurnLeaseFence::Current
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AcceptClaimedToolCallOutcome::LeaseLost);
    }

    let history_order = next_tool_history_order_on(&transaction, call.chat_id).await?;
    let inserted = entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(call.turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        history_order: Set(history_order),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        result_preview: Set(None),
        error_code: Set(None),
        error_detail: Set(None),
        approval_status: Set(None),
        approval_class: Set(None),
        approval_kind: Set(None),
        approval_reason: Set(None),
        approval_requested_at: Set(None),
        approval_decided_at: Set(None),
        approval_event_seq: Set(None),
        approval_grant_source_call_id: Set(None),
        auto_judge_status: Set(None),
        client_executor_id: Set(None),
        client_lease_token: Set(None),
        client_lease_expires_at: Set(None),
        turn_lease_token: Set(Some(lease_token)),
        resolution_turn_lease_token: Set(None),
        created_at: Set(created_at),
        resolved_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = tool_call_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AcceptClaimedToolCallOutcome::Accepted(inserted))
}

pub(in crate::db) async fn claim_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    executor_id: uuid::Uuid,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<ClaimClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if executor_id.is_nil() || lease_token.is_nil() {
        return Err(AgentError::Store(
            "client executor id and lease token must not be nil".into(),
        ));
    }
    if lease_expires_at <= now {
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if existing.chat_id != chat_id.0
        || existing.execution != ToolCallExecution::Client.as_str()
        || existing.name == crate::ASK_USER_QUESTIONS_TOOL
        || existing.status != ToolCallStatus::Pending.as_str()
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }
    if existing.client_executor_id == Some(executor_id)
        && existing.client_lease_token == Some(lease_token)
    {
        let mut active: entities::tool_call::ActiveModel = existing.into();
        active.client_lease_expires_at = Set(Some(lease_expires_at));
        let recovered = active.update(&transaction).await.map_err(store_err)?;
        let claim = client_claim_from_model(recovered)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Existing(claim));
    }
    if existing.client_executor_id.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimClientToolCallOutcome::Unavailable);
    }

    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_executor_id = Set(Some(executor_id));
    active.client_lease_token = Set(Some(lease_token));
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    let claimed = active.update(&transaction).await.map_err(store_err)?;
    let claim = client_claim_from_model(claimed)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ClaimClientToolCallOutcome::Claimed(claim))
}

pub(in crate::db) async fn heartbeat_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<HeartbeatClientToolCallOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    validate_lease(lease_token, now, lease_expires_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    let current_expiry = existing.client_lease_expires_at;
    if existing.chat_id != chat_id.0
        || existing.execution != ToolCallExecution::Client.as_str()
        || existing.status != ToolCallStatus::Pending.as_str()
        || existing.client_lease_token != Some(lease_token)
        || current_expiry.is_none_or(|expiry| expiry <= now || lease_expires_at < expiry)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::LeaseLost);
    }
    if current_expiry == Some(lease_expires_at) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(HeartbeatClientToolCallOutcome::Existing);
    }
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.client_lease_expires_at = Set(Some(lease_expires_at));
    active.update(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(HeartbeatClientToolCallOutcome::Extended)
}

pub(in crate::db) async fn resolve_server_tool_call(
    store: &DbStore,
    id: CallId,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::Server,
        resolved_at,
        resolution,
        None,
        None,
    )
    .await?
    .outcome)
}

pub(in crate::db) async fn resolve_server_tool_call_with_preview(
    store: &DbStore,
    id: CallId,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    preview: Option<&crate::ToolResultPreview>,
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::Server,
        resolved_at,
        resolution,
        preview,
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_claimed_server_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    turn_id: crate::TurnId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    preview: Option<&crate::ToolResultPreview>,
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::ClaimedServer {
            chat_id,
            turn_id,
            lease_token,
            now,
            inherited: false,
        },
        resolved_at,
        resolution,
        preview,
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn abandon_inherited_server_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    turn_id: crate::TurnId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
) -> Result<ResolveToolCallOutcome> {
    Ok(resolve_tool_call(
        store,
        id,
        ResolutionAuthority::ClaimedServer {
            chat_id,
            turn_id,
            lease_token,
            now,
            inherited: true,
        },
        resolved_at,
        resolution,
        None,
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    rows: Option<&serde_json::Value>,
) -> Result<JournaledClientToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::LiveClient {
            chat_id,
            lease_token,
            now,
        },
        resolved_at,
        resolution,
        None,
        rows,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_expired_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: ChatId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    rows: Option<&serde_json::Value>,
) -> Result<JournaledClientToolCallOutcome> {
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "client lease token must not be nil".into(),
        ));
    }
    resolve_tool_call(
        store,
        id,
        ResolutionAuthority::ExpiredClient {
            chat_id,
            lease_token,
            now,
        },
        resolved_at,
        resolution,
        None,
        rows,
    )
    .await
}

pub(in crate::db) async fn list_pending_client_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    let models = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    models.into_iter().map(tool_call_from_model).collect()
}

#[allow(clippy::too_many_arguments)]
async fn resolve_tool_call(
    store: &DbStore,
    id: CallId,
    authority: ResolutionAuthority,
    resolved_at: DateTime<Utc>,
    resolution: &ToolCallResolution,
    preview: Option<&crate::ToolResultPreview>,
    rows: Option<&serde_json::Value>,
) -> Result<JournaledClientToolCallOutcome> {
    validate_resolution(resolution)?;
    let resolved_at = canonical_db_timestamp(resolved_at)?;
    let authority = authority.canonicalized()?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(chat_id) = authority.chat_id() {
        if !acquire_chat_write_lock(&transaction, chat_id).await? {
            transaction.commit().await.map_err(store_err)?;
            return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
        }
    }
    if let Some(turn_id) = authority.turn_id() {
        if !acquire_turn_write_lock(&transaction, turn_id).await? {
            transaction.commit().await.map_err(store_err)?;
            return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
        }
    }
    if !acquire_tool_call_write_lock(&transaction, id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(journaled_call_outcome(ResolveToolCallOutcome::NotFound));
    }
    let existing = entities::tool_call::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .expect("locked tool call exists");
    if resolved_at < existing.created_at {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(
            "tool call cannot resolve before it was created".into(),
        ));
    }
    if existing.status != ToolCallStatus::Pending.as_str() {
        let outcome = if !terminal_authority_matches(&existing, authority) {
            ResolveToolCallOutcome::LeaseLost
        } else if terminal_payload_matches(&existing, resolution) {
            ResolveToolCallOutcome::Existing
        } else {
            ResolveToolCallOutcome::AlreadyTerminal
        };
        let transition = if outcome == ResolveToolCallOutcome::Existing && authority.is_client() {
            super::turn::recover_turn_after_client_resolution_on(&transaction, &existing).await?
        } else {
            None
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(JournaledClientToolCallOutcome {
            outcome,
            turn: transition.as_ref().map(|item| item.turn.clone()),
            terminal_event: transition.and_then(|item| item.terminal_event),
        });
    }

    if let Some((turn_id, lease_token, now)) = authority.turn_lease() {
        if super::turn::turn_lease_is_current_on(&transaction, turn_id, lease_token, now).await?
            != TurnLeaseFence::Current
        {
            transaction.commit().await.map_err(store_err)?;
            return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
        }
    }

    let owns = match authority {
        ResolutionAuthority::Server => {
            existing.execution == ToolCallExecution::Server.as_str()
                && existing.turn_lease_token.is_none()
        }
        ResolutionAuthority::ClaimedServer {
            chat_id,
            turn_id,
            lease_token,
            inherited,
            ..
        } => {
            existing.chat_id == chat_id.0
                && existing.turn_id == turn_id.0
                && existing.execution == ToolCallExecution::Server.as_str()
                && (inherited || existing.turn_lease_token == Some(lease_token))
        }
        ResolutionAuthority::LiveClient {
            chat_id,
            lease_token,
            now,
        } => {
            existing.chat_id == chat_id.0
                && existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry > now)
        }
        ResolutionAuthority::ExpiredClient {
            chat_id,
            lease_token,
            now,
        } => {
            existing.chat_id == chat_id.0
                && existing.execution == ToolCallExecution::Client.as_str()
                && existing.client_lease_token == Some(lease_token)
                && existing
                    .client_lease_expires_at
                    .is_some_and(|expiry| expiry <= now)
        }
    };
    if !owns {
        transaction.commit().await.map_err(store_err)?;
        return Ok(journaled_call_outcome(ResolveToolCallOutcome::LeaseLost));
    }

    let (error_code, error_detail) = resolution_error(resolution);
    let resolved_name = existing.name.clone();
    let approval_status = existing.approval_status.clone();
    let approval_requested_at = existing.approval_requested_at;
    let mut active: entities::tool_call::ActiveModel = existing.into();
    active.status = Set(resolution.status().as_str().into());
    active.result = Set(Some(resolution.result().to_owned()));
    // Serialization of a closed, already-clamped projection cannot fail in
    // practice; a store write is the wrong place to panic if it ever did, and
    // losing the card is the whole cost.
    // A server call hands in a projection it already built. A client call hands
    // in the *rows it reported*, and the projection is built here — against the
    // name on the row rather than anything the executor claimed, so the
    // allowlist decides whether this tool may have a card at all and every row
    // goes through the same clamp a server-side one does.
    let projected = rows.and_then(|rows| {
        crate::ToolResultPreview::build(
            &resolved_name,
            &crate::ToolOutput {
                content: String::new(),
                data: Some(rows.clone()),
                is_error: resolution.status() != ToolCallStatus::Completed,
                error_category: None,
                ui_view: None,
                images: Vec::new(),
                image_data: crate::ImageAttachments::new(),
            },
        )
    });
    let preview = preview.or(projected.as_ref());
    active.result_preview = Set(preview.and_then(|preview| serde_json::to_value(preview).ok()));
    active.error_code = Set(error_code);
    active.error_detail = Set(error_detail);
    active.client_lease_expires_at = Set(None);
    active.resolution_turn_lease_token = Set(authority.turn_lease_token());
    active.resolved_at = Set(Some(resolved_at));
    if approval_status.as_deref() == Some(crate::ToolApprovalStatus::Pending.as_str()) {
        let requested_at = approval_requested_at
            .ok_or_else(|| AgentError::Store("pending approval is missing requested_at".into()))?;
        active.approval_status = Set(Some(crate::ToolApprovalStatus::Rejected.as_str().into()));
        active.approval_reason = Set(Some("tool ended before approval".into()));
        active.approval_decided_at = Set(Some(resolved_at.max(requested_at)));
    }
    let resolved = active.update(&transaction).await.map_err(store_err)?;
    // A client call is executed and resolved outside the agent loop, so nothing
    // else ever announces that it finished: the loop reads the result straight
    // into the model transcript on resume and never revisits the call, and the
    // only event this transition journaled was a cancellation. The renderer
    // showed the row running from `ToolCallStarted` until the chat was reopened
    // and the terminal transcript finally settled it.
    //
    // Journaled here, in the transaction that makes the row terminal, so the
    // event cannot disagree with the row it describes. An exact retry returns
    // above as `Existing` without reaching this, so the call announces itself
    // once.
    // Journaled chat-scoped rather than against the turn: a non-terminal
    // turn-scoped event has to name the attempt lease that produced it, and a
    // client call resolves precisely while the turn is parked with no lease.
    // The attempt that started the call is over and the one that resumes is a
    // different attempt, so there is no attempt this belongs to. Readers stream
    // by chat and sequence, which is all this event needs.
    if authority.is_client() {
        let event = client_completion_event(&resolved, resolution, preview);
        super::conversation::append_event_on(
            &transaction,
            ChatId(resolved.chat_id),
            None,
            None,
            None,
            None,
            &event,
        )
        .await?;
    }
    let transition = if authority.is_client() {
        super::turn::advance_turn_after_client_resolution_on(&transaction, &resolved, resolved_at)
            .await?
    } else {
        None
    };
    transaction.commit().await.map_err(store_err)?;
    Ok(JournaledClientToolCallOutcome {
        outcome: ResolveToolCallOutcome::Resolved,
        turn: transition.as_ref().map(|item| item.turn.clone()),
        terminal_event: transition.and_then(|item| item.terminal_event),
    })
}

/// The completion a client-executed call announces for itself.
///
/// Rebuilt from the row rather than carried in from the caller so it can only
/// ever describe what was actually committed. The action is projected from the
/// call's own stored arguments, the same way history rebuilds it, so a client
/// card names its action identically live and after a reload.
fn client_completion_event(
    resolved: &entities::tool_call::Model,
    resolution: &ToolCallResolution,
    preview: Option<&crate::ToolResultPreview>,
) -> crate::AgentEvent {
    crate::AgentEvent::ToolCallCompleted {
        call_id: CallId(resolved.id),
        output: crate::ToolOutput {
            content: resolution.result().to_owned(),
            data: None,
            // The renderer reads only this from the output, and it is what
            // separates a finished call from a failed one.
            is_error: resolution.status() != ToolCallStatus::Completed,
            // Already recorded on the row; re-deriving one here would be a
            // guess about a category the resolution never named.
            error_category: None,
            ui_view: None,
            images: Vec::new(),
            image_data: crate::ImageAttachments::new(),
        },
        action: crate::ToolActionPreview::build(&resolved.name, &resolved.arguments),
        result: preview.cloned(),
    }
}

fn journaled_call_outcome(outcome: ResolveToolCallOutcome) -> JournaledClientToolCallOutcome {
    JournaledClientToolCallOutcome {
        outcome,
        turn: None,
        terminal_event: None,
    }
}

#[derive(Clone, Copy)]
enum ResolutionAuthority {
    Server,
    ClaimedServer {
        chat_id: ChatId,
        turn_id: crate::TurnId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
        inherited: bool,
    },
    LiveClient {
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
    ExpiredClient {
        chat_id: ChatId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
}

impl ResolutionAuthority {
    fn canonicalized(self) -> Result<Self> {
        Ok(match self {
            Self::Server => Self::Server,
            Self::ClaimedServer {
                chat_id,
                turn_id,
                lease_token,
                now,
                inherited,
            } => Self::ClaimedServer {
                chat_id,
                turn_id,
                lease_token,
                now: canonical_db_timestamp(now)?,
                inherited,
            },
            Self::LiveClient {
                chat_id,
                lease_token,
                now,
            } => Self::LiveClient {
                chat_id,
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
            Self::ExpiredClient {
                chat_id,
                lease_token,
                now,
            } => Self::ExpiredClient {
                chat_id,
                lease_token,
                now: canonical_db_timestamp(now)?,
            },
        })
    }

    const fn lease_token(self) -> Option<uuid::Uuid> {
        match self {
            Self::Server | Self::ClaimedServer { .. } => None,
            Self::LiveClient { lease_token, .. } | Self::ExpiredClient { lease_token, .. } => {
                Some(lease_token)
            }
        }
    }

    const fn chat_id(self) -> Option<ChatId> {
        match self {
            Self::Server => None,
            Self::ClaimedServer { chat_id, .. }
            | Self::LiveClient { chat_id, .. }
            | Self::ExpiredClient { chat_id, .. } => Some(chat_id),
        }
    }

    const fn turn_id(self) -> Option<crate::TurnId> {
        match self {
            Self::ClaimedServer { turn_id, .. } => Some(turn_id),
            _ => None,
        }
    }

    const fn turn_lease(self) -> Option<(crate::TurnId, uuid::Uuid, DateTime<Utc>)> {
        match self {
            Self::ClaimedServer {
                turn_id,
                lease_token,
                now,
                ..
            } => Some((turn_id, lease_token, now)),
            _ => None,
        }
    }

    const fn turn_lease_token(self) -> Option<uuid::Uuid> {
        match self {
            Self::ClaimedServer { lease_token, .. } => Some(lease_token),
            _ => None,
        }
    }

    const fn is_client(self) -> bool {
        matches!(self, Self::LiveClient { .. } | Self::ExpiredClient { .. })
    }
}

fn validate_accept(call: &ToolCallRecord) -> Result<()> {
    let labels_valid = [call.provider_id.as_str(), call.name.as_str()]
        .into_iter()
        .all(|value| {
            !value.is_empty()
                && value.len() <= ToolCallRecord::MAX_LABEL_LEN
                && !value.contains('\0')
        });
    let args_len = serde_json::to_vec(&call.arguments)
        .map_err(|error| AgentError::Store(format!("serialize tool arguments: {error}")))?
        .len();
    if call.id.0.is_nil()
        || !labels_valid
        || args_len > ToolCallRecord::MAX_ARGUMENT_BYTES
        || !matches!(
            call.execution,
            ToolCallExecution::Server | ToolCallExecution::Client
        )
        || call.status != ToolCallStatus::Pending
        || call.result.is_some()
        || call.error_code.is_some()
        || call.error_detail.is_some()
        || call.client_executor_id.is_some()
        || call.client_lease_expires_at.is_some()
        || call.resolved_at.is_some()
    {
        return Err(AgentError::Store("invalid accepted tool call".into()));
    }
    Ok(())
}

fn validate_lease(
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    lease_expires_at: DateTime<Utc>,
) -> Result<()> {
    if lease_token.is_nil() || lease_expires_at <= now {
        return Err(AgentError::Store("invalid client execution lease".into()));
    }
    Ok(())
}

fn validate_resolution(resolution: &ToolCallResolution) -> Result<()> {
    let (error_code, error_detail) = resolution_error(resolution);
    if resolution.result().len() > ToolCallRecord::MAX_RESULT_BYTES
        || resolution.result().contains('\0')
        || error_code.as_deref().is_some_and(|code| {
            code.is_empty()
                || code.len() > ToolCallRecord::MAX_ERROR_CODE_LEN
                || code.contains('\0')
        })
        || error_detail.as_deref().is_some_and(|detail| {
            detail.is_empty()
                || detail.len() > ToolCallRecord::MAX_ERROR_DETAIL_LEN
                || detail.contains('\0')
        })
    {
        return Err(AgentError::Store("invalid tool call resolution".into()));
    }
    Ok(())
}

fn resolution_error(resolution: &ToolCallResolution) -> (Option<String>, Option<String>) {
    match resolution {
        ToolCallResolution::Failed {
            error_code,
            error_detail,
            ..
        } => (Some(error_code.clone()), error_detail.clone()),
        ToolCallResolution::Completed { .. } | ToolCallResolution::Cancelled { .. } => (None, None),
    }
}

fn immutable_request_matches(
    model: &entities::tool_call::Model,
    call: &ToolCallRecord,
    created_at: DateTime<Utc>,
) -> bool {
    model.chat_id == call.chat_id.0
        && model.turn_id == call.turn_id.0
        && model.provider_id == call.provider_id
        && model.name == call.name
        && model.arguments == call.arguments
        && model.execution == call.execution.as_str()
        && model.created_at == created_at
}

fn terminal_authority_matches(
    model: &entities::tool_call::Model,
    authority: ResolutionAuthority,
) -> bool {
    match authority {
        ResolutionAuthority::Server => {
            model.execution == ToolCallExecution::Server.as_str()
                && model.client_lease_token.is_none()
                && model.turn_lease_token.is_none()
                && model.resolution_turn_lease_token.is_none()
        }
        ResolutionAuthority::ClaimedServer {
            chat_id,
            turn_id,
            lease_token,
            ..
        } => {
            model.execution == ToolCallExecution::Server.as_str()
                && model.chat_id == chat_id.0
                && model.turn_id == turn_id.0
                && model.resolution_turn_lease_token == Some(lease_token)
        }
        ResolutionAuthority::LiveClient { .. } | ResolutionAuthority::ExpiredClient { .. } => {
            model.execution == ToolCallExecution::Client.as_str()
                && Some(ChatId(model.chat_id)) == authority.chat_id()
                && model.client_lease_token == authority.lease_token()
        }
    }
}

fn terminal_payload_matches(
    model: &entities::tool_call::Model,
    resolution: &ToolCallResolution,
) -> bool {
    let (error_code, error_detail) = resolution_error(resolution);
    model.status == resolution.status().as_str()
        && model.result.as_deref() == Some(resolution.result())
        && model.error_code == error_code
        && model.error_detail == error_detail
}

pub(in crate::db) fn tool_call_from_model(
    model: entities::tool_call::Model,
) -> Result<ToolCallRecord> {
    Ok(ToolCallRecord {
        id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: crate::id::TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        execution: execution_from_db(&model.execution)?,
        status: status_from_db(&model.status)?,
        result: model.result,
        result_preview: model
            .result_preview
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                AgentError::Store(format!("invalid stored tool result preview: {error}"))
            })?,
        error_code: model.error_code,
        error_detail: model.error_detail,
        client_executor_id: model.client_executor_id,
        client_lease_expires_at: model.client_lease_expires_at,
        created_at: model.created_at,
        resolved_at: model.resolved_at,
    })
}

fn client_claim_from_model(model: entities::tool_call::Model) -> Result<ClientToolCallClaim> {
    let lease_token = model.client_lease_token.ok_or_else(|| {
        AgentError::Store("claimed client tool call is missing its lease token".into())
    })?;
    Ok(ClientToolCallClaim {
        call: tool_call_from_model(model)?,
        lease_token,
    })
}

fn execution_from_db(value: &str) -> Result<ToolCallExecution> {
    match value {
        "server" => Ok(ToolCallExecution::Server),
        "client" => Ok(ToolCallExecution::Client),
        "orchestration" => Ok(ToolCallExecution::Orchestration),
        _ => Err(AgentError::Store(format!("invalid tool execution {value}"))),
    }
}

fn status_from_db(value: &str) -> Result<ToolCallStatus> {
    match value {
        "pending" => Ok(ToolCallStatus::Pending),
        "completed" => Ok(ToolCallStatus::Completed),
        "failed" => Ok(ToolCallStatus::Failed),
        "cancelled" => Ok(ToolCallStatus::Cancelled),
        _ => Err(AgentError::Store(format!("invalid tool status {value}"))),
    }
}
