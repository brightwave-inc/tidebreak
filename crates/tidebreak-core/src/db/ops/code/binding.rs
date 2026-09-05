//! External conversation bindings (docs/slack-sessions.md, stage 2).
//!
//! One row per external conversation. The unique key on
//! `(owner, channel_kind, external_key)` is the race gate: two get-or-creates
//! for one conversation cannot both commit their session, so first contact
//! converges on one session no matter how many times the channel retries.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

use crate::code::{
    CodeBindingId, CodeExternalBinding, CodeGrantId, CodeWorkspace, ExternalSessionResolution,
    Session, SessionId, SessionLifecycle,
};
use crate::error::Result;
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

fn binding_from_model(
    model: entities::code_external_binding::Model,
) -> Result<CodeExternalBinding> {
    Ok(CodeExternalBinding {
        id: CodeBindingId(model.id),
        owner: OwnerId::new(&model.owner)?,
        channel_kind: model.channel_kind,
        external_key: model.external_key,
        grant_id: CodeGrantId(model.grant_id),
        session_id: SessionId(model.session_id),
        created_at: model.created_at,
    })
}

/// The binding for one conversation, when one exists.
pub async fn get_external_binding(
    store: &DbStore,
    owner: &OwnerId,
    channel_kind: &str,
    external_key: &str,
) -> Result<Option<CodeExternalBinding>> {
    entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::ChannelKind.eq(channel_kind))
        .filter(entities::code_external_binding::Column::ExternalKey.eq(external_key))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(binding_from_model)
        .transpose()
}

/// Every binding whose session is in `session_ids`, for the owner.
///
/// The desktop snapshot join: one query answers provenance for a whole
/// workspace's session list instead of one lookup per session.
pub async fn list_external_bindings_for_sessions(
    store: &DbStore,
    owner: &OwnerId,
    session_ids: &[SessionId],
) -> Result<Vec<CodeExternalBinding>> {
    if session_ids.is_empty() {
        return Ok(Vec::new());
    }
    entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_external_binding::Column::SessionId
                .is_in(session_ids.iter().map(|id| id.0)),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(binding_from_model)
        .collect()
}

/// Whether `session_id` is bound under `grant_id`.
///
/// The scope check every grant-authenticated session call runs: a grant may
/// touch exactly the sessions its own bindings name, and nothing else.
pub async fn session_bound_to_grant(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
    grant_id: CodeGrantId,
) -> Result<bool> {
    Ok(entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::SessionId.eq(session_id.0))
        .filter(entities::code_external_binding::Column::GrantId.eq(grant_id.0))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// Classify an existing binding for a caller holding `grant_id`.
async fn classify_hit<C>(
    connection: &C,
    binding: entities::code_external_binding::Model,
    grant_id: CodeGrantId,
) -> Result<ExternalSessionResolution>
where
    C: sea_orm::ConnectionTrait,
{
    if binding.grant_id != grant_id.0 {
        return Ok(ExternalSessionResolution::GrantMismatch);
    }
    let session_id = SessionId(binding.session_id);
    let ended = entities::session::Entity::find_by_id(binding.session_id)
        .one(connection)
        .await
        .map_err(store_err)?
        .is_none_or(|session| session.lifecycle == SessionLifecycle::Ended.as_str());
    if ended {
        return Ok(ExternalSessionResolution::Ended { session_id });
    }
    Ok(ExternalSessionResolution::Existing(Box::new(
        binding_from_model(binding)?,
    )))
}

/// Bind one conversation to a session, creating everything on first contact.
///
/// A miss commits the caller-built workspace, session, and binding in one
/// transaction; a hit classifies the existing binding and inserts nothing.
/// Two racing creates cannot both commit — the unique conversation key
/// refuses the loser, which re-reads and answers `Existing` for the winner's
/// session. An ended session answers `Ended` rather than resurrecting, and a
/// binding under another grant refuses.
pub async fn resolve_external_session(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_kind: &str,
    external_key: &str,
    workspace: &CodeWorkspace,
    session: &Session,
) -> Result<ExternalSessionResolution> {
    resolve_external_session_inner(
        store,
        owner,
        grant_id,
        channel_kind,
        external_key,
        Some(workspace),
        session,
    )
    .await
}

/// Insert a machine session and its external binding atomically. Recovery
/// cannot observe the session before its grant is known.
pub async fn resolve_external_machine_session(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_kind: &str,
    external_key: &str,
    session: &Session,
) -> Result<ExternalSessionResolution> {
    resolve_external_session_inner(
        store,
        owner,
        grant_id,
        channel_kind,
        external_key,
        None,
        session,
    )
    .await
}

async fn resolve_external_session_inner(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_kind: &str,
    external_key: &str,
    workspace: Option<&CodeWorkspace>,
    session: &Session,
) -> Result<ExternalSessionResolution> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(hit) = entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::ChannelKind.eq(channel_kind))
        .filter(entities::code_external_binding::Column::ExternalKey.eq(external_key))
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let resolution = classify_hit(&transaction, hit, grant_id).await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(resolution);
    }
    if let Some(workspace) = workspace {
        super::workspace::insert_workspace_on(&transaction, workspace).await?;
    }
    super::session::insert_session_on(&transaction, session).await?;
    let now = database_now(&transaction).await?;
    let binding = CodeExternalBinding {
        id: CodeBindingId::new(),
        owner: owner.clone(),
        channel_kind: channel_kind.to_owned(),
        external_key: external_key.to_owned(),
        grant_id,
        session_id: session.id,
        created_at: now,
    };
    let inserted = entities::code_external_binding::ActiveModel {
        id: Set(binding.id.0),
        owner: Set(owner.as_str().to_owned()),
        channel_kind: Set(channel_kind.to_owned()),
        external_key: Set(external_key.to_owned()),
        grant_id: Set(grant_id.0),
        session_id: Set(session.id.0),
        created_at: Set(now),
    }
    .insert(&transaction)
    .await;
    match inserted {
        Ok(_) => {
            transaction.commit().await.map_err(store_err)?;
            Ok(ExternalSessionResolution::Created(Box::new(binding)))
        }
        Err(error) => {
            // The unique conversation key refused: another create committed
            // between our read and this insert. Roll back our rows and
            // answer with the winner's.
            transaction.rollback().await.map_err(store_err)?;
            let Some(hit) = entities::code_external_binding::Entity::find()
                .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
                .filter(entities::code_external_binding::Column::ChannelKind.eq(channel_kind))
                .filter(entities::code_external_binding::Column::ExternalKey.eq(external_key))
                .one(&store.conn)
                .await
                .map_err(store_err)?
            else {
                return Err(store_err(error));
            };
            classify_hit(&store.conn, hit, grant_id).await
        }
    }
}

/// Bind one conversation to a session that already exists, for the machine
/// location where the workspace and session were created by the ordinary
/// local path before the binding (decision 0088).
///
/// A hit classifies the existing binding and inserts nothing; a miss
/// inserts the binding. Two racing creates cannot both commit: the unique
/// conversation key refuses the loser, which re-reads and answers
/// `Existing` for the winner's binding. The loser's session stays as an
/// unbound, idle session its owner can reap.
///
/// # Errors
///
/// Returns an error when the store refuses.
pub async fn bind_external_session(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_kind: &str,
    external_key: &str,
    session_id: SessionId,
) -> Result<ExternalSessionResolution> {
    if let Some(hit) = entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::ChannelKind.eq(channel_kind))
        .filter(entities::code_external_binding::Column::ExternalKey.eq(external_key))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    {
        return classify_hit(&store.conn, hit, grant_id).await;
    }
    let now = database_now(&store.conn).await?;
    let binding = CodeExternalBinding {
        id: CodeBindingId::new(),
        owner: owner.clone(),
        channel_kind: channel_kind.to_owned(),
        external_key: external_key.to_owned(),
        grant_id,
        session_id,
        created_at: now,
    };
    let inserted = entities::code_external_binding::ActiveModel {
        id: Set(binding.id.0),
        owner: Set(owner.as_str().to_owned()),
        channel_kind: Set(channel_kind.to_owned()),
        external_key: Set(external_key.to_owned()),
        grant_id: Set(grant_id.0),
        session_id: Set(session_id.0),
        created_at: Set(now),
    }
    .insert(&store.conn)
    .await;
    match inserted {
        Ok(_) => Ok(ExternalSessionResolution::Created(Box::new(binding))),
        Err(error) => {
            let Some(hit) = entities::code_external_binding::Entity::find()
                .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
                .filter(entities::code_external_binding::Column::ChannelKind.eq(channel_kind))
                .filter(entities::code_external_binding::Column::ExternalKey.eq(external_key))
                .one(&store.conn)
                .await
                .map_err(store_err)?
            else {
                return Err(store_err(error));
            };
            classify_hit(&store.conn, hit, grant_id).await
        }
    }
}

/// Every binding pointing at one session, for revoke and scope sweeps.
pub async fn list_bindings_for_session(
    store: &DbStore,
    owner: &OwnerId,
    session_id: SessionId,
) -> Result<Vec<CodeExternalBinding>> {
    entities::code_external_binding::Entity::find()
        .filter(entities::code_external_binding::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_binding::Column::SessionId.eq(session_id.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(binding_from_model)
        .collect()
}
