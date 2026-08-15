use sea_orm::{ActiveModelTrait, EntityTrait, Set};

use crate::code::{
    CodeApproval, CodeApprovalId, CodeApprovalKind, CodeApprovalState, CodeSessionId, CodeTurnId,
};
use crate::error::{AgentError, Result};

use super::super::super::{entities, store_err, DbStore};

/// Insert an approval row.
pub async fn insert_approval(store: &DbStore, approval: &CodeApproval) -> Result<()> {
    entities::code_approval::ActiveModel {
        id: Set(approval.id.0),
        session_id: Set(approval.session_id.0),
        turn_id: Set(approval.turn_id.0),
        kind: Set(serde_json::to_value(&approval.kind)?),
        harness_raw: Set(approval.harness_raw.clone()),
        state: Set(approval.state.as_str().to_owned()),
        feedback: Set(approval.feedback.clone()),
        requested_at: Set(approval.requested_at),
        decided_at: Set(approval.decided_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Load an approval by id.
pub async fn get_approval(store: &DbStore, id: CodeApprovalId) -> Result<Option<CodeApproval>> {
    let Some(row) = entities::code_approval::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(approval_from_row(row)?))
}

pub(super) fn approval_from_row(row: entities::code_approval::Model) -> Result<CodeApproval> {
    let state = CodeApprovalState::from_str(&row.state).ok_or_else(|| {
        AgentError::Store(format!(
            "code_approval {} has unknown state {}",
            row.id, row.state
        ))
    })?;
    let kind = serde_json::from_value::<CodeApprovalKind>(row.kind)
        .map_err(|err| AgentError::Store(format!("code_approval {} kind: {err}", row.id)))?;
    Ok(CodeApproval {
        id: CodeApprovalId(row.id),
        session_id: CodeSessionId(row.session_id),
        turn_id: CodeTurnId(row.turn_id),
        kind,
        harness_raw: row.harness_raw,
        state,
        feedback: row.feedback,
        requested_at: row.requested_at,
        decided_at: row.decided_at,
    })
}
