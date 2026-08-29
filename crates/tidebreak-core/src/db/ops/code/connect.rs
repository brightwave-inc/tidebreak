//! Connect handshakes: the human half of minting a grant
//! (docs/slack-sessions.md, stage 2).
//!
//! The nonce is stored as a hash, like every adapter secret. Every lookup
//! is by that hash — the person opening the link is not the adapter that
//! created the row, and no owner exists until approval — so the functions
//! carry the `_all_owners` name. State moves one way, pending to approved
//! to completed, each step a compare-and-set: a nonce is one-time because
//! completion consumes the row, and a replayed or expired step answers
//! `None` rather than a second effect.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

use crate::code::{CodeConnectHandshake, CodeConnectState, CodeHandshakeId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

fn handshake_from_model(
    model: entities::code_connect_handshake::Model,
) -> Result<CodeConnectHandshake> {
    Ok(CodeConnectHandshake {
        id: CodeHandshakeId(model.id),
        channel_kind: model.channel_kind,
        external_identity: model.external_identity,
        workspace_identity: model.workspace_identity,
        display_name: model.display_name,
        workspace_name: model.workspace_name,
        avatar_url: model.avatar_url,
        state: CodeConnectState::from_str(&model.state)
            .ok_or_else(|| AgentError::Store("invalid stored connect handshake state".into()))?,
        approved_owner: model
            .approved_owner
            .as_deref()
            .map(OwnerId::new)
            .transpose()?,
        created_at: model.created_at,
        expires_at: model.expires_at,
    })
}

/// Park one handshake. The caller hashes the nonce and mints the CSRF
/// token; the nonce secret never reaches this layer.
#[allow(clippy::too_many_arguments)]
pub async fn insert_connect_handshake(
    store: &DbStore,
    nonce_hash: &str,
    csrf: &str,
    channel_kind: &str,
    external_identity: &str,
    workspace_identity: &str,
    display_name: &str,
    workspace_name: &str,
    avatar_url: Option<&str>,
    ttl: chrono::Duration,
) -> Result<CodeConnectHandshake> {
    if channel_kind.trim().is_empty()
        || external_identity.trim().is_empty()
        || workspace_identity.trim().is_empty()
        || display_name.trim().is_empty()
        || workspace_name.trim().is_empty()
    {
        return Err(AgentError::Store(
            "a connect handshake needs its channel, identities, and names".into(),
        ));
    }
    if nonce_hash.trim().is_empty() || csrf.trim().is_empty() {
        return Err(AgentError::Store(
            "a connect handshake needs a nonce hash and a CSRF token".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    let handshake = CodeConnectHandshake {
        id: CodeHandshakeId::new(),
        channel_kind: channel_kind.to_owned(),
        external_identity: external_identity.to_owned(),
        workspace_identity: workspace_identity.to_owned(),
        display_name: display_name.to_owned(),
        workspace_name: workspace_name.to_owned(),
        avatar_url: avatar_url.map(str::to_owned),
        state: CodeConnectState::Pending,
        approved_owner: None,
        created_at: now,
        expires_at: now + ttl,
    };
    entities::code_connect_handshake::ActiveModel {
        id: Set(handshake.id.0),
        nonce_hash: Set(nonce_hash.to_owned()),
        csrf: Set(csrf.to_owned()),
        channel_kind: Set(handshake.channel_kind.clone()),
        external_identity: Set(handshake.external_identity.clone()),
        workspace_identity: Set(handshake.workspace_identity.clone()),
        display_name: Set(handshake.display_name.clone()),
        workspace_name: Set(handshake.workspace_name.clone()),
        avatar_url: Set(handshake.avatar_url.clone()),
        state: Set(CodeConnectState::Pending.as_str().to_owned()),
        approved_owner: Set(None),
        created_at: Set(now),
        expires_at: Set(handshake.expires_at),
        approved_at: Set(None),
        completed_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(handshake)
}

/// The handshake a nonce opens, with its CSRF token for the approval
/// page. Answers `None` once completed or expired: a used or stale link
/// shows nothing.
pub async fn view_connect_handshake_all_owners(
    store: &DbStore,
    nonce_hash: &str,
) -> Result<Option<(CodeConnectHandshake, String)>> {
    let Some(model) = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    let now = database_now(&store.conn).await?;
    if model.completed_at.is_some() || model.expires_at <= now {
        return Ok(None);
    }
    let csrf = model.csrf.clone();
    Ok(Some((handshake_from_model(model)?, csrf)))
}

/// The owner's "is this you?" — pending to approved, one way.
///
/// Requires the CSRF token the page was served with, refuses an expired
/// or already-moved row, and records who approved. Approving mints
/// nothing: the adapter's closing confirm does.
pub async fn approve_connect_handshake_all_owners(
    store: &DbStore,
    nonce_hash: &str,
    csrf: &str,
    owner: &OwnerId,
) -> Result<Option<CodeConnectHandshake>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(model) = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let now = database_now(&transaction).await?;
    if model.state != CodeConnectState::Pending.as_str()
        || model.expires_at <= now
        || model.csrf != csrf
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let id = model.id;
    entities::code_connect_handshake::ActiveModel {
        id: Set(id),
        state: Set(CodeConnectState::Approved.as_str().to_owned()),
        approved_owner: Set(Some(owner.as_str().to_owned())),
        approved_at: Set(Some(now)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::code_connect_handshake::Entity::find_by_id(id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("connect handshake disappeared".into()))?;
    let handshake = handshake_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(handshake))
}

/// The adapter's closing confirm — approved to completed, one way, once.
///
/// Answers the approved row exactly once; a replay, an expired row, or a
/// row nobody approved answers `None`. This is the step that makes a
/// forwarded link worthless: without it, an approval binds nothing.
pub async fn complete_connect_handshake_all_owners(
    store: &DbStore,
    nonce_hash: &str,
) -> Result<Option<CodeConnectHandshake>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(model) = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let now = database_now(&transaction).await?;
    if model.state != CodeConnectState::Approved.as_str() || model.expires_at <= now {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let id = model.id;
    entities::code_connect_handshake::ActiveModel {
        id: Set(id),
        state: Set(CodeConnectState::Completed.as_str().to_owned()),
        completed_at: Set(Some(now)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::code_connect_handshake::Entity::find_by_id(id)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("connect handshake disappeared".into()))?;
    let handshake = handshake_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(handshake))
}
