//! Durable log of agents that finished. Inbox stays the queue of work
//! waiting on you.

use serde::{Deserialize, Serialize};
use tidebreak_core::{
    Notification, NotificationContext, NotificationId, NotificationKind, NotificationListCursor,
    SessionId, WorkspaceId,
};

use crate::error::ServerError;
use crate::extract::{Json, Query};
use crate::scoped_store::ScopedStore;

const DEFAULT_PAGE: u64 = 50;
const MAX_PAGE: u64 = 100;

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKindSnapshot {
    AgentCompleted,
    AgentFailed,
}

impl From<NotificationKind> for NotificationKindSnapshot {
    fn from(kind: NotificationKind) -> Self {
        match kind {
            NotificationKind::AgentCompleted => Self::AgentCompleted,
            NotificationKind::AgentFailed => Self::AgentFailed,
        }
    }
}

/// Where opening the row takes the reader.
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum NotificationContextSnapshot {
    Chat {
        chat_id: SessionId,
    },
    Code {
        session_id: SessionId,
        workspace_id: WorkspaceId,
    },
}

impl From<NotificationContext> for NotificationContextSnapshot {
    fn from(context: NotificationContext) -> Self {
        match context {
            NotificationContext::Chat { chat_id } => Self::Chat { chat_id },
            NotificationContext::Code {
                session_id,
                workspace_id,
            } => Self::Code {
                session_id,
                workspace_id,
            },
        }
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct NotificationSnapshot {
    pub id: NotificationId,
    pub kind: NotificationKindSnapshot,
    pub title: String,
    pub context: NotificationContextSnapshot,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub read_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<Notification> for NotificationSnapshot {
    fn from(row: Notification) -> Self {
        Self {
            id: row.id,
            kind: row.kind.into(),
            title: row.title,
            context: row.context.into(),
            created_at: row.created_at,
            read_at: row.read_at,
        }
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct NotificationPage {
    pub notifications: Vec<NotificationSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct NotificationUnreadCount {
    pub unread: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct NotificationListQuery {
    pub limit: Option<u64>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarkNotificationsReadBody {
    pub ids: Vec<NotificationId>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct MarkNotificationsReadResult {
    pub marked: u64,
}

fn encode_cursor(cursor: NotificationListCursor) -> String {
    format!(
        "{}:{:09}:{}",
        cursor.created_at.timestamp(),
        cursor.created_at.timestamp_subsec_nanos(),
        cursor.id
    )
}

fn decode_cursor(raw: &str) -> Result<NotificationListCursor, ServerError> {
    let mut parts = raw.splitn(3, ':');
    let seconds = parts.next().and_then(|part| part.parse::<i64>().ok());
    let nanos = parts.next().and_then(|part| part.parse::<u32>().ok());
    let id = parts.next();
    let created_at = seconds
        .zip(nanos)
        .and_then(|(seconds, nanos)| chrono::DateTime::from_timestamp(seconds, nanos))
        .ok_or_else(|| ServerError::bad_request("invalid notification cursor"))?;
    let id = id
        .ok_or_else(|| ServerError::bad_request("invalid notification cursor"))?
        .parse()
        .map_err(|_| ServerError::bad_request("invalid notification cursor"))?;
    Ok(NotificationListCursor { created_at, id })
}

/// `GET /notifications` — newest first.
pub async fn list_notifications(
    store: ScopedStore,
    Query(query): Query<NotificationListQuery>,
) -> Result<Json<NotificationPage>, ServerError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let cursor = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let mut rows = store.list_notifications(cursor, limit + 1).await?;
    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.pop();
    }
    let next_cursor = has_more
        .then(|| {
            rows.last().map(|row| {
                encode_cursor(NotificationListCursor {
                    created_at: row.created_at,
                    id: row.id,
                })
            })
        })
        .flatten();
    Ok(Json(NotificationPage {
        notifications: rows.into_iter().map(NotificationSnapshot::from).collect(),
        next_cursor,
    }))
}

/// `GET /notifications/unread-count`
pub async fn unread_notification_count(
    store: ScopedStore,
) -> Result<Json<NotificationUnreadCount>, ServerError> {
    Ok(Json(NotificationUnreadCount {
        unread: store.unread_notification_count().await?,
    }))
}

/// `POST /notifications/read`
pub async fn mark_notifications_read(
    store: ScopedStore,
    Json(body): Json<MarkNotificationsReadBody>,
) -> Result<Json<MarkNotificationsReadResult>, ServerError> {
    let marked = store
        .mark_notifications_read(&body.ids, chrono::Utc::now())
        .await?;
    Ok(Json(MarkNotificationsReadResult { marked }))
}

/// `POST /notifications/read-all`
pub async fn mark_all_notifications_read(
    store: ScopedStore,
) -> Result<Json<MarkNotificationsReadResult>, ServerError> {
    let marked = store
        .mark_all_notifications_read(chrono::Utc::now())
        .await?;
    Ok(Json(MarkNotificationsReadResult { marked }))
}
