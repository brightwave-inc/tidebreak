use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{CallId, SessionId};
use crate::model::{
    ToolCallExecution, ToolCallRecord, ToolCallResolution, ToolCallStatus, TurnRunStatus,
};
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

const CLIENT_EXECUTOR_UNAVAILABLE_RESULT: &str =
    "The client executor became unavailable before the operation completed.";
const CLIENT_EXECUTOR_LEASE_EXPIRED_CODE: &str = "client_executor_lease_expired";
const CLIENT_EXECUTOR_LEASE_EXPIRED_DETAIL: &str = "The client execution lease expired.";

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
        raw_arguments: Set(call.raw_arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        result_preview: Set(None),
        provider_replay: Set(serialize_provider_replay(call.provider_replay.as_ref())?),
        error_code: Set(None),
        error_detail: Set(None),
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
        raw_arguments: Set(call.raw_arguments.clone()),
        execution: Set(call.execution.as_str().into()),
        status: Set(ToolCallStatus::Pending.as_str().into()),
        result: Set(None),
        result_preview: Set(None),
        provider_replay: Set(serialize_provider_replay(call.provider_replay.as_ref())?),
        error_code: Set(None),
        error_detail: Set(None),
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

// The caller holds the chat write lock shared by cancellation. Check the
// parent before granting execution; a pending call remains reconcilable while
// its cancelling turn waits for the native operation to stop.
async fn client_turn_allows_execution_on(
    transaction: &DatabaseTransaction,
    call: &entities::tool_call::Model,
) -> Result<bool> {
    let Some(turn) = entities::turn::Entity::find_by_id(call.turn_id)
        .one(transaction)
        .await
        .map_err(store_err)?
    else {
        // Legacy client calls can predate durable turns. Browser tools always
        // require the authoritative turn that accepted their native work.
        return Ok(!crate::is_browser_tool(&call.name));
    };
    if turn.session_id != call.chat_id {
        return Ok(false);
    }
    let status = super::turn::turn_run_from_model(turn)?.status;
    Ok(!status.is_terminal()
        && !matches!(
            status,
            TurnRunStatus::Cancelling | TurnRunStatus::CancellingClient
        ))
}

pub(in crate::db) async fn claim_client_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: SessionId,
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
        || existing.name == crate::EXIT_PLAN_MODE_TOOL
        || existing.status != ToolCallStatus::Pending.as_str()
        || !client_turn_allows_execution_on(&transaction, &existing).await?
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
    chat_id: SessionId,
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
        || !client_turn_allows_execution_on(&transaction, &existing).await?
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
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_claimed_server_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: SessionId,
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
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn abandon_inherited_server_tool_call(
    store: &DbStore,
    id: CallId,
    chat_id: SessionId,
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
        None,
    )
    .await?
    .outcome)
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: SessionId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    rows: Option<&serde_json::Value>,
    images: Option<&[crate::ImageRef]>,
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
        images,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn resolve_expired_client_tool_call_and_append_event(
    store: &DbStore,
    id: CallId,
    chat_id: SessionId,
    lease_token: uuid::Uuid,
    now: DateTime<Utc>,
    resolution: &ToolCallResolution,
    resolved_at: DateTime<Utc>,
    rows: Option<&serde_json::Value>,
    images: Option<&[crate::ImageRef]>,
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
        images,
    )
    .await
}

pub(in crate::db) async fn list_pending_client_tool_calls(
    store: &DbStore,
    chat_id: SessionId,
) -> Result<Vec<ToolCallRecord>> {
    expire_claimed_client_tool_calls(store, chat_id, Utc::now()).await?;
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

async fn expire_claimed_client_tool_calls(
    store: &DbStore,
    chat_id: SessionId,
    now: DateTime<Utc>,
) -> Result<()> {
    let now = canonical_db_timestamp(now)?;
    let expired = entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .filter(entities::tool_call::Column::Execution.eq(ToolCallExecution::Client.as_str()))
        .filter(entities::tool_call::Column::Status.eq(ToolCallStatus::Pending.as_str()))
        .filter(entities::tool_call::Column::ClientLeaseToken.is_not_null())
        .filter(entities::tool_call::Column::ClientLeaseExpiresAt.lte(now))
        .order_by_asc(entities::tool_call::Column::HistoryOrder)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let resolution = ToolCallResolution::Failed {
        result: CLIENT_EXECUTOR_UNAVAILABLE_RESULT.into(),
        error_code: CLIENT_EXECUTOR_LEASE_EXPIRED_CODE.into(),
        error_detail: Some(CLIENT_EXECUTOR_LEASE_EXPIRED_DETAIL.into()),
    };
    for call in expired {
        let Some(lease_token) = call.client_lease_token else {
            continue;
        };
        resolve_tool_call(
            store,
            CallId(call.id),
            ResolutionAuthority::ExpiredClient {
                chat_id,
                lease_token,
                now,
            },
            now,
            &resolution,
            None,
            None,
            None,
        )
        .await?;
        // A heartbeat or an independent resolution may win after the scan and
        // before this call acquires its write lock. The resolution transaction
        // rechecks the lease and terminal identity, so every non-error outcome
        // is a benign race rather than a reason to fail authoritative polling.
    }
    Ok(())
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
    images: Option<&[crate::ImageRef]>,
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
    let resolved_call_id = CallId(existing.id);
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
    // A computer-use capture carries its screenshot by reference, not in
    // `rows`: the desktop published the PNG to the blob store and sent the
    // `ImageRef`, so the projection is a `ScreenCapture` preview that lets the
    // transcript reattach the image (gated on the model's image-input flag).
    // Only the capture tool may carry one — a client cannot grant another tool
    // an image card by naming it. `None` images or a non-capture tool yields no
    // capture preview.
    // The mark count comes from the capture's reported rows (the desktop puts
    // the marks list there), so the card reflects what was actually drawn
    // rather than a placeholder.
    let capture_mark_count = rows
        .and_then(|rows| rows.get("marks"))
        .and_then(serde_json::Value::as_array)
        .map(|marks| u32::try_from(marks.len()).unwrap_or(u32::MAX))
        .unwrap_or(0);
    let capture_preview = images.and_then(|images| {
        if resolved_name == crate::COMPUTER_CAPTURE_SCREEN_TOOL
            && resolution.status() == ToolCallStatus::Completed
        {
            images
                .first()
                .map(|image| crate::ToolResultPreview::ScreenCapture {
                    image: *image,
                    mark_count: capture_mark_count,
                })
        } else {
            None
        }
    });
    let preview = preview.or(capture_preview.as_ref()).or(projected.as_ref());
    active.result_preview = Set(preview.and_then(|preview| serde_json::to_value(preview).ok()));
    active.error_code = Set(error_code);
    active.error_detail = Set(error_detail);
    active.client_lease_expires_at = Set(None);
    active.resolution_turn_lease_token = Set(authority.turn_lease_token());
    active.resolved_at = Set(Some(resolved_at));
    let resolved = active.update(&transaction).await.map_err(store_err)?;
    // A call that ends while its consent card is still open leaves nothing
    // for a decision to reach; the card settles as abandoned with the call.
    super::approval::abandon_pending_for_call_on(&transaction, resolved_call_id, resolved_at)
        .await?;
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
            SessionId(resolved.chat_id),
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
    let executor_lease_expired = matches!(
        resolution,
        ToolCallResolution::Failed { error_code, .. }
            if error_code == CLIENT_EXECUTOR_LEASE_EXPIRED_CODE
    );
    let mut output = crate::ToolOutput {
        content: resolution.result().to_owned(),
        data: None,
        // The renderer reads only this from the output, and it is what
        // separates a finished call from a failed one.
        is_error: resolution.status() != ToolCallStatus::Completed,
        error_category: executor_lease_expired.then_some(crate::ToolErrorCategory::TransportFailed),
        ui_view: None,
        images: Vec::new(),
        image_data: crate::ImageAttachments::new(),
    };
    if executor_lease_expired {
        output = output.with_data(serde_json::json!({
            "failure": "executor_unavailable",
            "reason": "lease_expired",
        }));
    }
    crate::AgentEvent::ToolCallCompleted {
        call_id: CallId(resolved.id),
        output,
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
        chat_id: SessionId,
        turn_id: crate::TurnId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
        inherited: bool,
    },
    LiveClient {
        chat_id: SessionId,
        lease_token: uuid::Uuid,
        now: DateTime<Utc>,
    },
    ExpiredClient {
        chat_id: SessionId,
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

    const fn chat_id(self) -> Option<SessionId> {
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
    // The raw fragment is untrusted stream text kept for debugging: it must
    // stay bounded and store-safe, and it is never parsed on the way back out.
    let raw_valid = call.raw_arguments.as_deref().is_none_or(|fragment| {
        !fragment.is_empty()
            && fragment.len() <= ToolCallRecord::MAX_ARGUMENT_BYTES
            && !fragment.contains('\0')
    });
    if call.id.0.is_nil()
        || !labels_valid
        || args_len > ToolCallRecord::MAX_ARGUMENT_BYTES
        || !raw_valid
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
        && model.raw_arguments == call.raw_arguments
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
                && Some(SessionId(model.chat_id)) == authority.chat_id()
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
        chat_id: SessionId(model.chat_id),
        turn_id: crate::id::TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        raw_arguments: model.raw_arguments,
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
        provider_replay: model
            .provider_replay
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                AgentError::Store(format!("invalid stored provider tool replay: {error}"))
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

fn serialize_provider_replay(
    replay: Option<&crate::provider::ProviderToolReplay>,
) -> Result<Option<serde_json::Value>> {
    replay
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| AgentError::Store(format!("invalid provider tool replay: {error}")))
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
