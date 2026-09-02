//! Request validation for turn identity. The admission ledger is retired;
//! identity lives on `code_turn.fingerprint`.

use crate::error::{AgentError, Result};
use crate::model::{TurnAdmissionLease, TurnAdmissionRequest, TurnRun};
use crate::storage::BeginTurnAdmissionOutcome;

use super::super::super::DbStore;

pub(super) fn validate_request(request: &TurnAdmissionRequest) -> Result<()> {
    if request.id.0.is_nil()
        || request.content.trim().is_empty()
        || request.content.contains('\0')
        || request
            .attachments
            .len()
            .saturating_add(request.file_attachments.len())
            > crate::MAX_MESSAGE_ATTACHMENTS
        || request.invoked_skills.len() > TurnRun::MAX_INVOKED_SKILLS
    {
        return Err(AgentError::Store("invalid turn admission request".into()));
    }
    let mut distinct = std::collections::HashSet::with_capacity(request.invoked_skills.len());
    if request.invoked_skills.iter().any(|skill| {
        skill.is_empty()
            || skill.len() > TurnRun::MAX_INVOKED_SKILL_NAME_LEN
            || !distinct.insert(skill.as_str())
    }) {
        return Err(AgentError::Store(
            "invalid invoked skill identity in turn admission".into(),
        ));
    }
    Ok(())
}

pub(in crate::db) async fn begin(
    _store: &DbStore,
    request: &TurnAdmissionRequest,
    lease_token: uuid::Uuid,
    lease_ttl: chrono::Duration,
) -> Result<BeginTurnAdmissionOutcome> {
    validate_request(request)?;
    if lease_token.is_nil() || lease_ttl <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "turn admission requires a non-nil token and a positive lease".into(),
        ));
    }
    Ok(BeginTurnAdmissionOutcome::Acquired(TurnAdmissionLease {
        id: request.id,
        lease_token,
        lease_expires_at: chrono::Utc::now() + lease_ttl,
    }))
}

pub(in crate::db) async fn release(_store: &DbStore, _lease: TurnAdmissionLease) -> Result<bool> {
    Ok(true)
}
