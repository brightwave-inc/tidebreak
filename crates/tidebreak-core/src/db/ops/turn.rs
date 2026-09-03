use chrono::Utc;
use sea_orm::{
    sea_query::ExprTrait, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, AgentErrorInfo, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{AgentRunId, ChatId, DocumentId, MessageId, TurnId};
use crate::image::ImageRef;
use crate::model::{
    user_message_llm_content, AgentRunStatus, TurnAdmissionLease, TurnAdmissionRequest, TurnRun,
    TurnRunStatus,
};
use crate::provider::Usage;
use crate::storage::{
    AcceptTurnOutcome, ClaimScanTerminalEvent, ClaimTurnRunOutcome, ReservedTurnAcceptanceOutcome,
};

use super::super::{entities, store_err, DbStore};
use super::chat_image_publication as chat_image_publication_ops;
use super::message_attachment as message_attachment_ops;
use super::message_document_attachment as message_document_attachment_ops;
use super::{
    acquire_chat_write_lock, acquire_turn_write_lock,
    agent_run::find_foreground_agent_run_on,
    conversation::{
        append_event_on, next_message_seq_on, reserve_message_identity_on,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    },
};

pub(in crate::db) mod admission;
mod client_wait;
mod multi_agent_run_wait;
pub(in crate::db) mod queued;
mod resolution;
mod sandbox_spawn;
pub(in crate::db) mod steer;

pub(in crate::db) use client_wait::{
    advance_turn_after_client_resolution_on, approval_park_call_id, park_turn_for_client_tool_call,
    recover_turn_after_client_resolution_on,
};
#[cfg(test)]
pub(in crate::db) use multi_agent_run_wait::ready_agent_run_wait_set_candidates_sql;
pub(in crate::db) use multi_agent_run_wait::{
    list_ready_agent_run_wait_set_candidates, park_turn_for_agent_run_wait_set,
    resume_turn_for_agent_run_wait_set,
};

pub(in crate::db) use resolution::{
    complete_refused_turn_with_citations_and_append_event, complete_turn,
    complete_turn_and_append_event, complete_turn_with_citations_and_append_event,
    finish_turn_cancellation, finish_turn_cancellation_and_append_event, record_turn_failure,
    record_turn_failure_and_append_event, recover_exact_completed_turn_event,
    request_turn_cancellation, request_turn_cancellation_and_append_event,
};
pub(in crate::db) use sandbox_spawn::{checkpoint_sandbox_spawn, resumed_sandbox_spawn_batch};
pub(in crate::db) use steer::{accept_turn_steer, apply_turn_steer, list_pending_turn_steers};

/// Take a lease on one already-inserted turn so a session worker can drive
/// the leg under a durable claim. Returns whether this claim won.
pub(in crate::db) async fn take_lease_on_turn(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<Option<()>> {
    take_lease_on_turn_inner(store, id, lease_token, now, lease_expires_at, None).await
}

/// Claim one inserted turn and add its missing user transcript row atomically.
pub(in crate::db) async fn take_lease_on_turn_with_input_message(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
    content: &str,
) -> Result<Option<()>> {
    take_lease_on_turn_inner(store, id, lease_token, now, lease_expires_at, Some(content)).await
}

async fn take_lease_on_turn_inner(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
    input: Option<&str>,
) -> Result<Option<()>> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if lease_token.is_nil() || lease_expires_at <= now {
        return Err(AgentError::Store(
            "turn lease requires a non-nil token and an expiry after now".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = entities::code_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let chat_id = ChatId(existing.session_id);
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if !acquire_turn_write_lock(&transaction, id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(existing) = entities::code_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    };
    if existing.lease_token.is_some()
        && existing.status == TurnRunStatus::Running.as_str()
        && existing
            .lease_expires_at
            .is_some_and(|expires_at| expires_at > now)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let next_attempt = std::cmp::Ord::max(existing.attempt_count.saturating_add(1), 1);
    let next_claim = std::cmp::Ord::max(existing.claim_count.saturating_add(1), 1);
    let inserted =
        entities::code_turn_claim::Entity::insert(entities::code_turn_claim::ActiveModel {
            token: Set(lease_token),
            turn_id: Set(id.0),
            owner: Set(existing.owner.clone()),
            attempt_count: Set(next_attempt),
            claim_count: Set(next_claim),
            claimed_at: Set(now),
            lease_expires_at: Set(lease_expires_at),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::new()
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;
    if !matches!(inserted, TryInsertResult::Inserted(1)) {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    let updated = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::Status,
            sea_orm::sea_query::Expr::value(TurnRunStatus::Running.as_str()),
        )
        .col_expr(
            entities::code_turn::Column::AttemptCount,
            sea_orm::sea_query::Expr::value(next_attempt),
        )
        .col_expr(
            entities::code_turn::Column::ClaimCount,
            sea_orm::sea_query::Expr::value(next_claim),
        )
        .col_expr(
            entities::code_turn::Column::LeaseToken,
            sea_orm::sea_query::Expr::value(Some(lease_token)),
        )
        .col_expr(
            entities::code_turn::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::code_turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::code_turn::Column::StartedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .col_expr(
            entities::code_turn::Column::EndedAt,
            sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
        )
        .col_expr(
            entities::code_turn::Column::LastErrorCode,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .col_expr(
            entities::code_turn::Column::LastErrorDetail,
            sea_orm::sea_query::Expr::value(Option::<String>::None),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(None);
    }
    if let Some(content) = input {
        ensure_turn_input_message_on(&transaction, &existing, content).await?;
    }
    transaction.commit().await.map_err(store_err)?;
    // Harness turns have no transcript message. The caller only needs to
    // know the lease stuck; it already holds the `code_turn` row.
    Ok(Some(()))
}

async fn ensure_turn_input_message_on<C>(
    conn: &C,
    existing: &entities::code_turn::Model,
    content: &str,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if existing.input_message_id.is_some() {
        return Ok(());
    }
    let id = TurnId(existing.id);
    let chat_id = ChatId(existing.session_id);
    let now = super::agent_run::database_now(conn).await?;
    let input_message_id = MessageId::new();
    if !reserve_message_identity_on(
        conn,
        input_message_id,
        chat_id,
        id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        return Err(AgentError::Store(format!(
            "turn input message identity {input_message_id} is already reserved"
        )));
    }
    entities::message::ActiveModel {
        id: Set(input_message_id.0),
        chat_id: Set(chat_id.0),
        turn_id: Set(id.0),
        seq: Set(next_message_seq_on(conn, chat_id).await?),
        role: Set("user".into()),
        content: Set(content.into()),
        llm_content: Set(None),
        reasoning: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    let updated = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::InputMessageId,
            sea_orm::sea_query::Expr::value(Some(input_message_id.0)),
        )
        .col_expr(
            entities::code_turn::Column::UserInput,
            sea_orm::sea_query::Expr::value(content.to_owned()),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .filter(entities::code_turn::Column::InputMessageId.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "turn {id} gained an input message from another writer"
        )));
    }
    let images =
        message_attachment_ops::bind_turn_to_message_on(conn, id, input_message_id).await?;
    let llm_content = user_message_llm_content(content, &images, &[], &[], false);
    if llm_content.is_some() {
        entities::message::ActiveModel {
            id: Set(input_message_id.0),
            llm_content: Set(llm_content),
            ..Default::default()
        }
        .update(conn)
        .await
        .map_err(store_err)?;
    }
    Ok(())
}

pub(in crate::db) async fn get_turn(store: &DbStore, id: TurnId) -> Result<Option<TurnRun>> {
    entities::code_turn::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(turn_run_from_model)
        .transpose()
}

pub(in crate::db) async fn list_turns(store: &DbStore, chat_id: ChatId) -> Result<Vec<TurnRun>> {
    entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(chat_id.0))
        .filter(entities::code_turn::Column::InputMessageId.is_not_null())
        .order_by_asc(entities::code_turn::Column::StartedAt)
        .order_by_asc(entities::code_turn::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(turn_run_from_model)
        .collect()
}

pub(in crate::db) async fn recover_exact_terminal_event(
    store: &DbStore,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    expected: &AgentEvent,
) -> Result<Option<SequencedEvent>> {
    let Some(stored) = entities::code_event::Entity::find()
        .filter(entities::code_event::Column::TurnId.eq(turn_id.0))
        .filter(entities::code_event::Column::Terminal.eq(true))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    if stored.lease_token != Some(lease_token) || stored.attempt_event_ordinal != Some(i32::MAX) {
        return Ok(None);
    }
    let Ok(event) = crate::chat_journal::decode_chat_event_required(stored.event) else {
        return Ok(None);
    };
    if event != *expected {
        return Ok(None);
    }
    Ok(Some(SequencedEvent {
        seq: stored.seq,
        event,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn accept_turn(
    store: &DbStore,
    id: TurnId,
    chat_id: ChatId,
    model: &str,
    content: &str,
    images: &[ImageRef],
    documents: &[DocumentId],
    invoked_skills: &[String],
    voice_input_used: bool,
) -> Result<AcceptTurnOutcome> {
    match accept_turn_inner(
        store,
        None,
        id,
        chat_id,
        model,
        content,
        images,
        documents,
        invoked_skills,
        voice_input_used,
    )
    .await?
    {
        ReservedTurnAcceptanceOutcome::Outcome(outcome) => Ok(*outcome),
        ReservedTurnAcceptanceOutcome::LeaseLost => Err(AgentError::Store(format!(
            "turn {id} has an unresolved admission owned by another process"
        ))),
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn accept_reserved_turn(
    store: &DbStore,
    lease: TurnAdmissionLease,
    chat_id: ChatId,
    model: &str,
    content: &str,
    images: &[ImageRef],
    documents: &[DocumentId],
    invoked_skills: &[String],
    voice_input_used: bool,
) -> Result<ReservedTurnAcceptanceOutcome> {
    accept_turn_inner(
        store,
        Some(lease),
        lease.id,
        chat_id,
        model,
        content,
        images,
        documents,
        invoked_skills,
        voice_input_used,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn accept_turn_inner(
    store: &DbStore,
    reservation: Option<TurnAdmissionLease>,
    id: TurnId,
    chat_id: ChatId,
    model: &str,
    content: &str,
    images: &[ImageRef],
    documents: &[DocumentId],
    invoked_skills: &[String],
    voice_input_used: bool,
) -> Result<ReservedTurnAcceptanceOutcome> {
    validate_turn_input(id, model, content, invoked_skills)?;
    message_attachment_ops::validate(images)?;
    message_document_attachment_ops::validate_count(images.len(), documents)?;
    let request = TurnAdmissionRequest {
        id,
        chat_id,
        content: content.into(),
        attachments: images.iter().map(|image| image.blob_id).collect(),
        file_attachments: documents.to_vec(),
        invoked_skills: invoked_skills.to_vec(),
        voice_input_used,
    };
    admission::validate_request(&request)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    let foreground = find_foreground_agent_run_on(&transaction, chat_id)
        .await?
        .ok_or_else(|| AgentError::Store(format!("chat {chat_id} has no foreground agent run")))?;

    let _ = reservation;

    if let Some(existing) = entities::code_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let existing = exact_accepted_turn_on(
            &transaction,
            existing,
            chat_id,
            foreground.id,
            model,
            content,
            images,
            documents,
            invoked_skills,
            voice_input_used,
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ReservedTurnAcceptanceOutcome::Outcome(Box::new(existing)));
    }

    if let Err(error) =
        chat_image_publication_ops::require_exact_on(&transaction, chat_id, images).await
    {
        transaction.rollback().await.map_err(store_err)?;
        return Err(error);
    }

    if foreground.status != AgentRunStatus::Active {
        return Err(AgentError::Store(format!(
            "chat {chat_id} foreground agent run is not active"
        )));
    }

    if let Some(active) = find_active_turn_on(&transaction, chat_id).await? {
        let active = turn_run_from_model(active)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ReservedTurnAcceptanceOutcome::Outcome(Box::new(
            AcceptTurnOutcome::ChatBusy(active),
        )));
    }

    let now = super::agent_run::database_now(&transaction).await?;
    let inserted = match insert_accepted_turn_on(
        &transaction,
        id,
        chat_id,
        foreground.id,
        model,
        content,
        images,
        documents,
        invoked_skills,
        voice_input_used,
        now,
    )
    .await
    {
        Ok(inserted) => inserted,
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(existing) = entities::code_turn::Entity::find_by_id(id.0)
                .one(&store.conn)
                .await
                .map_err(store_err)?
            {
                return exact_accepted_turn_on(
                    &store.conn,
                    existing,
                    chat_id,
                    foreground.id,
                    model,
                    content,
                    images,
                    documents,
                    invoked_skills,
                    voice_input_used,
                )
                .await
                .map(|outcome| ReservedTurnAcceptanceOutcome::Outcome(Box::new(outcome)));
            }
            if let Some(active) = find_active_turn_on(&store.conn, chat_id).await? {
                return Ok(ReservedTurnAcceptanceOutcome::Outcome(Box::new(
                    AcceptTurnOutcome::ChatBusy(turn_run_from_model(active)?),
                )));
            }
            return Err(error);
        }
    };

    transaction.commit().await.map_err(store_err)?;
    Ok(ReservedTurnAcceptanceOutcome::Outcome(Box::new(
        AcceptTurnOutcome::Accepted(turn_run_from_model(inserted)?),
    )))
}

/// Persist the message, attachments, and queued turn beneath a caller-owned
/// transaction and chat lock. Admission ownership is deliberately handled by
/// the caller so ordinary acceptance and FIFO promotion share one creation
/// path without weakening either fence.
#[allow(clippy::too_many_arguments)]
pub(super) async fn insert_accepted_turn_on<C>(
    conn: &C,
    id: TurnId,
    chat_id: ChatId,
    foreground_id: AgentRunId,
    model: &str,
    content: &str,
    images: &[ImageRef],
    documents: &[DocumentId],
    invoked_skills: &[String],
    voice_input_used: bool,
    now: chrono::DateTime<Utc>,
) -> Result<entities::code_turn::Model>
where
    C: ConnectionTrait,
{
    let input_message_id = MessageId::new();
    if !reserve_message_identity_on(
        conn,
        input_message_id,
        chat_id,
        id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        return Err(AgentError::Store(format!(
            "turn input message identity {input_message_id} is already reserved"
        )));
    }
    entities::message::ActiveModel {
        id: Set(input_message_id.0),
        chat_id: Set(chat_id.0),
        turn_id: Set(id.0),
        seq: Set(next_message_seq_on(conn, chat_id).await?),
        role: Set("user".into()),
        content: Set(content.into()),
        llm_content: Set(None),
        reasoning: Set(None),
        turn_lease_token: Set(None),
        created_at: Set(now),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;

    let fingerprint = crate::model::TurnAdmissionRequest {
        id,
        chat_id,
        content: content.into(),
        attachments: images.iter().map(|image| image.blob_id).collect(),
        file_attachments: documents.to_vec(),
        invoked_skills: invoked_skills.to_vec(),
        voice_input_used,
    }
    .fingerprint()
    .to_vec();
    let session = entities::code_session::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("session {chat_id} does not exist")))?;
    let last = entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(chat_id.0))
        .order_by_desc(entities::code_turn::Column::Ordinal)
        .one(conn)
        .await
        .map_err(store_err)?;
    let ordinal = last
        .map_or(Some(1), |row| row.ordinal.checked_add(1))
        .ok_or_else(|| {
            AgentError::Store(format!("turn ordinal exhausted for session {chat_id}"))
        })?;
    let _ = foreground_id;
    entities::code_turn::ActiveModel {
        id: Set(id.0),
        owner: Set(session.owner),
        session_id: Set(chat_id.0),
        ordinal: Set(ordinal),
        status: Set(TurnRunStatus::Queued.as_str().into()),
        model: Set(Some(model.into())),
        fast_mode: Set(false),
        user_input: Set(content.into()),
        user_input_blob_id: Set(None),
        checkpoint_ref: Set(None),
        diffstat: Set(None),
        usage: Set(None),
        narrative: Set(None),
        rewrite: Set(None),
        started_at: Set(now),
        ended_at: Set(None),
        park_ref: Set(None),
        park_wait: Set(None),
        attempt_count: Set(0),
        max_attempts: Set(TurnRun::DEFAULT_MAX_ATTEMPTS),
        claim_count: Set(0),
        model_steps: Set(0),
        input_tokens: Set(0),
        output_tokens: Set(0),
        cache_read_input_tokens: Set(0),
        cache_creation_input_tokens: Set(0),
        available_at: Set(Some(now)),
        lease_token: Set(None),
        lease_expires_at: Set(None),
        last_error_code: Set(None),
        last_error_detail: Set(None),
        steer_revision: Set(0),
        last_steer_applied_at: Set(None),
        invoked_skills: Set(serde_json::json!(invoked_skills)),
        voice_input_used: Set(voice_input_used),
        input_message_id: Set(Some(input_message_id.0)),
        output_message_id: Set(None),
        updated_at: Set(Some(now)),
        fingerprint: Set(Some(fingerprint)),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    message_attachment_ops::insert_on(conn, chat_id, id, input_message_id, images, now).await?;
    let document_context = message_document_attachment_ops::insert_on(
        conn,
        chat_id,
        id,
        input_message_id,
        documents,
        now,
    )
    .await?;
    let llm_content = user_message_llm_content(
        content,
        images,
        &document_context,
        invoked_skills,
        voice_input_used,
    );
    if llm_content.is_some() {
        entities::message::ActiveModel {
            id: Set(input_message_id.0),
            llm_content: Set(llm_content),
            ..Default::default()
        }
        .update(conn)
        .await
        .map_err(store_err)?;
    }
    entities::code_turn::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store(format!("turn {id} vanished after insert")))
}

pub(in crate::db) async fn claim_turn(
    store: &DbStore,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<ClaimTurnRunOutcome> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if lease_token.is_nil() {
        return Err(AgentError::Store("turn lease token must not be nil".into()));
    }
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "turn lease expiry must be after claim time".into(),
        ));
    }

    // Idle fast path. A turn worker draws a fresh token for each poll, and an
    // empty scan has no receipt to persist. Check exact-retry evidence and
    // claimable work without taking SQLite's single writer; the locked scan
    // below remains authoritative when any of them exists. A turn admitted
    // after this hint is picked up by the worker wake or the next bounded poll.
    if !any_turn_claim_work_on(&store.conn, lease_token, now).await? {
        return Ok(ClaimTurnRunOutcome {
            turn: None,
            terminal_event: None,
        });
    }
    loop {
        let transaction = store.conn.begin().await.map_err(store_err)?;
        acquire_turn_claim_write_lock(&transaction).await?;
        if let Some(existing) = entities::code_event::Entity::find()
            .filter(entities::code_event::Column::ScanToken.eq(lease_token))
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let terminal_event = claim_scan_terminal_event_from_model(existing, lease_token)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(terminal_event),
            });
        }
        if let Some(receipt) = entities::code_turn_claim::Entity::find_by_id(lease_token)
            .one(&transaction)
            .await
            .map_err(store_err)?
        {
            let existing = entities::code_turn::Entity::find_by_id(receipt.turn_id)
                .one(&transaction)
                .await
                .map_err(store_err)?
                .filter(|run| {
                    run.status == TurnRunStatus::Running.as_str()
                        && run.attempt_count == receipt.attempt_count
                        && run.claim_count == receipt.claim_count
                        && run.lease_token == Some(lease_token)
                        && run
                            .lease_expires_at
                            .is_some_and(|lease_expires_at| lease_expires_at > now)
                })
                .map(turn_run_from_model)
                .transpose()?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: existing,
                terminal_event: None,
            });
        }
        let due = entities::code_turn::Entity::find()
            .filter(due_turn_candidate_condition(now))
            .order_by_asc(entities::code_turn::Column::AvailableAt)
            .order_by_asc(entities::code_turn::Column::StartedAt)
            .order_by_asc(entities::code_turn::Column::Id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let expired = entities::code_turn::Entity::find()
            .filter(expired_turn_candidate_condition(now))
            .order_by_asc(entities::code_turn::Column::LeaseExpiresAt)
            .order_by_asc(entities::code_turn::Column::StartedAt)
            .order_by_asc(entities::code_turn::Column::Id)
            .one(&transaction)
            .await
            .map_err(store_err)?;
        let candidate = match (due, expired) {
            (Some(due), Some(expired)) => {
                if turn_run_due_order(&due, &expired).is_le() {
                    Some(due)
                } else {
                    Some(expired)
                }
            }
            (candidate @ Some(_), None) | (None, candidate @ Some(_)) => candidate,
            (None, None) => None,
        };
        let Some(candidate) = candidate else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: None,
            });
        };

        if candidate.status == TurnRunStatus::Cancelling.as_str() {
            let chat_id = ChatId(candidate.session_id);
            if !acquire_chat_write_lock(&transaction, chat_id).await? {
                return Err(AgentError::Store(format!(
                    "turn {} references missing chat {chat_id}",
                    TurnId(candidate.id)
                )));
            }
            let cancelled = entities::code_turn::Entity::update_many()
                .col_expr(
                    entities::code_turn::Column::Status,
                    sea_orm::sea_query::Expr::value(TurnRunStatus::Cancelled.as_str()),
                )
                .col_expr(
                    entities::code_turn::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::code_turn::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .col_expr(
                    entities::code_turn::Column::EndedAt,
                    sea_orm::sea_query::Expr::value(Some(now)),
                )
                .col_expr(
                    entities::code_turn::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(now),
                )
                .filter(entities::code_turn::Column::Id.eq(candidate.id))
                .filter(entities::code_turn::Column::Status.eq(TurnRunStatus::Cancelling.as_str()))
                .filter(entities::code_turn::Column::AttemptCount.eq(candidate.attempt_count))
                .filter(entities::code_turn::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::code_turn::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::code_turn::Column::LeaseExpiresAt.lte(now))
                .filter(entities::code_turn::Column::UpdatedAt.eq(candidate.updated_at))
                .filter(entities::code_turn::Column::UpdatedAt.lte(now))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if cancelled.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            steer::reject_pending_turn_steers_on(&transaction, TurnId(candidate.id), now).await?;
            let event = AgentEvent::TurnCancelled {
                usage: usage_from_turn_model(&candidate)?,
            };
            let sequenced_event = append_claim_scan_terminal_event_on(
                &transaction,
                &candidate,
                chat_id,
                lease_token,
                &event,
            )
            .await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(ClaimScanTerminalEvent {
                    chat_id,
                    turn_id: TurnId(candidate.id),
                    event: sequenced_event,
                }),
            });
        }

        let reclaiming = candidate.status == TurnRunStatus::Running.as_str();
        if reclaiming && candidate.attempt_count >= candidate.max_attempts {
            let chat_id = ChatId(candidate.session_id);
            // Final lease exhaustion shares the terminal lock order with
            // explicit completion and failure: sandbox scheduler, chat, turn.
            super::agent_run::acquire_agent_run_claim_lock(&transaction).await?;
            if !acquire_chat_write_lock(&transaction, chat_id).await? {
                return Err(AgentError::Store(format!(
                    "turn {} references missing chat {chat_id}",
                    TurnId(candidate.id)
                )));
            }
            if !acquire_turn_write_lock(&transaction, TurnId(candidate.id)).await? {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            let Some(locked_candidate) = entities::code_turn::Entity::find_by_id(candidate.id)
                .one(&transaction)
                .await
                .map_err(store_err)?
            else {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            };
            if locked_candidate.status != TurnRunStatus::Running.as_str()
                || locked_candidate.session_id != candidate.session_id
                || locked_candidate.attempt_count != candidate.attempt_count
                || locked_candidate.max_attempts != candidate.max_attempts
                || locked_candidate.claim_count != candidate.claim_count
                || locked_candidate.lease_token != candidate.lease_token
                || locked_candidate.lease_expires_at != candidate.lease_expires_at
                || locked_candidate.updated_at != candidate.updated_at
            {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            let candidate = locked_candidate;
            let terminal_now =
                std::cmp::max(now, super::agent_run::database_now(&transaction).await?);
            if candidate
                .lease_expires_at
                .is_none_or(|expires_at| expires_at > terminal_now)
                || candidate
                    .updated_at
                    .is_some_and(|updated_at| updated_at > terminal_now)
            {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            if !super::agent_run::cancel_sandbox_children_for_origin_turn_on(
                &transaction,
                &candidate,
                terminal_now,
                crate::model::AgentRunCancellationReason::ParentTurnFailed,
            )
            .await?
            {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            let failed = entities::code_turn::Entity::update_many()
                .col_expr(
                    entities::code_turn::Column::Status,
                    sea_orm::sea_query::Expr::value(TurnRunStatus::Failed.as_str()),
                )
                .col_expr(
                    entities::code_turn::Column::LeaseToken,
                    sea_orm::sea_query::Expr::value(Option::<uuid::Uuid>::None),
                )
                .col_expr(
                    entities::code_turn::Column::LeaseExpiresAt,
                    sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                )
                .col_expr(
                    entities::code_turn::Column::EndedAt,
                    sea_orm::sea_query::Expr::value(Some(terminal_now)),
                )
                .col_expr(
                    entities::code_turn::Column::LastErrorCode,
                    sea_orm::sea_query::Expr::value(Some("lease_expired".to_owned())),
                )
                .col_expr(
                    entities::code_turn::Column::LastErrorDetail,
                    sea_orm::sea_query::Expr::value(Some("final worker lease expired".to_owned())),
                )
                .col_expr(
                    entities::code_turn::Column::UpdatedAt,
                    sea_orm::sea_query::Expr::value(terminal_now),
                )
                .filter(entities::code_turn::Column::Id.eq(candidate.id))
                .filter(entities::code_turn::Column::Status.eq(TurnRunStatus::Running.as_str()))
                .filter(entities::code_turn::Column::AttemptCount.eq(candidate.attempt_count))
                .filter(entities::code_turn::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::code_turn::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
                .filter(entities::code_turn::Column::UpdatedAt.eq(candidate.updated_at))
                .exec(&transaction)
                .await
                .map_err(store_err)?;
            if failed.rows_affected != 1 {
                transaction.rollback().await.map_err(store_err)?;
                continue;
            }
            steer::reject_pending_turn_steers_on(&transaction, TurnId(candidate.id), terminal_now)
                .await?;
            let event = AgentEvent::TurnFailed {
                error: AgentErrorInfo {
                    kind: "lease_expired".into(),
                    message: "final worker lease expired".into(),
                },
            };
            let claim_token = candidate.lease_token.ok_or_else(|| {
                AgentError::Store(format!(
                    "terminalized turn {} is missing its claim token",
                    TurnId(candidate.id)
                ))
            })?;
            entities::code_turn_failure::ActiveModel {
                lease_token: Set(claim_token),
                turn_id: Set(candidate.id),
                owner: Set(candidate.owner.clone()),
                attempt_count: Set(candidate.attempt_count),
                model_steps: Set(candidate.model_steps),
                input_tokens: Set(candidate.input_tokens),
                output_tokens: Set(candidate.output_tokens),
                cache_read_input_tokens: Set(candidate.cache_read_input_tokens),
                cache_creation_input_tokens: Set(candidate.cache_creation_input_tokens),
                requested_retry_at: Set(None),
                error_code: Set("lease_expired".into()),
                error_detail: Set(Some("final worker lease expired".into())),
                resolved_at: Set(terminal_now),
                result_status: Set(TurnRunStatus::Failed.as_str().into()),
            }
            .insert(&transaction)
            .await
            .map_err(store_err)?;
            let sequenced_event = append_claim_scan_terminal_event_on(
                &transaction,
                &candidate,
                chat_id,
                lease_token,
                &event,
            )
            .await?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(ClaimTurnRunOutcome {
                turn: None,
                terminal_event: Some(ClaimScanTerminalEvent {
                    chat_id,
                    turn_id: TurnId(candidate.id),
                    event: sequenced_event,
                }),
            });
        }

        let resuming = candidate.status == TurnRunStatus::Resuming.as_str();
        let next_attempt = if resuming {
            candidate.attempt_count
        } else {
            candidate.attempt_count.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("turn {} attempt overflow", TurnId(candidate.id)))
            })?
        };
        let next_claim = candidate.claim_count.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("turn {} claim overflow", TurnId(candidate.id)))
        })?;
        let receipt =
            entities::code_turn_claim::Entity::insert(entities::code_turn_claim::ActiveModel {
                token: Set(lease_token),
                turn_id: Set(candidate.id),
                owner: Set(candidate.owner.clone()),
                attempt_count: Set(next_attempt),
                claim_count: Set(next_claim),
                claimed_at: Set(now),
                lease_expires_at: Set(lease_expires_at),
            })
            .on_conflict(
                sea_orm::sea_query::OnConflict::new()
                    .do_nothing()
                    .to_owned(),
            )
            .try_insert()
            .exec_without_returning(&transaction)
            .await
            .map_err(store_err)?;
        if !matches!(receipt, TryInsertResult::Inserted(1)) {
            transaction.rollback().await.map_err(store_err)?;
            if entities::code_turn_claim::Entity::find_by_id(lease_token)
                .one(&store.conn)
                .await
                .map_err(store_err)?
                .is_some()
            {
                continue;
            }
            let conflicting_claim = entities::code_turn_claim::Entity::find()
                .filter(entities::code_turn_claim::Column::TurnId.eq(candidate.id))
                .filter(entities::code_turn_claim::Column::ClaimCount.eq(next_claim))
                .one(&store.conn)
                .await
                .map_err(store_err)?;
            let current = entities::code_turn::Entity::find_by_id(candidate.id)
                .one(&store.conn)
                .await
                .map_err(store_err)?;
            if conflicting_claim.is_some()
                && current
                    .as_ref()
                    .is_some_and(|turn| turn.claim_count >= next_claim)
            {
                continue;
            }
            return Err(AgentError::Store(format!(
                "turn {} receipt for claim {next_claim} exists before the turn advanced",
                TurnId(candidate.id)
            )));
        }
        let claim = entities::code_turn::Entity::update_many()
            .col_expr(
                entities::code_turn::Column::Status,
                sea_orm::sea_query::Expr::value(TurnRunStatus::Running.as_str()),
            )
            .col_expr(
                entities::code_turn::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(next_attempt),
            )
            .col_expr(
                entities::code_turn::Column::ClaimCount,
                sea_orm::sea_query::Expr::value(next_claim),
            )
            .col_expr(
                entities::code_turn::Column::LeaseToken,
                sea_orm::sea_query::Expr::value(Some(lease_token)),
            )
            .col_expr(
                entities::code_turn::Column::LeaseExpiresAt,
                sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
            )
            .col_expr(
                entities::code_turn::Column::StartedAt,
                sea_orm::sea_query::Expr::value(
                    if candidate.status == TurnRunStatus::Queued.as_str() {
                        now
                    } else {
                        candidate.started_at
                    },
                ),
            )
            .col_expr(
                entities::code_turn::Column::LastErrorCode,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::code_turn::Column::LastErrorDetail,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::code_turn::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(now),
            )
            .filter(entities::code_turn::Column::Id.eq(candidate.id))
            .filter(entities::code_turn::Column::Status.eq(&candidate.status))
            .filter(entities::code_turn::Column::AttemptCount.eq(candidate.attempt_count))
            .filter(entities::code_turn::Column::ClaimCount.eq(candidate.claim_count))
            .filter(entities::code_turn::Column::UpdatedAt.eq(candidate.updated_at));
        let claim = if reclaiming {
            claim
                .filter(entities::code_turn::Column::LeaseToken.eq(candidate.lease_token))
                .filter(entities::code_turn::Column::LeaseExpiresAt.eq(candidate.lease_expires_at))
        } else {
            claim.filter(entities::code_turn::Column::AvailableAt.lte(now))
        };
        let claimed = claim.exec(&transaction).await.map_err(store_err)?;
        if claimed.rows_affected != 1 {
            transaction.rollback().await.map_err(store_err)?;
            continue;
        }

        let claimed = entities::code_turn::Entity::find_by_id(candidate.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| {
                AgentError::Store(format!("claimed turn {} disappeared", TurnId(candidate.id)))
            })
            .and_then(turn_run_from_model)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(ClaimTurnRunOutcome {
            turn: Some(claimed),
            terminal_event: None,
        });
    }
}

fn due_turn_candidate_condition(now: chrono::DateTime<Utc>) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(
            sea_orm::Condition::any()
                .add(entities::code_turn::Column::Status.eq(TurnRunStatus::Resuming.as_str()))
                .add(
                    sea_orm::Condition::all()
                        .add(entities::code_turn::Column::Status.is_in([
                            TurnRunStatus::Queued.as_str(),
                            TurnRunStatus::RetryWait.as_str(),
                        ]))
                        .add(
                            sea_orm::sea_query::Expr::col(
                                entities::code_turn::Column::AttemptCount,
                            )
                            .lt(sea_orm::sea_query::Expr::col(
                                entities::code_turn::Column::MaxAttempts,
                            )),
                        ),
                ),
        )
        .add(entities::code_turn::Column::AvailableAt.lte(now))
        .add(entities::code_turn::Column::UpdatedAt.lte(now))
        .add(entities::code_turn::Column::SessionId.not_in_subquery(code_runtime_session_ids()))
}

fn expired_turn_candidate_condition(now: chrono::DateTime<Utc>) -> sea_orm::Condition {
    sea_orm::Condition::all()
        .add(entities::code_turn::Column::Status.is_in([
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
        ]))
        .add(entities::code_turn::Column::LeaseExpiresAt.lte(now))
        .add(entities::code_turn::Column::UpdatedAt.lte(now))
        .add(entities::code_turn::Column::SessionId.not_in_subquery(code_runtime_session_ids()))
}

/// Session ids whose turns only a code session worker may claim.
///
/// Plain chats also use the internal harness. They stay on the global chat
/// lane until a code worker attaches, which is the boundary encoded by
/// `code_runtime_sessions`.
fn code_runtime_session_ids() -> sea_orm::sea_query::SelectStatement {
    sea_orm::sea_query::Query::select()
        .column(entities::code_session::Column::Id)
        .from(entities::code_session::Entity)
        .cond_where(super::code::code_runtime_sessions())
        .to_owned()
}

async fn any_turn_claim_work_on<C>(
    conn: &C,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if entities::code_event::Entity::find()
        .filter(entities::code_event::Column::ScanToken.eq(lease_token))
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some()
        || entities::code_turn_claim::Entity::find_by_id(lease_token)
            .one(conn)
            .await
            .map_err(store_err)?
            .is_some()
    {
        return Ok(true);
    }

    Ok(entities::code_turn::Entity::find()
        .filter(
            sea_orm::Condition::any()
                .add(due_turn_candidate_condition(now))
                .add(expired_turn_candidate_condition(now)),
        )
        .one(conn)
        .await
        .map_err(store_err)?
        .is_some())
}

pub(in crate::db) async fn heartbeat_turn(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
    lease_expires_at: chrono::DateTime<Utc>,
) -> Result<bool> {
    let now = canonical_db_timestamp(now)?;
    let lease_expires_at = canonical_db_timestamp(lease_expires_at)?;
    if lease_expires_at <= now {
        return Err(AgentError::Store(
            "turn lease expiry must be after heartbeat time".into(),
        ));
    }
    let heartbeat = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(lease_expires_at)),
        )
        .col_expr(
            entities::code_turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .filter(entities::code_turn::Column::Status.eq(TurnRunStatus::Running.as_str()))
        .filter(entities::code_turn::Column::LeaseToken.eq(lease_token))
        .filter(entities::code_turn::Column::LeaseExpiresAt.gt(now))
        .filter(entities::code_turn::Column::LeaseExpiresAt.lt(lease_expires_at))
        .filter(entities::code_turn::Column::UpdatedAt.lte(now))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(heartbeat.rows_affected == 1)
}

pub(in crate::db) async fn expire_turn_lease(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<bool> {
    let now = canonical_db_timestamp(now)?;
    let expired = entities::code_turn::Entity::update_many()
        .col_expr(
            entities::code_turn::Column::LeaseExpiresAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .col_expr(
            entities::code_turn::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::code_turn::Column::Id.eq(id.0))
        .filter(entities::code_turn::Column::Status.is_in([
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
        ]))
        .filter(entities::code_turn::Column::LeaseToken.eq(lease_token))
        .filter(entities::code_turn::Column::LeaseExpiresAt.gt(now))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(expired.rows_affected == 1)
}

pub(in crate::db) async fn fence_turn_lease(
    store: &DbStore,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<crate::storage::TurnLeaseFence> {
    if id.0.is_nil() {
        return Err(AgentError::Store("fenced turn id must not be nil".into()));
    }
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "fence lease token must not be nil".into(),
        ));
    }
    let now = canonical_db_timestamp(now)?;
    turn_lease_is_current_on(&store.conn, id, lease_token, now).await
}

/// Check the exact turn-claim identity on an existing transaction. Callers
/// that co-commit an effect first acquire the chat and turn write locks, then
/// use this helper before inserting or resolving that effect.
pub(in crate::db) async fn turn_lease_is_current_on<C>(
    conn: &C,
    id: TurnId,
    lease_token: uuid::Uuid,
    now: chrono::DateTime<Utc>,
) -> Result<crate::storage::TurnLeaseFence>
where
    C: ConnectionTrait,
{
    use crate::storage::TurnLeaseFence;

    // The claim receipt binds this token to the exact attempt and claim segment
    // it was issued for. Matching it against the turn's live counters rejects a
    // token that once owned an earlier segment of the same turn.
    let Some(claim) = entities::code_turn_claim::Entity::find_by_id(lease_token)
        .one(conn)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == id.0)
    else {
        return Ok(TurnLeaseFence::Stale);
    };
    let Some(turn) = entities::code_turn::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(TurnLeaseFence::Stale);
    };
    let owns_live_segment = (turn.status == TurnRunStatus::Running.as_str()
        || turn.status == TurnRunStatus::Cancelling.as_str())
        && turn.attempt_count == claim.attempt_count
        && turn.claim_count == claim.claim_count
        && turn.lease_token == Some(lease_token)
        && turn
            .lease_expires_at
            .is_some_and(|lease_expires_at| lease_expires_at > now);
    Ok(if owns_live_segment {
        TurnLeaseFence::Current
    } else {
        TurnLeaseFence::Stale
    })
}

pub(in crate::db) fn canonical_db_timestamp(
    timestamp: chrono::DateTime<Utc>,
) -> Result<chrono::DateTime<Utc>> {
    chrono::DateTime::from_timestamp_micros(timestamp.timestamp_micros())
        .ok_or_else(|| AgentError::Store("timestamp is outside the database range".into()))
}

async fn acquire_turn_claim_write_lock<C>(conn: &C) -> Result<()>
where
    C: ConnectionTrait,
{
    super::acquire_advisory_lock(conn, super::AdvisoryLockName::TurnClaim).await
}

async fn append_claim_scan_terminal_event_on<C>(
    conn: &C,
    candidate: &entities::code_turn::Model,
    chat_id: ChatId,
    scan_token: uuid::Uuid,
    event: &AgentEvent,
) -> Result<SequencedEvent>
where
    C: ConnectionTrait,
{
    let lease_token = candidate.lease_token.ok_or_else(|| {
        AgentError::Store(format!(
            "terminalized turn {} is missing its claim token",
            TurnId(candidate.id)
        ))
    })?;
    let seq = append_event_on(
        conn,
        chat_id,
        Some(TurnId(candidate.id)),
        Some(lease_token),
        Some(i32::MAX),
        Some(scan_token),
        event,
    )
    .await?;
    Ok(SequencedEvent {
        seq,
        event: event.clone(),
    })
}

fn claim_scan_terminal_event_from_model(
    model: entities::code_event::Model,
    scan_token: uuid::Uuid,
) -> Result<ClaimScanTerminalEvent> {
    if model.scan_token != Some(scan_token) || !model.terminal {
        return Err(AgentError::Store(format!(
            "claim scan token {scan_token} references an invalid journal receipt"
        )));
    }
    let turn_id = model.turn_id.map(TurnId).ok_or_else(|| {
        AgentError::Store(format!(
            "claim scan token {scan_token} references an event without a turn"
        ))
    })?;
    Ok(ClaimScanTerminalEvent {
        chat_id: ChatId(model.session_id),
        turn_id,
        event: SequencedEvent {
            seq: model.seq,
            event: crate::chat_journal::decode_chat_event_required(model.event)?,
        },
    })
}

fn turn_run_due_order(
    left: &entities::code_turn::Model,
    right: &entities::code_turn::Model,
) -> std::cmp::Ordering {
    let due_at = |run: &entities::code_turn::Model| {
        if run.status == TurnRunStatus::Running.as_str()
            || run.status == TurnRunStatus::Cancelling.as_str()
        {
            run.lease_expires_at
                .or(run.available_at)
                .unwrap_or(run.started_at)
        } else {
            run.available_at.unwrap_or(run.started_at)
        }
    };
    due_at(left)
        .cmp(&due_at(right))
        .then_with(|| left.started_at.cmp(&right.started_at))
        .then_with(|| left.id.cmp(&right.id))
}

pub(super) fn validate_turn_input(
    id: TurnId,
    model: &str,
    content: &str,
    invoked_skills: &[String],
) -> Result<()> {
    if id.0.is_nil() {
        return Err(AgentError::Store("turn id must not be nil".into()));
    }
    if model.trim().is_empty()
        || model.contains('\0')
        || model.chars().count() > TurnRun::MAX_MODEL_LEN
    {
        return Err(AgentError::Store(format!(
            "turn model must contain 1 to {} non-NUL characters",
            TurnRun::MAX_MODEL_LEN
        )));
    }
    if content.trim().is_empty() || content.contains('\0') {
        return Err(AgentError::Store(
            "turn content must be non-empty and contain no NUL characters".into(),
        ));
    }
    validate_invoked_skills(invoked_skills)
}

/// The bounds an invoked-skill list must satisfy to be stored.
///
/// Shared by the turn's opening message and by a steer, which carries its own
/// list under the same budget — one rule, so the two cannot drift apart.
pub(in crate::db) fn validate_invoked_skills(invoked_skills: &[String]) -> Result<()> {
    if invoked_skills.len() > TurnRun::MAX_INVOKED_SKILLS {
        return Err(AgentError::Store(format!(
            "a turn may invoke at most {} skills",
            TurnRun::MAX_INVOKED_SKILLS
        )));
    }
    if invoked_skills.iter().any(|skill| {
        skill.trim().is_empty()
            || skill.len() > TurnRun::MAX_INVOKED_SKILL_NAME_LEN
            || skill.chars().any(char::is_control)
    }) {
        return Err(AgentError::Store(format!(
            "an invoked skill name must be 1 to {} characters with no control characters",
            TurnRun::MAX_INVOKED_SKILL_NAME_LEN
        )));
    }
    Ok(())
}

pub(super) async fn find_active_turn_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Option<entities::code_turn::Model>>
where
    C: ConnectionTrait,
{
    entities::code_turn::Entity::find()
        .filter(entities::code_turn::Column::SessionId.eq(chat_id.0))
        .filter(entities::code_turn::Column::Status.is_in([
            TurnRunStatus::Queued.as_str(),
            TurnRunStatus::Running.as_str(),
            TurnRunStatus::Cancelling.as_str(),
            TurnRunStatus::WaitingForClient.as_str(),
            TurnRunStatus::WaitingForAgentRun.as_str(),
            TurnRunStatus::CancellingClient.as_str(),
            TurnRunStatus::Resuming.as_str(),
            TurnRunStatus::RetryWait.as_str(),
        ]))
        .one(conn)
        .await
        .map_err(store_err)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn exact_accepted_turn_on<C>(
    conn: &C,
    existing: entities::code_turn::Model,
    chat_id: ChatId,
    agent_run_id: AgentRunId,
    model: &str,
    content: &str,
    images: &[ImageRef],
    documents: &[DocumentId],
    invoked_skills: &[String],
    voice_input_used: bool,
) -> Result<AcceptTurnOutcome>
where
    C: ConnectionTrait,
{
    let input_message_id = existing.input_message_id.ok_or_else(|| {
        AgentError::Store(format!(
            "turn {} is missing its input message",
            TurnId(existing.id)
        ))
    })?;
    let message = entities::message::Entity::find_by_id(input_message_id)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "turn {} is missing its input message",
                TurnId(existing.id)
            ))
        })?;
    if message.chat_id != existing.session_id
        || message.turn_id != existing.id
        || message.role != "user"
    {
        return Err(AgentError::Store(format!(
            "turn {} has inconsistent accepted input",
            TurnId(existing.id)
        )));
    }
    // Attachments are part of the accepted input, so they are part of the
    // idempotency proof. Without this a retried submit carrying different
    // images would be reported as the same accepted turn while the durable
    // history kept the first submission's images.
    let accepted_images =
        message_attachment_ops::list_for_message_on(conn, MessageId(message.id)).await?;
    let accepted_documents =
        message_document_attachment_ops::list_ids_for_message_on(conn, MessageId(message.id))
            .await?;
    let _ = agent_run_id;
    let exact = existing.session_id == chat_id.0
        && existing.model.as_deref() == Some(model)
        && message.content == content
        && accepted_images == images
        && accepted_documents == documents
        && invoked_skills_from_model(&existing)? == invoked_skills
        && existing.voice_input_used == voice_input_used;
    Ok(if exact {
        AcceptTurnOutcome::Existing(turn_run_from_model(existing)?)
    } else {
        AcceptTurnOutcome::IdentityConflict
    })
}

pub(in crate::db) fn turn_run_from_model(model: entities::code_turn::Model) -> Result<TurnRun> {
    let usage = usage_from_turn_model(&model)?;
    let invoked_skills = invoked_skills_from_model(&model)?;
    let input_message_id = MessageId(model.input_message_id.unwrap_or(uuid::Uuid::nil()));
    let available_at = model.available_at.unwrap_or(model.started_at);
    let updated_at = model.updated_at.unwrap_or(model.started_at);
    Ok(TurnRun {
        id: TurnId(model.id),
        chat_id: ChatId(model.session_id),
        agent_run_id: AgentRunId::foreground_for_chat(ChatId(model.session_id)),
        input_message_id,
        output_message_id: model.output_message_id.map(MessageId),
        model: model.model.unwrap_or_default(),
        invoked_skills,
        voice_input_used: model.voice_input_used,
        status: turn_run_status_from_db(&model.status)?,
        attempt_count: model.attempt_count,
        max_attempts: model.max_attempts,
        claim_count: model.claim_count,
        model_steps: model.model_steps,
        usage,
        available_at,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        started_at: Some(model.started_at),
        finished_at: model.ended_at,
        last_error_code: model.last_error_code,
        last_error_detail: model.last_error_detail,
        steer_revision: model.steer_revision,
        last_steer_applied_at: model.last_steer_applied_at,
        created_at: model.started_at,
        updated_at,
    })
}

/// Read back the skills the user invoked when the turn was accepted.
///
/// A row whose value is not an array of strings is corrupt rather than merely
/// unusual: the turn's accepted input can no longer be described honestly, so
/// this fails instead of degrading to "no skills were invoked".
pub(in crate::db) fn invoked_skills_from_model(
    model: &entities::code_turn::Model,
) -> Result<Vec<String>> {
    serde_json::from_value(model.invoked_skills.clone()).map_err(|error| {
        AgentError::Store(format!(
            "turn {} has unreadable invoked skills: {error}",
            TurnId(model.id)
        ))
    })
}

pub(in crate::db) fn usage_from_turn_model(model: &entities::code_turn::Model) -> Result<Usage> {
    fn token_count(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value).map_err(|_| {
            AgentError::Store(format!(
                "turn {field} token count is outside the supported range"
            ))
        })
    }

    Ok(Usage {
        input_tokens: token_count(model.input_tokens, "input")?,
        output_tokens: token_count(model.output_tokens, "output")?,
        cache_read_input_tokens: token_count(model.cache_read_input_tokens, "cache-read input")?,
        cache_creation_input_tokens: token_count(
            model.cache_creation_input_tokens,
            "cache-creation input",
        )?,
    })
}

fn turn_run_status_from_db(text: &str) -> Result<TurnRunStatus> {
    match text {
        "queued" => Ok(TurnRunStatus::Queued),
        "running" => Ok(TurnRunStatus::Running),
        "cancelling" => Ok(TurnRunStatus::Cancelling),
        "waiting_for_client" | "waiting" => Ok(TurnRunStatus::WaitingForClient),
        "waiting_for_agent_run" => Ok(TurnRunStatus::WaitingForAgentRun),
        "cancelling_client" => Ok(TurnRunStatus::CancellingClient),
        "resuming" => Ok(TurnRunStatus::Resuming),
        "retry_wait" => Ok(TurnRunStatus::RetryWait),
        "completed" => Ok(TurnRunStatus::Completed),
        "failed" => Ok(TurnRunStatus::Failed),
        "cancelled" | "interrupted" => Ok(TurnRunStatus::Cancelled),
        other => Err(AgentError::Store(format!(
            "unknown durable turn status: {other}"
        ))),
    }
}
