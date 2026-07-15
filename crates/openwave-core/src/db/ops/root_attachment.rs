use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait, TryInsertResult,
};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::id::{ChatId, HostRootId, RootAttachmentChangeId};
use crate::model::{
    BeginRootAttachmentChange, RootAttachmentChange, RootAttachmentChangeAction,
    RootAttachmentChangeFailure, RootAttachmentChangePhase, RootAttachmentChangeTerminal,
    RootAttachmentOrigin, RootAttachmentSubjectKind, MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};
use crate::storage::{
    BeginRootAttachmentChangeOutcome, FinishRootAttachmentChangeOutcome,
    MAX_PENDING_ROOT_ATTACHMENT_CHANGES,
};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::conversation::{attachment_origin_from_db, attachment_origin_to_db};
use super::turn::canonical_db_timestamp;

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

    let chat = entities::chat::Entity::find_by_id(request.chat_id.0)
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
        let busy = change_from_model(busy)?;
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
            Some(attachment_origin_from_db(&projection[position].origin)?),
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
        .do_nothing()
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

pub(in crate::db) async fn finish_root_attachment_change(
    store: &DbStore,
    id: RootAttachmentChangeId,
    executor_id: Uuid,
    terminal: &RootAttachmentChangeTerminal,
    finished_at: DateTime<Utc>,
) -> Result<FinishRootAttachmentChangeOutcome> {
    if executor_id.is_nil() {
        return Err(AgentError::Store(
            "root attachment change executor id must not be nil".into(),
        ));
    }
    terminal
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;
    let finished_at = canonical_db_timestamp(finished_at)?;
    let Some(initial) = find_change(store, id).await? else {
        return Ok(FinishRootAttachmentChangeOutcome::NotFound);
    };
    if initial.executor_id != executor_id {
        return Ok(FinishRootAttachmentChangeOutcome::ExecutorMismatch);
    }

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, initial.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "root attachment change {id} references missing chat {}",
            initial.chat_id
        )));
    }
    let Some(mut change) = find_change_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "root attachment change {id} disappeared while locked"
        )));
    };
    if change.executor_id != executor_id {
        transaction.commit().await.map_err(store_err)?;
        return Ok(FinishRootAttachmentChangeOutcome::ExecutorMismatch);
    }
    if change.phase != RootAttachmentChangePhase::AwaitingBroker {
        let outcome = if terminal_matches(&change, terminal) {
            FinishRootAttachmentChangeOutcome::Existing(change)
        } else {
            FinishRootAttachmentChangeOutcome::AlreadyTerminal(change)
        };
        transaction.commit().await.map_err(store_err)?;
        return Ok(outcome);
    }
    if finished_at < change.created_at {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(
            "root attachment change finish time precedes creation".into(),
        ));
    }

    if let RootAttachmentChangeTerminal::Completed {
        broker_currently_attached,
        ..
    } = terminal
    {
        let desired = change.action == RootAttachmentChangeAction::Attach;
        if *broker_currently_attached != desired {
            transaction.commit().await.map_err(store_err)?;
            return Ok(FinishRootAttachmentChangeOutcome::BrokerStateMismatch);
        }
    }
    if let RootAttachmentChangeTerminal::Failed {
        broker_currently_attached: Some(broker_currently_attached),
        ..
    } = terminal
    {
        let desired = change.action == RootAttachmentChangeAction::Attach;
        if *broker_currently_attached == desired {
            transaction.commit().await.map_err(store_err)?;
            return Ok(FinishRootAttachmentChangeOutcome::BrokerStateMismatch);
        }
    }

    let chat = entities::chat::Entity::find_by_id(change.chat_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!("chat {} disappeared while locked", change.chat_id))
        })?;
    if chat.attachment_revision != change.intent_revision {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} attachment revision changed while root attachment change {id} was pending",
            change.chat_id
        )));
    }
    let projection =
        load_projection(&transaction, change.chat_id, chat.attachment_revision).await?;
    validate_pending_projection(&change, &projection)?;

    let completed = matches!(terminal, RootAttachmentChangeTerminal::Completed { .. });
    let remove_projection = (completed
        && change.action == RootAttachmentChangeAction::Detach
        && change.projection_existed_before)
        || (!completed
            && change.action == RootAttachmentChangeAction::Attach
            && !change.projection_existed_before);
    let result_revision = if remove_projection {
        let position = change.projection_position.ok_or_else(|| {
            AgentError::Store(format!(
                "root attachment change {id} has no projection position"
            ))
        })?;
        remove_projection_row(
            &transaction,
            change.chat_id,
            change.root_id,
            position,
            change.intent_revision,
        )
        .await?;
        change.intent_revision.checked_add(1).ok_or_else(|| {
            AgentError::Store(format!("root attachment change {id} exhausted revisions"))
        })?
    } else {
        change.intent_revision
    };

    change.phase = if completed {
        RootAttachmentChangePhase::Completed
    } else {
        RootAttachmentChangePhase::Failed
    };
    change.result_revision = Some(result_revision);
    change.projection_changed =
        Some(completed && change.projection_existed_before != desired_state(&change));
    match terminal {
        RootAttachmentChangeTerminal::Completed {
            broker_changed,
            broker_currently_attached,
        } => {
            change.broker_changed = Some(*broker_changed);
            change.broker_currently_attached = Some(*broker_currently_attached);
            change.failure = None;
        }
        RootAttachmentChangeTerminal::Failed {
            broker_changed,
            broker_currently_attached,
            failure,
        } => {
            change.broker_changed = *broker_changed;
            change.broker_currently_attached = *broker_currently_attached;
            change.failure = Some(failure.clone());
        }
    }
    change.finished_at = Some(finished_at);
    change
        .validate()
        .map_err(|message| AgentError::Store(message.into()))?;
    persist_terminal_change(&transaction, &change).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(FinishRootAttachmentChangeOutcome::Finished(change))
}

pub(in crate::db) async fn get_root_attachment_change(
    store: &DbStore,
    id: RootAttachmentChangeId,
) -> Result<Option<RootAttachmentChange>> {
    find_change(store, id).await
}

pub(in crate::db) async fn list_pending_root_attachment_changes(
    store: &DbStore,
    executor_id: Uuid,
    limit: u64,
) -> Result<Vec<RootAttachmentChange>> {
    if executor_id.is_nil() {
        return Err(AgentError::Store(
            "root attachment change executor id must not be nil".into(),
        ));
    }
    if !(1..=MAX_PENDING_ROOT_ATTACHMENT_CHANGES).contains(&limit) {
        return Err(AgentError::Store(format!(
            "pending root attachment change limit must be in 1..={MAX_PENDING_ROOT_ATTACHMENT_CHANGES}"
        )));
    }
    let rows = entities::root_attachment_change::Entity::find()
        .filter(entities::root_attachment_change::Column::ExecutorId.eq(executor_id))
        .filter(
            entities::root_attachment_change::Column::Phase
                .eq(phase_to_db(RootAttachmentChangePhase::AwaitingBroker)),
        )
        .order_by_asc(entities::root_attachment_change::Column::CreatedAt)
        .order_by_asc(entities::root_attachment_change::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    let mut changes = Vec::with_capacity(rows.len());
    for row in rows {
        changes.push(change_from_model(row)?);
    }
    if changes.is_empty() {
        return Ok(changes);
    }
    let chat_ids = changes
        .iter()
        .map(|change| change.chat_id.0)
        .collect::<Vec<_>>();
    let chats = entities::chat::Entity::find()
        .filter(entities::chat::Column::Id.is_in(chat_ids))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|chat| (chat.id, chat.project_id))
        .collect::<HashMap<_, _>>();
    for change in &changes {
        let project_id = chats.get(&change.chat_id.0).ok_or_else(|| {
            AgentError::Store(format!(
                "root attachment change {} references missing chat {}",
                change.id, change.chat_id
            ))
        })?;
        validate_change_subject(change, *project_id)?;
    }
    Ok(changes)
}

async fn find_change(
    store: &DbStore,
    id: RootAttachmentChangeId,
) -> Result<Option<RootAttachmentChange>> {
    find_change_on(&store.conn, id).await
}

async fn find_change_on<C>(
    conn: &C,
    id: RootAttachmentChangeId,
) -> Result<Option<RootAttachmentChange>>
where
    C: sea_orm::ConnectionTrait,
{
    let change = entities::root_attachment_change::Entity::find_by_id(*id.as_uuid())
        .one(conn)
        .await
        .map_err(store_err)?
        .map(change_from_model)
        .transpose()?;
    if let Some(change) = &change {
        validate_change_subject_on(conn, change).await?;
    }
    Ok(change)
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
        .map(change_from_model)
        .transpose()?;
    if let Some(busy) = &busy {
        validate_change_subject_on(&store.conn, busy).await?;
    }
    Ok(busy.map(|_| BeginRootAttachmentChangeOutcome::ChatBusy))
}

fn derive_subject(
    chat_id: ChatId,
    project_id: Option<Uuid>,
) -> Result<(RootAttachmentSubjectKind, Uuid)> {
    if let Some(project_id) = project_id {
        if project_id.is_nil() {
            return Err(AgentError::Store(format!(
                "chat {chat_id} has a nil root attachment project subject"
            )));
        }
        Ok((RootAttachmentSubjectKind::Project, project_id))
    } else {
        Ok((RootAttachmentSubjectKind::Conversation, *chat_id.as_uuid()))
    }
}

async fn validate_change_subject_on<C>(conn: &C, change: &RootAttachmentChange) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let chat = entities::chat::Entity::find_by_id(change.chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| {
            AgentError::Store(format!(
                "root attachment change {} references missing chat {}",
                change.id, change.chat_id
            ))
        })?;
    validate_change_subject(change, chat.project_id)
}

fn validate_change_subject(change: &RootAttachmentChange, project_id: Option<Uuid>) -> Result<()> {
    let expected = derive_subject(change.chat_id, project_id)?;
    if (change.subject_kind, change.subject_id) != expected {
        return Err(AgentError::Store(format!(
            "root attachment change {} has authority inconsistent with chat {}",
            change.id, change.chat_id
        )));
    }
    Ok(())
}

async fn load_projection<C>(
    conn: &C,
    chat_id: ChatId,
    revision: i64,
) -> Result<Vec<entities::chat_root_attachment::Model>>
where
    C: sea_orm::ConnectionTrait,
{
    if !(0..=MAX_ATTACHMENT_REVISION).contains(&revision) {
        return Err(AgentError::Store(format!(
            "chat {chat_id} has an invalid attachment revision"
        )));
    }
    let rows = entities::chat_root_attachment::Entity::find()
        .filter(entities::chat_root_attachment::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)?;
    if rows.len() > MAX_ROOT_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "chat {chat_id} exceeds the root attachment limit"
        )));
    }
    for (expected, row) in rows.iter().enumerate() {
        if row.chat_id != chat_id.0 || usize::try_from(row.position).ok() != Some(expected) {
            return Err(AgentError::Store(format!(
                "chat {chat_id} root attachment positions are invalid"
            )));
        }
        HostRootId::from_uuid(row.root_id).map_err(|error| {
            AgentError::Store(format!("chat {chat_id} has an invalid root id: {error}"))
        })?;
        attachment_origin_from_db(&row.origin)?;
    }
    if !rows.is_empty() && revision == 0 {
        return Err(AgentError::Store(format!(
            "chat {chat_id} has roots at attachment revision zero"
        )));
    }
    Ok(rows)
}

fn validate_pending_projection(
    change: &RootAttachmentChange,
    projection: &[entities::chat_root_attachment::Model],
) -> Result<()> {
    let found = projection
        .iter()
        .position(|row| row.root_id == *change.root_id.as_uuid());
    let expected_present =
        change.projection_existed_before || change.action == RootAttachmentChangeAction::Attach;
    if expected_present != found.is_some() {
        return Err(AgentError::Store(format!(
            "root attachment change {} pending projection is inconsistent",
            change.id
        )));
    }
    if let Some(position) = found {
        let expected_position = change
            .projection_position
            .and_then(|value| usize::try_from(value).ok());
        if expected_position != Some(position)
            || change.origin != Some(attachment_origin_from_db(&projection[position].origin)?)
        {
            return Err(AgentError::Store(format!(
                "root attachment change {} pending projection metadata changed",
                change.id
            )));
        }
    }
    Ok(())
}

async fn set_chat_revision<C>(conn: &C, chat_id: ChatId, before: i64, after: i64) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let updated = entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::AttachmentRevision,
            Expr::value(after),
        )
        .filter(entities::chat::Column::Id.eq(chat_id.0))
        .filter(entities::chat::Column::AttachmentRevision.eq(before))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "chat {chat_id} attachment revision changed while locked"
        )));
    }
    Ok(())
}

async fn remove_projection_row<C>(
    conn: &C,
    chat_id: ChatId,
    root_id: HostRootId,
    position: u32,
    before_revision: i64,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let position = i32::try_from(position)
        .map_err(|_| AgentError::Store("root attachment position exceeds i32".into()))?;
    let deleted = entities::chat_root_attachment::Entity::delete_many()
        .filter(entities::chat_root_attachment::Column::ChatId.eq(chat_id.0))
        .filter(entities::chat_root_attachment::Column::RootId.eq(*root_id.as_uuid()))
        .filter(entities::chat_root_attachment::Column::Position.eq(position))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if deleted.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "chat {chat_id} root attachment changed while locked"
        )));
    }
    let trailing = entities::chat_root_attachment::Entity::find()
        .filter(entities::chat_root_attachment::Column::ChatId.eq(chat_id.0))
        .filter(entities::chat_root_attachment::Column::Position.gt(position))
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)?;
    // Compact from the gap upward. Updating one exact row at a time avoids
    // transient unique-position collisions on backends that check uniqueness
    // row-by-row rather than at the end of a multi-row UPDATE.
    for row in trailing {
        let compacted = entities::chat_root_attachment::Entity::update_many()
            .col_expr(
                entities::chat_root_attachment::Column::Position,
                Expr::value(row.position - 1),
            )
            .filter(entities::chat_root_attachment::Column::ChatId.eq(chat_id.0))
            .filter(entities::chat_root_attachment::Column::RootId.eq(row.root_id))
            .filter(entities::chat_root_attachment::Column::Position.eq(row.position))
            .exec(conn)
            .await
            .map_err(store_err)?;
        if compacted.rows_affected != 1 {
            return Err(AgentError::Store(format!(
                "chat {chat_id} root attachment positions changed while locked"
            )));
        }
    }
    set_chat_revision(conn, chat_id, before_revision, before_revision + 1).await
}

async fn persist_terminal_change<C>(conn: &C, change: &RootAttachmentChange) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let updated = entities::root_attachment_change::Entity::update_many()
        .col_expr(
            entities::root_attachment_change::Column::Phase,
            Expr::value(phase_to_db(change.phase)),
        )
        .col_expr(
            entities::root_attachment_change::Column::ResultRevision,
            Expr::value(change.result_revision),
        )
        .col_expr(
            entities::root_attachment_change::Column::ProjectionChanged,
            Expr::value(change.projection_changed),
        )
        .col_expr(
            entities::root_attachment_change::Column::BrokerChanged,
            Expr::value(change.broker_changed),
        )
        .col_expr(
            entities::root_attachment_change::Column::BrokerCurrentlyAttached,
            Expr::value(change.broker_currently_attached),
        )
        .col_expr(
            entities::root_attachment_change::Column::FailureCode,
            Expr::value(change.failure.as_ref().map(|failure| failure.code.clone())),
        )
        .col_expr(
            entities::root_attachment_change::Column::FailureMessage,
            Expr::value(
                change
                    .failure
                    .as_ref()
                    .map(|failure| failure.message.clone()),
            ),
        )
        .col_expr(
            entities::root_attachment_change::Column::FailureRetryable,
            Expr::value(change.failure.as_ref().map(|failure| failure.retryable)),
        )
        .col_expr(
            entities::root_attachment_change::Column::FinishedAt,
            Expr::value(change.finished_at),
        )
        .filter(entities::root_attachment_change::Column::Id.eq(*change.id.as_uuid()))
        .filter(
            entities::root_attachment_change::Column::Phase
                .eq(phase_to_db(RootAttachmentChangePhase::AwaitingBroker)),
        )
        .exec(conn)
        .await
        .map_err(store_err)?;
    if updated.rows_affected != 1 {
        return Err(AgentError::Store(format!(
            "root attachment change {} changed while locked",
            change.id
        )));
    }
    Ok(())
}

fn change_active_model(
    change: &RootAttachmentChange,
) -> entities::root_attachment_change::ActiveModel {
    entities::root_attachment_change::ActiveModel {
        id: Set(*change.id.as_uuid()),
        chat_id: Set(change.chat_id.0),
        subject_kind: Set(subject_kind_to_db(change.subject_kind).into()),
        subject_id: Set(change.subject_id),
        executor_id: Set(change.executor_id),
        root_id: Set(*change.root_id.as_uuid()),
        action: Set(action_to_db(change.action).into()),
        origin: Set(change
            .origin
            .map(|origin| attachment_origin_to_db(origin).into())),
        projection_position: Set(change
            .projection_position
            .map(i64::from)
            .map(|position| i32::try_from(position).expect("bounded projection position"))),
        projection_existed_before: Set(change.projection_existed_before),
        expected_revision: Set(change.expected_revision),
        before_revision: Set(change.before_revision),
        intent_revision: Set(change.intent_revision),
        phase: Set(phase_to_db(change.phase).into()),
        result_revision: Set(change.result_revision),
        projection_changed: Set(change.projection_changed),
        broker_changed: Set(change.broker_changed),
        broker_currently_attached: Set(change.broker_currently_attached),
        failure_code: Set(change.failure.as_ref().map(|failure| failure.code.clone())),
        failure_message: Set(change
            .failure
            .as_ref()
            .map(|failure| failure.message.clone())),
        failure_retryable: Set(change.failure.as_ref().map(|failure| failure.retryable)),
        created_at: Set(change.created_at),
        finished_at: Set(change.finished_at),
    }
}

fn change_from_model(
    model: entities::root_attachment_change::Model,
) -> Result<RootAttachmentChange> {
    let created_at = require_canonical_timestamp("created_at", model.id, model.created_at)?;
    let finished_at = model
        .finished_at
        .map(|value| require_canonical_timestamp("finished_at", model.id, value))
        .transpose()?;
    let failure = match (
        model.failure_code,
        model.failure_message,
        model.failure_retryable,
    ) {
        (None, None, None) => None,
        (Some(code), Some(message), Some(retryable)) => Some(RootAttachmentChangeFailure {
            code,
            message,
            retryable,
        }),
        _ => {
            return Err(AgentError::Store(format!(
                "root attachment change {} has partial failure fields",
                model.id
            )))
        }
    };
    let change = RootAttachmentChange {
        id: RootAttachmentChangeId::from_uuid(model.id).map_err(|error| {
            AgentError::Store(format!("invalid root attachment change id: {error}"))
        })?,
        chat_id: ChatId(model.chat_id),
        executor_id: model.executor_id,
        root_id: HostRootId::from_uuid(model.root_id).map_err(|error| {
            AgentError::Store(format!(
                "root attachment change {} has invalid root id: {error}",
                model.id
            ))
        })?,
        action: action_from_db(&model.action)?,
        subject_kind: subject_kind_from_db(&model.subject_kind)?,
        subject_id: model.subject_id,
        origin: model
            .origin
            .as_deref()
            .map(attachment_origin_from_db)
            .transpose()?,
        projection_position: model
            .projection_position
            .map(|position| {
                u32::try_from(position).map_err(|_| {
                    AgentError::Store(format!(
                        "root attachment change {} has an invalid projection position",
                        model.id
                    ))
                })
            })
            .transpose()?,
        projection_existed_before: model.projection_existed_before,
        expected_revision: model.expected_revision,
        before_revision: model.before_revision,
        intent_revision: model.intent_revision,
        phase: phase_from_db(&model.phase)?,
        result_revision: model.result_revision,
        projection_changed: model.projection_changed,
        broker_changed: model.broker_changed,
        broker_currently_attached: model.broker_currently_attached,
        failure,
        created_at,
        finished_at,
    };
    change.validate().map_err(|message| {
        AgentError::Store(format!(
            "invalid root attachment change {}: {message}",
            change.id
        ))
    })?;
    Ok(change)
}

fn require_canonical_timestamp(
    field: &str,
    id: Uuid,
    value: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    let canonical = canonical_db_timestamp(value)?;
    if canonical != value {
        return Err(AgentError::Store(format!(
            "root attachment change {id} has noncanonical {field}"
        )));
    }
    Ok(canonical)
}

fn terminal_matches(
    change: &RootAttachmentChange,
    terminal: &RootAttachmentChangeTerminal,
) -> bool {
    match terminal {
        RootAttachmentChangeTerminal::Completed {
            broker_changed,
            broker_currently_attached,
        } => {
            change.phase == RootAttachmentChangePhase::Completed
                && change.broker_changed == Some(*broker_changed)
                && change.broker_currently_attached == Some(*broker_currently_attached)
                && change.failure.is_none()
        }
        RootAttachmentChangeTerminal::Failed {
            broker_changed,
            broker_currently_attached,
            failure,
        } => {
            change.phase == RootAttachmentChangePhase::Failed
                && change.broker_changed == *broker_changed
                && change.broker_currently_attached == *broker_currently_attached
                && change.failure.as_ref() == Some(failure)
        }
    }
}

fn desired_state(change: &RootAttachmentChange) -> bool {
    change.action == RootAttachmentChangeAction::Attach
}

fn action_to_db(action: RootAttachmentChangeAction) -> &'static str {
    match action {
        RootAttachmentChangeAction::Attach => "attach",
        RootAttachmentChangeAction::Detach => "detach",
    }
}

fn action_from_db(value: &str) -> Result<RootAttachmentChangeAction> {
    match value {
        "attach" => Ok(RootAttachmentChangeAction::Attach),
        "detach" => Ok(RootAttachmentChangeAction::Detach),
        other => Err(AgentError::Store(format!(
            "unknown root attachment action: {other}"
        ))),
    }
}

fn subject_kind_to_db(kind: RootAttachmentSubjectKind) -> &'static str {
    match kind {
        RootAttachmentSubjectKind::Project => "project",
        RootAttachmentSubjectKind::Conversation => "conversation",
    }
}

fn subject_kind_from_db(value: &str) -> Result<RootAttachmentSubjectKind> {
    match value {
        "project" => Ok(RootAttachmentSubjectKind::Project),
        "conversation" => Ok(RootAttachmentSubjectKind::Conversation),
        other => Err(AgentError::Store(format!(
            "unknown root attachment subject kind: {other}"
        ))),
    }
}

fn phase_to_db(phase: RootAttachmentChangePhase) -> &'static str {
    match phase {
        RootAttachmentChangePhase::AwaitingBroker => "awaiting_broker",
        RootAttachmentChangePhase::Completed => "completed",
        RootAttachmentChangePhase::Failed => "failed",
    }
}

fn phase_from_db(value: &str) -> Result<RootAttachmentChangePhase> {
    match value {
        "awaiting_broker" => Ok(RootAttachmentChangePhase::AwaitingBroker),
        "completed" => Ok(RootAttachmentChangePhase::Completed),
        "failed" => Ok(RootAttachmentChangePhase::Failed),
        other => Err(AgentError::Store(format!(
            "unknown root attachment change phase: {other}"
        ))),
    }
}
