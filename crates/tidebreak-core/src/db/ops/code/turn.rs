use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::code::{CodeSessionId, CodeTurn, CodeTurnId, CodeTurnStatus, CodeUsage, Diffstat};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};

/// Insert a turn row.
pub async fn insert_turn(store: &DbStore, turn: &CodeTurn) -> Result<()> {
    entities::code_turn::ActiveModel {
        id: Set(turn.id.0),
        session_id: Set(turn.session_id.0),
        ordinal: Set(turn.ordinal),
        status: Set(turn.status.as_str().to_owned()),
        user_input: Set(turn.user_input.clone()),
        user_input_blob_id: Set(turn.user_input_blob_id),
        checkpoint_ref: Set(turn.checkpoint_ref.clone()),
        diffstat: Set(match &turn.diffstat {
            Some(stat) => Some(serde_json::to_value(stat)?),
            None => None,
        }),
        usage: Set(match &turn.usage {
            Some(usage) => Some(serde_json::to_value(usage)?),
            None => None,
        }),
        narrative: Set(turn.narrative.clone()),
        started_at: Set(turn.started_at),
        ended_at: Set(turn.ended_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load a turn by id.
pub async fn get_turn(store: &DbStore, id: CodeTurnId) -> Result<Option<CodeTurn>> {
    let Some(row) = entities::code_turn::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(turn_from_row(row)?))
}

pub(super) fn turn_from_row(row: entities::code_turn::Model) -> Result<CodeTurn> {
    let status = CodeTurnStatus::from_str(&row.status).ok_or_else(|| {
        AgentError::Store(format!(
            "code_turn {} has unknown status {}",
            row.id, row.status
        ))
    })?;
    let diffstat =
        match row.diffstat {
            Some(value) => Some(serde_json::from_value::<Diffstat>(value).map_err(|err| {
                AgentError::Store(format!("code_turn {} diffstat: {err}", row.id))
            })?),
            None => None,
        };
    let usage = match row.usage {
        Some(value) => Some(
            serde_json::from_value::<CodeUsage>(value)
                .map_err(|err| AgentError::Store(format!("code_turn {} usage: {err}", row.id)))?,
        ),
        None => None,
    };
    Ok(CodeTurn {
        id: CodeTurnId(row.id),
        session_id: CodeSessionId(row.session_id),
        ordinal: row.ordinal,
        status,
        user_input: row.user_input,
        user_input_blob_id: row.user_input_blob_id,
        checkpoint_ref: row.checkpoint_ref,
        diffstat,
        usage,
        narrative: row.narrative,
        started_at: row.started_at,
        ended_at: row.ended_at,
    })
}
