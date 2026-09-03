//! Request validation for turn identity. The admission ledger is retired;
//! identity lives on `code_turn.fingerprint`.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::error::{AgentError, Result};
use crate::model::{TurnAdmissionLease, TurnAdmissionRequest, TurnRun, TurnRunStatus};
use crate::storage::BeginTurnAdmissionOutcome;

use super::super::super::{entities, store_err, DbStore};

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
    store: &DbStore,
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
    // An exact retry must not consult the model catalog. The fingerprint is
    // the caller request; if it already committed, the route returns Accepted
    // before it tries to resolve a provider that may have gone away.
    if let Some(existing) = entities::code_turn::Entity::find_by_id(request.id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    {
        if existing.session_id != request.chat_id.0
            || existing.fingerprint.as_deref() != Some(request.fingerprint().as_slice())
        {
            return Ok(BeginTurnAdmissionOutcome::IdentityConflict);
        }
        return Ok(if existing.status == TurnRunStatus::Queued.as_str() {
            BeginTurnAdmissionOutcome::Queued
        } else {
            BeginTurnAdmissionOutcome::Accepted
        });
    }
    if entities::code_queued_turn::Entity::find_by_id(request.id.0)
        .filter(entities::code_queued_turn::Column::SessionId.eq(request.chat_id.0))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Ok(BeginTurnAdmissionOutcome::Queued);
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
