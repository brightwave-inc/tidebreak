use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::error::{AgentError, Result};
use crate::id::{CallId, ChatId, ProjectId, TurnId};

use super::{entities, store_err};

pub(in crate::db) mod active_work;
pub(in crate::db) mod agent_run;
pub(in crate::db) mod app;
pub(in crate::db) mod approval;
pub(in crate::db) mod blob;
pub(in crate::db) mod chat_prompt;
pub(in crate::db) mod citation;
pub(in crate::db) mod client_execution;
pub(in crate::db) mod connected_app;
pub(in crate::db) mod context_checkpoint;
pub(in crate::db) mod conversation;
pub(in crate::db) mod document;
pub(in crate::db) mod exec_file_rejection;
pub(in crate::db) mod exec_file_snapshot;
pub(in crate::db) mod message_attachment;
pub(in crate::db) mod message_document_attachment;
pub(in crate::db) mod operation_log;
pub(in crate::db) mod output;
pub(in crate::db) mod plan;
pub(in crate::db) mod root_attachment;
pub(in crate::db) mod sandbox_provision;
pub(in crate::db) mod sandbox_tool;
pub(in crate::db) mod turn;
pub(in crate::db) mod user_question;

/// Acquire the shared cross-backend write lock for one chat row.
///
/// Turn admission and per-chat event sequence allocation use the same lock so
/// their read-then-write decisions remain serialized across server processes.
pub(in crate::db) async fn acquire_chat_write_lock<C>(conn: &C, chat_id: ChatId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::Title,
            sea_orm::sea_query::Expr::col(entities::chat::Column::Title),
        )
        .filter(entities::chat::Column::Id.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

/// Acquire the shared cross-backend write lock for one project row.
///
/// Project-scoped child insertion and deletion take this same lock so an empty
/// project cannot gain a conversation or document between its emptiness check
/// and deletion.
pub(in crate::db) async fn acquire_project_write_lock<C>(
    conn: &C,
    project_id: ProjectId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::project::Entity::update_many()
        .col_expr(
            entities::project::Column::Title,
            sea_orm::sea_query::Expr::col(entities::project::Column::Title),
        )
        .filter(entities::project::Column::Id.eq(project_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

/// Fence a project-scoped child write and report a concurrent deletion with a
/// typed error that product adapters can map without parsing database text.
pub(in crate::db) async fn require_project_write_lock<C>(
    conn: &C,
    project_id: Option<ProjectId>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if let Some(project_id) = project_id {
        if !acquire_project_write_lock(conn, project_id).await? {
            return Err(AgentError::ProjectNotFound(project_id));
        }
    }
    Ok(())
}

/// Fence creation or replacement of a document inside exactly one optional
/// corpus. Legacy rows may be unscoped; new conversation routes provide only a
/// chat id. A document may never simultaneously belong to a chat and project.
pub(in crate::db) async fn require_document_scope_write_lock<C>(
    conn: &C,
    chat_id: Option<ChatId>,
    project_id: Option<ProjectId>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    if chat_id.is_some() && project_id.is_some() {
        return Err(AgentError::Store(
            "document cannot belong to both a conversation and a project".into(),
        ));
    }
    if let Some(chat_id) = chat_id {
        if !acquire_chat_write_lock(conn, chat_id).await? {
            return Err(AgentError::Store(format!("chat {chat_id} not found")));
        }
    }
    require_project_write_lock(conn, project_id).await
}

/// Allocate the next stable tool-history position while the caller owns the
/// chat write lock.
pub(in crate::db) async fn next_tool_history_order_on<C>(conn: &C, chat_id: ChatId) -> Result<i64>
where
    C: ConnectionTrait,
{
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::tool_call::Column::HistoryOrder)
        .one(conn)
        .await
        .map_err(store_err)?
        .map_or(Some(1), |call| call.history_order.checked_add(1))
        .ok_or_else(|| AgentError::Store(format!("tool history exhausted for chat {chat_id}")))
}

/// Acquire the cross-backend write lock for one tool-call row.
pub(in crate::db) async fn acquire_tool_call_write_lock<C>(conn: &C, id: CallId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::tool_call::Entity::update_many()
        .col_expr(
            entities::tool_call::Column::ResolvedAt,
            sea_orm::sea_query::Expr::col(entities::tool_call::Column::ResolvedAt),
        )
        .filter(entities::tool_call::Column::Id.eq(id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

/// Acquire the shared cross-backend write lock for one durable turn row.
pub(in crate::db) async fn acquire_turn_write_lock<C>(conn: &C, turn_id: TurnId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::turn_run::Entity::update_many()
        .col_expr(
            entities::turn_run::Column::UpdatedAt,
            sea_orm::sea_query::Expr::col(entities::turn_run::Column::UpdatedAt),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}
