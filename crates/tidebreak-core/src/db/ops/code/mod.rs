//! Persistence for the external agent-engine domain.
//!
//! Isolated from the chat [`crate::storage::Store`] trait: these tables never
//! reference chat ids, and chat tables never reference these.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::code::CodeSessionId;
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::{entities, store_err};

pub mod approval;
pub mod journal;
pub mod pull_request;
pub mod queued;
pub mod recovery;
pub mod repo;
pub mod session;
mod session_image;
mod transcript_search;
pub mod trigger;
pub mod turn;
pub mod watch;
pub mod workflow_run;
pub mod workspace;

pub use approval::*;
pub use journal::*;
pub use pull_request::*;
pub use queued::*;
pub use recovery::*;
pub use repo::*;
pub use session::*;
pub use session_image::*;
pub use transcript_search::*;
pub use trigger::*;
pub use turn::*;
pub use watch::*;
pub use workflow_run::*;
pub use workspace::*;

/// Insert a Code notification when an interactive session turn settles.
pub async fn record_code_turn_notification(
    store: &super::super::DbStore,
    owner: &OwnerId,
    session_id: crate::code::CodeSessionId,
    workspace_id: crate::code::WorkspaceId,
    turn_id: crate::code::CodeTurnId,
    workspace_title: Option<&str>,
    kind: crate::NotificationKind,
) -> crate::error::Result<crate::Notification> {
    super::notification::record_code_turn_notification(
        store,
        owner,
        session_id,
        workspace_id,
        turn_id,
        workspace_title,
        kind,
    )
    .await
}

/// Typed journal-append failure. A stale spawn epoch is a distinct variant so
/// a superseded worker cannot be mistaken for a generic store error.
#[derive(Debug, thiserror::Error)]
pub enum CodeJournalError {
    /// The session row is gone.
    #[error("code session {session_id} not found")]
    SessionNotFound {
        /// Missing session.
        session_id: CodeSessionId,
    },
    /// The writer’s spawn epoch does not match the session row.
    #[error(
        "stale spawn epoch for session {session_id}: attempted {attempted}, current {current}"
    )]
    StaleSpawnEpoch {
        /// Session the write targeted.
        session_id: CodeSessionId,
        /// Epoch the writer still believes it owns.
        attempted: i64,
        /// Epoch currently on the session row.
        current: i64,
    },
    /// Underlying store failure.
    #[error(transparent)]
    Store(#[from] AgentError),
}

/// Acquire the shared write lock for one code-session row.
///
/// Sequence allocation and epoch checks take this lock so independently
/// running workers cannot race a journal append with a spawn-epoch bump.
pub(in crate::db) async fn acquire_code_session_write_lock<C>(
    conn: &C,
    session_id: CodeSessionId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::code_session::Entity::update_many()
        .col_expr(
            entities::code_session::Column::UnrecognizedEventCount,
            sea_orm::sea_query::Expr::col(entities::code_session::Column::UnrecognizedEventCount),
        )
        .filter(entities::code_session::Column::Id.eq(session_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}
