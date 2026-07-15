use std::collections::HashMap;

use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::id::{ChatId, RootAttachmentChangeId};
use crate::model::{RootAttachmentChange, RootAttachmentChangePhase, RootAttachmentSubjectKind};
use crate::storage::MAX_PENDING_ROOT_ATTACHMENT_CHANGES;

use super::super::super::{entities, store_err, DbStore};
use super::codec::{change_from_model, phase_to_db};

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

pub(super) async fn find_change(
    store: &DbStore,
    id: RootAttachmentChangeId,
) -> Result<Option<RootAttachmentChange>> {
    find_change_on(&store.conn, id).await
}

pub(super) async fn find_change_on<C>(
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

pub(super) fn derive_subject(
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

pub(super) async fn validate_change_subject_on<C>(
    conn: &C,
    change: &RootAttachmentChange,
) -> Result<()>
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

pub(super) async fn persist_terminal_change<C>(
    conn: &C,
    change: &RootAttachmentChange,
) -> Result<()>
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
