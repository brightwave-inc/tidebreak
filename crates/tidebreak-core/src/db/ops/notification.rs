//! Durable log of top-level agent turns that settled without cancel.

use sea_orm::sea_query::ExprTrait;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};

use crate::code::{Event, WorkspaceId};
use crate::error::{provider_failure_detail, AgentError, AgentErrorInfo, Result};
use crate::id::{NotificationId, SessionId, TurnId};
use crate::model::OwnerId;
use crate::storage::{
    code_notification_dedupe_key, notification_body_line, notification_title,
    work_notification_dedupe_key, Notification, NotificationContext, NotificationKind,
    NotificationListCursor,
};

use super::super::{entities, store_err, DbStore};

const MAX_PAGE: u64 = 100;

/// How far back a Code turn's closing message is looked for. A turn that
/// journals more events than this after its closing message is rare, and its
/// notification goes without a body.
const CLOSING_MESSAGE_EVENT_WINDOW: u64 = 200;

/// Insert one Work turn settlement. Idempotent on `(owner, dedupe_key)`.
pub(in crate::db) async fn record_work_turn_notification(
    store: &DbStore,
    chat_id: SessionId,
    turn_id: TurnId,
    kind: NotificationKind,
) -> Result<Option<Notification>> {
    record_work_turn_notification_on(&store.conn, chat_id, turn_id, kind, None).await
}

/// Insert one Work turn settlement on the caller's transaction.
///
/// `failure` is the error a failed turn ended with. The body shows it only
/// when the provider reported it, the same rule the transcript follows, so a
/// banner never says more than the conversation does.
pub(in crate::db) async fn record_work_turn_notification_on<C>(
    conn: &C,
    chat_id: SessionId,
    turn_id: TurnId,
    kind: NotificationKind,
    failure: Option<&AgentErrorInfo>,
) -> Result<Option<Notification>>
where
    C: ConnectionTrait,
{
    let Some(chat) = entities::session::Entity::find_by_id(chat_id.0)
        .one(conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let owner = OwnerId::new(&chat.owner)?;
    let body = match kind {
        NotificationKind::AgentCompleted => work_closing_message_on(conn, chat_id, turn_id)
            .await?
            .as_deref()
            .and_then(notification_body_line),
        NotificationKind::AgentFailed => failure
            .and_then(|failure| provider_failure_detail(&failure.kind, &failure.message))
            .as_deref()
            .and_then(notification_body_line),
    };
    insert_notification_on(
        conn,
        &owner,
        kind,
        notification_title(chat.title.as_deref(), kind),
        body,
        NotificationContext::Chat { chat_id },
        work_notification_dedupe_key(kind, chat_id, turn_id),
    )
    .await
    .map(Some)
}

/// The assistant message a Work turn ended on. Completion writes it earlier
/// in the same transaction as the terminal event.
async fn work_closing_message_on<C>(
    conn: &C,
    chat_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<String>>
where
    C: ConnectionTrait,
{
    Ok(entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .filter(entities::message::Column::TurnId.eq(turn_id.0))
        .filter(entities::message::Column::Role.eq("assistant"))
        .order_by_desc(entities::message::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(|message| message.content))
}

/// Insert one Code turn settlement. Idempotent on `(owner, dedupe_key)`.
#[cfg(test)]
pub(in crate::db) async fn record_code_turn_notification(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    turn_id: TurnId,
    workspace_title: Option<&str>,
    kind: NotificationKind,
) -> Result<Notification> {
    record_code_turn_notification_on(
        &store.conn,
        owner,
        session_id,
        workspace_id,
        turn_id,
        workspace_title,
        kind,
        None,
    )
    .await
}

/// Insert one Code turn settlement on the caller's transaction.
///
/// `failure` is the bounded message a failed turn ended with: the same text
/// the turn's review card shows.
#[allow(clippy::too_many_arguments)]
pub(in crate::db) async fn record_code_turn_notification_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
    workspace_id: WorkspaceId,
    turn_id: TurnId,
    workspace_title: Option<&str>,
    kind: NotificationKind,
    failure: Option<&str>,
) -> Result<Notification>
where
    C: ConnectionTrait,
{
    let body = match kind {
        NotificationKind::AgentCompleted => {
            code_closing_message_on(conn, owner, session_id, turn_id)
                .await?
                .as_deref()
                .and_then(notification_body_line)
        }
        NotificationKind::AgentFailed => failure.and_then(notification_body_line),
    };
    insert_notification_on(
        conn,
        owner,
        kind,
        notification_title(workspace_title.or(Some("Code")), kind),
        body,
        NotificationContext::Code {
            session_id,
            workspace_id,
        },
        code_notification_dedupe_key(kind, session_id, turn_id),
    )
    .await
}

/// The engine's closing message for `turn_id`: its last top-level assistant
/// message, read back from the journal to the turn's start.
///
/// A subagent's messages answer the parent, not you, so they are skipped. So
/// is a row that does not decode as a Code event: a body is not worth failing
/// the terminal append that mints this notification.
async fn code_closing_message_on<C>(
    conn: &C,
    owner: &OwnerId,
    session_id: SessionId,
    turn_id: TurnId,
) -> Result<Option<String>>
where
    C: ConnectionTrait,
{
    let rows = entities::event::Entity::find()
        .filter(entities::event::Column::Owner.eq(owner.as_str()))
        .filter(entities::event::Column::SessionId.eq(session_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .limit(CLOSING_MESSAGE_EVENT_WINDOW)
        .all(conn)
        .await
        .map_err(store_err)?;
    for row in rows {
        match serde_json::from_value::<Event>(row.event) {
            Ok(Event::TurnStarted { turn_id: started }) if started == turn_id => break,
            Ok(Event::AssistantMessage {
                text,
                parent_call_id: None,
            }) => return Ok(Some(text)),
            _ => {}
        }
    }
    Ok(None)
}

async fn insert_notification_on<C>(
    conn: &C,
    owner: &OwnerId,
    kind: NotificationKind,
    title: String,
    body: Option<String>,
    context: NotificationContext,
    dedupe_key: String,
) -> Result<Notification>
where
    C: ConnectionTrait,
{
    let id = NotificationId::new();
    let created_at = chrono::Utc::now();
    let context_json = serde_json::to_value(&context)?;
    entities::notification::Entity::insert(entities::notification::ActiveModel {
        id: Set(id.0),
        owner: Set(owner.as_str().to_owned()),
        kind: Set(kind.as_str().to_owned()),
        title: Set(title),
        body: Set(body),
        context: Set(context_json),
        dedupe_key: Set(dedupe_key.clone()),
        created_at: Set(created_at),
        read_at: Set(None),
    })
    .on_conflict(
        OnConflict::columns([
            entities::notification::Column::Owner,
            entities::notification::Column::DedupeKey,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(conn)
    .await
    .map_err(store_err)?;

    let row = entities::notification::Entity::find()
        .filter(entities::notification::Column::Owner.eq(owner.as_str()))
        .filter(entities::notification::Column::DedupeKey.eq(dedupe_key))
        .one(conn)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("notification insert did not persist".into()))?;
    notification_from_row(row)
}

/// Newest-first page for one owner.
pub(in crate::db) async fn list_notifications(
    store: &DbStore,
    owner: &OwnerId,
    cursor: Option<NotificationListCursor>,
    limit: u64,
) -> Result<Vec<Notification>> {
    let limit = limit.clamp(1, MAX_PAGE);
    let mut query = entities::notification::Entity::find()
        .filter(entities::notification::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::notification::Column::CreatedAt)
        .order_by_desc(entities::notification::Column::Id)
        .limit(limit);
    if let Some(cursor) = cursor {
        query = query.filter(
            Expr::tuple([
                Expr::col(entities::notification::Column::CreatedAt),
                Expr::col(entities::notification::Column::Id),
            ])
            .lt(Expr::tuple([cursor.created_at.into(), cursor.id.0.into()])),
        );
    }
    query
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(notification_from_row)
        .collect()
}

/// Unread rows for one owner.
pub(in crate::db) async fn unread_notification_count(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<u64> {
    let count = entities::notification::Entity::find()
        .filter(entities::notification::Column::Owner.eq(owner.as_str()))
        .filter(entities::notification::Column::ReadAt.is_null())
        .count(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(count)
}

/// Mark the given ids read. Other owners' rows are ignored.
pub(in crate::db) async fn mark_notifications_read(
    store: &DbStore,
    owner: &OwnerId,
    ids: &[NotificationId],
    read_at: chrono::DateTime<chrono::Utc>,
) -> Result<u64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let result = entities::notification::Entity::update_many()
        .col_expr(entities::notification::Column::ReadAt, Expr::value(read_at))
        .filter(entities::notification::Column::Owner.eq(owner.as_str()))
        .filter(entities::notification::Column::Id.is_in(ids.iter().map(|id| id.0)))
        .filter(entities::notification::Column::ReadAt.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected)
}

pub(in crate::db) async fn mark_all_notifications_read(
    store: &DbStore,
    owner: &OwnerId,
    read_at: chrono::DateTime<chrono::Utc>,
) -> Result<u64> {
    let result = entities::notification::Entity::update_many()
        .col_expr(entities::notification::Column::ReadAt, Expr::value(read_at))
        .filter(entities::notification::Column::Owner.eq(owner.as_str()))
        .filter(entities::notification::Column::ReadAt.is_null())
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected)
}

fn notification_from_row(row: entities::notification::Model) -> Result<Notification> {
    let kind = NotificationKind::from_storage_str(&row.kind).ok_or_else(|| {
        AgentError::Store(format!(
            "notification {} has unknown kind {}",
            row.id, row.kind
        ))
    })?;
    let context = serde_json::from_value(row.context).map_err(|error| {
        AgentError::Store(format!(
            "notification {} has invalid context: {error}",
            row.id
        ))
    })?;
    Ok(Notification {
        id: NotificationId(row.id),
        kind,
        title: row.title,
        body: row.body,
        context,
        created_at: row.created_at,
        read_at: row.read_at,
    })
}
