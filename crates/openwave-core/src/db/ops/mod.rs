use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::error::Result;
use crate::id::{CallId, ChatId, TurnId};

use super::{entities, store_err};

pub(in crate::db) mod agent_run;
pub(in crate::db) mod blob;
pub(in crate::db) mod client_execution;
pub(in crate::db) mod conversation;
pub(in crate::db) mod document;
pub(in crate::db) mod root_attachment;
pub(in crate::db) mod turn;

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
            sea_orm::sea_query::Expr::col(entities::chat::Column::Title).into(),
        )
        .filter(entities::chat::Column::Id.eq(chat_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

/// Acquire the cross-backend write lock for one tool-call row.
pub(in crate::db) async fn acquire_tool_call_write_lock<C>(conn: &C, id: CallId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::tool_call::Entity::update_many()
        .col_expr(
            entities::tool_call::Column::ResolvedAt,
            sea_orm::sea_query::Expr::col(entities::tool_call::Column::ResolvedAt).into(),
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
            sea_orm::sea_query::Expr::col(entities::turn_run::Column::UpdatedAt).into(),
        )
        .filter(entities::turn_run::Column::Id.eq(turn_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}
