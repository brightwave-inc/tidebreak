//! Durable queued-message operations: messages accepted while a chat had a
//! live turn, promoted into real turns strictly FIFO once the chat is free.
//!
//! Promotion re-reads the exact FIFO head beneath the chat write lock and
//! commits admission, message, turn, and queue removal together. Queue edits,
//! reorders, and retractions use the same lock, so whichever operation wins is
//! authoritative and a stale promoter snapshot can neither execute nor delete.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::id::{ChatId, DocumentId, TurnId};
use crate::image::ImageRef;
use crate::model::{AgentRunStatus, QueuedTurn, TurnAdmissionLease, TurnAdmissionRequest};
use crate::storage::{AcceptTurnOutcome, PromoteQueuedTurnOutcome, ReservedQueuedTurnOutcome};

use super::super::super::{entities, store_err, DbStore};
use super::super::acquire_chat_write_lock;
use super::super::agent_run::database_now;
use super::admission;

fn queued_turn_from_model(model: entities::queued_turn::Model) -> Result<QueuedTurn> {
    let parse = |json: &str| -> Result<Vec<uuid::Uuid>> {
        serde_json::from_str(json)
            .map_err(|_| AgentError::Store("invalid stored queued-turn attachment list".into()))
    };
    Ok(QueuedTurn {
        id: TurnId(model.id),
        chat_id: ChatId(model.chat_id),
        content: model.content,
        attachments: parse(&model.attachments_json)?,
        file_attachments: parse(&model.file_attachments_json)?
            .into_iter()
            .map(DocumentId)
            .collect(),
        invoked_skills: serde_json::from_str(&model.invoked_skills_json)
            .map_err(|_| AgentError::Store("invalid stored queued-turn skill list".into()))?,
        voice_input_used: model.voice_input_used,
        position: model.position,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn admission_request(queued: &QueuedTurn) -> TurnAdmissionRequest {
    TurnAdmissionRequest {
        id: queued.id,
        chat_id: queued.chat_id,
        content: queued.content.clone(),
        attachments: queued.attachments.clone(),
        file_attachments: queued.file_attachments.clone(),
        invoked_skills: queued.invoked_skills.clone(),
        voice_input_used: queued.voice_input_used,
    }
}

async fn current_head_on<C>(conn: &C, chat_id: ChatId) -> Result<Option<QueuedTurn>>
where
    C: sea_orm::ConnectionTrait,
{
    entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::queued_turn::Column::Position)
        .order_by_asc(entities::queued_turn::Column::CreatedAt)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(queued_turn_from_model)
        .transpose()
}

pub(in crate::db) async fn enqueue_turn(
    store: &DbStore,
    queued: &QueuedTurn,
) -> Result<QueuedTurn> {
    match enqueue_turn_inner(store, None, queued).await? {
        ReservedQueuedTurnOutcome::Queued(queued) => Ok(queued),
        ReservedQueuedTurnOutcome::LeaseLost => Err(AgentError::Store(format!(
            "queued turn {} has an unresolved admission owned by another process",
            queued.id
        ))),
    }
}

pub(in crate::db) async fn enqueue_reserved_turn(
    store: &DbStore,
    lease: TurnAdmissionLease,
    queued: &QueuedTurn,
) -> Result<ReservedQueuedTurnOutcome> {
    enqueue_turn_inner(store, Some(lease), queued).await
}

async fn enqueue_turn_inner(
    store: &DbStore,
    reservation: Option<TurnAdmissionLease>,
    queued: &QueuedTurn,
) -> Result<ReservedQueuedTurnOutcome> {
    if queued.id.0.is_nil() || queued.content.trim().is_empty() || queued.content.contains('\0') {
        return Err(AgentError::Store("invalid queued turn".into()));
    }
    let request = admission_request(queued);
    admission::validate_request(&request)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, queued.chat_id).await? {
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            queued.chat_id
        )));
    }
    let now = database_now(&transaction).await?;

    let admission_row = entities::turn_admission::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(row) = admission_row.as_ref() {
        if !admission::request_matches(row, &request) {
            transaction.rollback().await.map_err(store_err)?;
            return Err(AgentError::Store(
                "queued turn id was already used with different request data".into(),
            ));
        }
        match row.state.as_str() {
            admission::STATE_QUEUED => {}
            admission::STATE_ACCEPTED => {
                transaction.rollback().await.map_err(store_err)?;
                return Err(AgentError::Store(
                    "queued turn id was already accepted".into(),
                ));
            }
            admission::STATE_PENDING => {
                let Some(lease) = reservation else {
                    transaction.rollback().await.map_err(store_err)?;
                    return Ok(ReservedQueuedTurnOutcome::LeaseLost);
                };
                if !admission::lease_is_current_on(
                    &transaction,
                    lease,
                    queued.chat_id,
                    request.fingerprint(),
                )
                .await?
                {
                    transaction.rollback().await.map_err(store_err)?;
                    return Ok(ReservedQueuedTurnOutcome::LeaseLost);
                }
            }
            state => {
                transaction.rollback().await.map_err(store_err)?;
                return Err(AgentError::Store(format!(
                    "turn admission {} has invalid state {state}",
                    queued.id
                )));
            }
        }
    } else if reservation.is_some() {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(ReservedQueuedTurnOutcome::LeaseLost);
    }

    if let Some(existing) = entities::queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        // An ambiguous retry of the same enqueue is the row it made.
        let existing = queued_turn_from_model(existing)?;
        transaction.commit().await.map_err(store_err)?;
        if existing.same_request(queued) {
            return Ok(ReservedQueuedTurnOutcome::Queued(existing));
        }
        return Err(AgentError::Store(
            "queued turn id was already used with different request data".into(),
        ));
    }
    let count = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(queued.chat_id.0))
        .count(&transaction)
        .await
        .map_err(store_err)?;
    if count >= QueuedTurn::MAX_PER_CHAT as u64 {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "a chat may queue at most {} messages",
            QueuedTurn::MAX_PER_CHAT
        )));
    }
    let position = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(queued.chat_id.0))
        .order_by_desc(entities::queued_turn::Column::Position)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .map_or(0, |last| last.position + 1);

    if let Some(lease) = reservation {
        if !admission::transition_pending_on(&transaction, lease, admission::STATE_QUEUED).await? {
            transaction.rollback().await.map_err(store_err)?;
            return Ok(ReservedQueuedTurnOutcome::LeaseLost);
        }
    } else if admission_row.is_none() {
        let inserted =
            entities::turn_admission::Entity::insert(entities::turn_admission::ActiveModel {
                id: Set(queued.id.0),
                chat_id: Set(queued.chat_id.0),
                fingerprint: Set(request.fingerprint().to_vec()),
                state: Set(admission::STATE_QUEUED.into()),
                lease_token: Set(None),
                lease_expires_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            })
            .on_conflict_do_nothing()
            .exec_without_returning(&transaction)
            .await
            .map_err(store_err)?;
        if !matches!(inserted, TryInsertResult::Inserted(1)) {
            transaction.rollback().await.map_err(store_err)?;
            return Err(AgentError::Store(
                "queued turn id was already reserved".into(),
            ));
        }
    }
    entities::queued_turn::ActiveModel {
        id: Set(queued.id.0),
        chat_id: Set(queued.chat_id.0),
        content: Set(queued.content.clone()),
        attachments_json: Set(serde_json::to_string(&queued.attachments).map_err(store_err)?),
        file_attachments_json: Set(serde_json::to_string(
            &queued
                .file_attachments
                .iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
        )
        .map_err(store_err)?),
        invoked_skills_json: Set(serde_json::to_string(&queued.invoked_skills).map_err(store_err)?),
        voice_input_used: Set(queued.voice_input_used),
        position: Set(position),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    let inserted = entities::queued_turn::Entity::find_by_id(queued.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let inserted = queued_turn_from_model(inserted)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(ReservedQueuedTurnOutcome::Queued(inserted))
}

pub(in crate::db) async fn promote_turn(
    store: &DbStore,
    expected: &QueuedTurn,
    model: &str,
    images: &[ImageRef],
) -> Result<PromoteQueuedTurnOutcome> {
    super::validate_turn_input(
        expected.id,
        model,
        &expected.content,
        &expected.invoked_skills,
    )?;
    super::message_attachment_ops::validate(images)?;
    super::message_document_attachment_ops::validate_count(
        images.len(),
        &expected.file_attachments,
    )?;
    let request = admission_request(expected);
    admission::validate_request(&request)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let Some(head) = current_head_on(&transaction, expected.chat_id).await? else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    };
    if head != *expected {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let Some(admission_row) = entities::turn_admission::Entity::find_by_id(expected.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    };
    if !admission::request_matches(&admission_row, &request) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let foreground = super::find_foreground_agent_run_on(&transaction, expected.chat_id)
        .await?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "chat {} has no foreground agent run",
                expected.chat_id
            ))
        })?;

    if admission_row.state == admission::STATE_ACCEPTED {
        let Some(existing) = entities::turn_run::Entity::find_by_id(expected.id.0)
            .one(&transaction)
            .await
            .map_err(store_err)?
        else {
            return Err(AgentError::Store(format!(
                "accepted turn admission {} is missing its turn",
                expected.id
            )));
        };
        let outcome = super::exact_accepted_turn_on(
            &transaction,
            existing,
            expected.chat_id,
            foreground.id,
            model,
            &expected.content,
            images,
            &expected.file_attachments,
            &expected.invoked_skills,
            expected.voice_input_used,
        )
        .await?;
        let AcceptTurnOutcome::Existing(existing) = outcome else {
            transaction.commit().await.map_err(store_err)?;
            return Ok(PromoteQueuedTurnOutcome::Stale);
        };
        delete_exact_head_on(&transaction, expected).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Existing(existing));
    }
    if admission_row.state != admission::STATE_QUEUED {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    if foreground.status != AgentRunStatus::Active {
        return Err(AgentError::Store(format!(
            "chat {} foreground agent run is not active",
            expected.chat_id
        )));
    }
    if let Some(active) = super::find_active_turn_on(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::ChatBusy(
            super::turn_run_from_model(active)?,
        ));
    }
    if entities::turn_run::Entity::find_by_id(expected.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Err(AgentError::Store(format!(
            "queued admission {} already has a turn",
            expected.id
        )));
    }

    let now = database_now(&transaction).await?;
    let transitioned = entities::turn_admission::Entity::update_many()
        .col_expr(
            entities::turn_admission::Column::State,
            sea_orm::sea_query::Expr::value(admission::STATE_ACCEPTED),
        )
        .col_expr(
            entities::turn_admission::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(entities::turn_admission::Column::Id.eq(expected.id.0))
        .filter(entities::turn_admission::Column::ChatId.eq(expected.chat_id.0))
        .filter(entities::turn_admission::Column::Fingerprint.eq(request.fingerprint().to_vec()))
        .filter(entities::turn_admission::Column::State.eq(admission::STATE_QUEUED))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if transitioned.rows_affected != 1 {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(PromoteQueuedTurnOutcome::Stale);
    }
    let inserted = super::insert_accepted_turn_on(
        &transaction,
        expected.id,
        expected.chat_id,
        foreground.id,
        model,
        &expected.content,
        images,
        &expected.file_attachments,
        &expected.invoked_skills,
        expected.voice_input_used,
        now,
    )
    .await?;
    delete_exact_head_on(&transaction, expected).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(PromoteQueuedTurnOutcome::Promoted(
        super::turn_run_from_model(inserted)?,
    ))
}

async fn delete_exact_head_on<C>(conn: &C, expected: &QueuedTurn) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let deleted = entities::queued_turn::Entity::delete_many()
        .filter(entities::queued_turn::Column::Id.eq(expected.id.0))
        .filter(entities::queued_turn::Column::ChatId.eq(expected.chat_id.0))
        .filter(entities::queued_turn::Column::Position.eq(expected.position))
        .filter(entities::queued_turn::Column::UpdatedAt.eq(expected.updated_at))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if deleted.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "queued turn {} changed beneath its chat lock",
            expected.id
        )));
    }
    Ok(())
}

pub(in crate::db) async fn delete_turn_if_current(
    store: &DbStore,
    expected: &QueuedTurn,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, expected.chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    if current_head_on(&transaction, expected.chat_id)
        .await?
        .as_ref()
        != Some(expected)
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let request = admission_request(expected);
    let owns_queue = entities::turn_admission::Entity::find_by_id(expected.id.0)
        .filter(entities::turn_admission::Column::ChatId.eq(expected.chat_id.0))
        .filter(entities::turn_admission::Column::Fingerprint.eq(request.fingerprint().to_vec()))
        .filter(entities::turn_admission::Column::State.eq(admission::STATE_QUEUED))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some();
    if !owns_queue {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    delete_exact_head_on(&transaction, expected).await?;
    entities::turn_admission::Entity::delete_by_id(expected.id.0)
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

pub(in crate::db) async fn list_queued_turns(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<QueuedTurn>> {
    entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::queued_turn::Column::Position)
        .order_by_asc(entities::queued_turn::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(queued_turn_from_model)
        .collect()
}

/// Chats that currently hold at least one queued message, for the promoter's
/// scan. Bounded output: distinct chat ids only.
pub(in crate::db) async fn chats_with_queued_turns(store: &DbStore) -> Result<Vec<ChatId>> {
    use sea_orm::QuerySelect;
    Ok(entities::queued_turn::Entity::find()
        .select_only()
        .column(entities::queued_turn::Column::ChatId)
        .distinct()
        .into_tuple::<uuid::Uuid>()
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(ChatId)
        .collect())
}

pub(in crate::db) async fn delete_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    id: TurnId,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let deleted = entities::queued_turn::Entity::delete_many()
        .filter(entities::queued_turn::Column::Id.eq(id.0))
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if deleted.rows_affected == 1 {
        entities::turn_admission::Entity::delete_many()
            .filter(entities::turn_admission::Column::Id.eq(id.0))
            .filter(entities::turn_admission::Column::ChatId.eq(chat_id.0))
            .filter(entities::turn_admission::Column::State.eq(admission::STATE_QUEUED))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(deleted.rows_affected == 1)
}

/// Edit a queued message's content and/or move it to a new position. Rewrites
/// every position in the chat so the order stays dense and total.
pub(in crate::db) async fn update_queued_turn(
    store: &DbStore,
    chat_id: ChatId,
    id: TurnId,
    content: Option<&str>,
    position: Option<i32>,
) -> Result<Option<QueuedTurn>> {
    if let Some(content) = content {
        if content.trim().is_empty() || content.contains('\0') {
            return Err(AgentError::Store("invalid queued-turn content".into()));
        }
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let now = database_now(&transaction).await?;
    let mut rows = entities::queued_turn::Entity::find()
        .filter(entities::queued_turn::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::queued_turn::Column::Position)
        .order_by_asc(entities::queued_turn::Column::CreatedAt)
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let Some(index) = rows.iter().position(|row| row.id == id.0) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if let Some(target) = position {
        let target = usize::try_from(target.max(0))
            .unwrap_or(0)
            .min(rows.len() - 1);
        let row = rows.remove(index);
        rows.insert(target, row);
    }
    for (ordinal, row) in rows.iter().enumerate() {
        let is_edited = row.id == id.0;
        let mut active = entities::queued_turn::ActiveModel {
            id: Set(row.id),
            position: Set(i32::try_from(ordinal).unwrap_or(i32::MAX)),
            ..Default::default()
        };
        if is_edited {
            if let Some(content) = content {
                active.content = Set(content.to_owned());
            }
            active.updated_at = Set(now);
        }
        active.update(&transaction).await.map_err(store_err)?;
    }
    let updated = entities::queued_turn::Entity::find_by_id(id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("queued turn disappeared".into()))?;
    let updated = queued_turn_from_model(updated)?;
    let request = TurnAdmissionRequest {
        id: updated.id,
        chat_id: updated.chat_id,
        content: updated.content.clone(),
        attachments: updated.attachments.clone(),
        file_attachments: updated.file_attachments.clone(),
        invoked_skills: updated.invoked_skills.clone(),
        voice_input_used: updated.voice_input_used,
    };
    if !admission::update_queued_fingerprint_on(&transaction, &request, now).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "queued turn {} is missing its admission ownership",
            updated.id
        )));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(updated))
}
