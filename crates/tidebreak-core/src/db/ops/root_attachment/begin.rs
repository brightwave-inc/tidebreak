use chrono::{DateTime, Utc};
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::model::{
    BeginRootAttachmentChange, RootAttachmentChange, RootAttachmentChangeAction,
    RootAttachmentChangePhase, RootAttachmentOrigin, MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};
use crate::storage::BeginRootAttachmentChangeOutcome;

use super::super::super::{entities, store_err, DbStore};
use super::super::acquire_chat_write_lock;
use super::super::conversation::attachment_origin_to_db;
use super::super::turn::canonical_db_timestamp;
use super::codec::{change_active_model, phase_to_db};
use super::persistence::{derive_subject, find_change, find_change_on, validate_change_subject_on};
use super::projection::{load_projection, set_chat_revision};

pub(in crate::db) async fn begin_root_attachment_change(
    store: &DbStore,
    request: &BeginRootAttachmentChange,
) -> Result<BeginRootAttachmentChangeOutcome> {
    request
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;
    let created_at = canonical_db_timestamp(request.created_at)?;

    if let Some(existing) = find_change(store, request.id).await? {
        return Ok(exact_begin_outcome(existing, request, created_at));
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(BeginRootAttachmentChangeOutcome::ChatNotFound);
    }

    if let Some(existing) = find_change_on(&transaction, request.id).await? {
        let outcome = exact_begin_outcome(existing, request, created_at);
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }

    let chat = entities::code_session::Entity::find_by_id(request.chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "chat {} disappeared while beginning root attachment change {}",
                request.chat_id, request.id
            ))
        })?;
    let projection =
        load_projection(&transaction, request.chat_id, chat.attachment_revision).await?;

    if let Some(busy) = entities::root_attachment_change::Entity::find()
        .filter(entities::root_attachment_change::Column::ChatId.eq(request.chat_id.0))
        .filter(
            entities::root_attachment_change::Column::Phase
                .eq(phase_to_db(RootAttachmentChangePhase::AwaitingBroker)),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let busy = super::codec::change_from_model(busy)?;
        validate_change_subject_on(&transaction, &busy).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(BeginRootAttachmentChangeOutcome::ChatBusy);
    }
    if chat.attachment_revision != request.expected_attachment_revision {
        transaction.commit().await.map_err(store_err)?;
        return Ok(BeginRootAttachmentChangeOutcome::RevisionConflict {
            current_attachment_revision: chat.attachment_revision,
        });
    }

    let existing_position = projection
        .iter()
        .position(|row| row.root_id == *request.root_id.as_uuid());
    let projection_existed_before = existing_position.is_some();
    let (origin, projection_position) = match existing_position {
        Some(position) => (
            Some(super::super::conversation::attachment_origin_from_db(
                &projection[position].origin,
            )?),
            Some(u32::try_from(position).map_err(|_| {
                AgentError::Store("root attachment projection position exceeds u32".into())
            })?),
        ),
        None if request.action == RootAttachmentChangeAction::Attach => {
            if projection.len() >= MAX_ROOT_ATTACHMENTS {
                transaction.commit().await.map_err(store_err)?;
                return Ok(BeginRootAttachmentChangeOutcome::CapacityExceeded);
            }
            (
                Some(RootAttachmentOrigin::Conversation),
                Some(u32::try_from(projection.len()).map_err(|_| {
                    AgentError::Store("root attachment projection position exceeds u32".into())
                })?),
            )
        }
        None => (None, None),
    };

    let (subject_kind, subject_id) = derive_subject(request.chat_id, chat.project_id)?;
    let advances_intent =
        request.action == RootAttachmentChangeAction::Attach && !projection_existed_before;
    let needs_terminal_revision = advances_intent
        || (request.action == RootAttachmentChangeAction::Detach && projection_existed_before);
    let required_headroom = i64::from(advances_intent) + i64::from(needs_terminal_revision);
    let Some(max_begin_revision) = MAX_ATTACHMENT_REVISION.checked_sub(required_headroom) else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(BeginRootAttachmentChangeOutcome::RevisionExhausted);
    };
    if chat.attachment_revision > max_begin_revision {
        transaction.commit().await.map_err(store_err)?;
        return Ok(BeginRootAttachmentChangeOutcome::RevisionExhausted);
    }
    let intent_revision = chat.attachment_revision + i64::from(advances_intent);

    let change = RootAttachmentChange {
        id: request.id,
        chat_id: request.chat_id,
        executor_id: request.executor_id,
        root_id: request.root_id,
        action: request.action,
        subject_kind,
        subject_id,
        origin,
        projection_position,
        projection_existed_before,
        expected_revision: request.expected_attachment_revision,
        before_revision: chat.attachment_revision,
        intent_revision,
        phase: RootAttachmentChangePhase::AwaitingBroker,
        result_revision: None,
        projection_changed: None,
        broker_changed: None,
        broker_currently_attached: None,
        failure: None,
        created_at,
        finished_at: None,
    };
    change
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;

    let inserted = entities::root_attachment_change::Entity::insert(change_active_model(&change))
        .on_conflict(
            OnConflict::column(entities::root_attachment_change::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .try_insert()
        .exec_without_returning(&transaction)
        .await;
    match inserted {
        Ok(TryInsertResult::Inserted(1)) => {}
        Ok(_) => {
            transaction.rollback().await.map_err(store_err)?;
            return recover_begin_insert_race(store, request, created_at).await;
        }
        Err(error) => {
            transaction.rollback().await.map_err(store_err)?;
            if let Some(outcome) = recover_begin_state(store, request, created_at).await? {
                return Ok(outcome);
            }
            return Err(store_err(error));
        }
    }

    if advances_intent {
        entities::chat_root_attachment::ActiveModel {
            chat_id: Set(request.chat_id.0),
            root_id: Set(*request.root_id.as_uuid()),
            position: Set(i32::try_from(projection.len()).map_err(|_| {
                AgentError::Store("root attachment projection position exceeds i32".into())
            })?),
            origin: Set(attachment_origin_to_db(RootAttachmentOrigin::Conversation).into()),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        set_chat_revision(
            &transaction,
            request.chat_id,
            chat.attachment_revision,
            intent_revision,
        )
        .await?;
    }

    transaction.commit().await.map_err(store_err)?;
    Ok(BeginRootAttachmentChangeOutcome::Begun(change))
}

fn exact_begin_outcome(
    existing: RootAttachmentChange,
    request: &BeginRootAttachmentChange,
    created_at: DateTime<Utc>,
) -> BeginRootAttachmentChangeOutcome {
    if existing.chat_id == request.chat_id
        && existing.executor_id == request.executor_id
        && existing.root_id == request.root_id
        && existing.action == request.action
        && existing.expected_revision == request.expected_attachment_revision
        && existing.created_at == created_at
    {
        BeginRootAttachmentChangeOutcome::Existing(existing)
    } else {
        BeginRootAttachmentChangeOutcome::IdentityConflict
    }
}

async fn recover_begin_insert_race(
    store: &DbStore,
    request: &BeginRootAttachmentChange,
    created_at: DateTime<Utc>,
) -> Result<BeginRootAttachmentChangeOutcome> {
    if let Some(outcome) = recover_begin_state(store, request, created_at).await? {
        return Ok(outcome);
    }
    Err(AgentError::Store(format!(
        "root attachment change {} insert was ignored without durable state",
        request.id
    )))
}

async fn recover_begin_state(
    store: &DbStore,
    request: &BeginRootAttachmentChange,
    created_at: DateTime<Utc>,
) -> Result<Option<BeginRootAttachmentChangeOutcome>> {
    if let Some(existing) = find_change(store, request.id).await? {
        return Ok(Some(exact_begin_outcome(existing, request, created_at)));
    }
    let busy = entities::root_attachment_change::Entity::find()
        .filter(entities::root_attachment_change::Column::ChatId.eq(request.chat_id.0))
        .filter(
            entities::root_attachment_change::Column::Phase
                .eq(phase_to_db(RootAttachmentChangePhase::AwaitingBroker)),
        )
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(super::codec::change_from_model)
        .transpose()?;
    if let Some(busy) = &busy {
        validate_change_subject_on(&store.conn, busy).await?;
    }
    Ok(busy.map(|_| BeginRootAttachmentChangeOutcome::ChatBusy))
}
