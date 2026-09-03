use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::error::{AgentError, Result};
use crate::id::{HostRootId, SessionId};
use crate::model::{
    RootAttachmentChange, RootAttachmentChangeAction, MAX_ATTACHMENT_REVISION, MAX_ROOT_ATTACHMENTS,
};

use super::super::super::{entities, store_err};
use super::super::conversation::attachment_origin_from_db;

pub(super) async fn load_projection<C>(
    conn: &C,
    chat_id: SessionId,
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

pub(super) fn validate_pending_projection(
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

pub(super) async fn set_chat_revision<C>(
    conn: &C,
    chat_id: SessionId,
    before: i64,
    after: i64,
) -> Result<()>
where
    C: sea_orm::ConnectionTrait,
{
    let updated = entities::session::Entity::update_many()
        .col_expr(
            entities::session::Column::AttachmentRevision,
            Expr::value(after),
        )
        .filter(entities::session::Column::Id.eq(chat_id.0))
        .filter(entities::session::Column::AttachmentRevision.eq(before))
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

pub(super) async fn remove_projection_row<C>(
    conn: &C,
    chat_id: SessionId,
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

pub(super) fn desired_state(change: &RootAttachmentChange) -> bool {
    change.action == RootAttachmentChangeAction::Attach
}
