//! Adapter grants: the credential a channel adapter holds per linked user
//! (docs/slack-sessions.md, stage 2).
//!
//! The machine stores only hashes. Authentication is a lookup by the
//! presented access token's hash; rotation trades the current refresh hash
//! for a new pair and retires the old hash into a per-grant history. A
//! replayed refresh token from any retired generation — the adapter
//! discarded those tokens, so a replay is theft — revokes the grant in the
//! same transaction that detects it.

use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};

use crate::code::{CodeExternalGrant, CodeGrantId, GrantRotation};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

fn grant_from_model(model: entities::code_external_grant::Model) -> Result<CodeExternalGrant> {
    Ok(CodeExternalGrant {
        id: CodeGrantId(model.id),
        owner: OwnerId::new(&model.owner)?,
        channel_kind: model.channel_kind,
        external_identity: model.external_identity,
        workspace_identity: model.workspace_identity,
        rotated_at: model.rotated_at,
        created_at: model.created_at,
        revoked_at: model.revoked_at,
        revoked_reason: model.revoked_reason,
    })
}

fn hash_like(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Mint one grant. The caller hashes the token pair; the secrets never
/// reach this layer. A live grant already covering the same linked
/// identity refuses — revoke it first, so a re-link is an explicit
/// replacement rather than a silent second credential.
pub async fn mint_external_grant(
    store: &DbStore,
    owner: &OwnerId,
    channel_kind: &str,
    external_identity: &str,
    workspace_identity: &str,
    token_hash: &str,
    refresh_hash: &str,
) -> Result<CodeExternalGrant> {
    if channel_kind.trim().is_empty()
        || external_identity.trim().is_empty()
        || workspace_identity.trim().is_empty()
    {
        return Err(AgentError::Store(
            "a grant needs a channel kind and both identities".into(),
        ));
    }
    if !hash_like(token_hash) || !hash_like(refresh_hash) {
        return Err(AgentError::Store(
            "a grant stores 64-hex-digit token hashes, never secrets".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let live = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_grant::Column::ChannelKind.eq(channel_kind))
        .filter(entities::code_external_grant::Column::ExternalIdentity.eq(external_identity))
        .filter(entities::code_external_grant::Column::WorkspaceIdentity.eq(workspace_identity))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if live.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store(
            "this identity already holds a live grant; revoke it first".into(),
        ));
    }
    let now = database_now(&transaction).await?;
    let grant = CodeExternalGrant {
        id: CodeGrantId::new(),
        owner: owner.clone(),
        channel_kind: channel_kind.to_owned(),
        external_identity: external_identity.to_owned(),
        workspace_identity: workspace_identity.to_owned(),
        rotated_at: None,
        created_at: now,
        revoked_at: None,
        revoked_reason: None,
    };
    entities::code_external_grant::ActiveModel {
        id: Set(grant.id.0),
        owner: Set(owner.as_str().to_owned()),
        channel_kind: Set(grant.channel_kind.clone()),
        external_identity: Set(grant.external_identity.clone()),
        workspace_identity: Set(grant.workspace_identity.clone()),
        token_hash: Set(token_hash.to_owned()),
        refresh_hash: Set(refresh_hash.to_owned()),
        rotated_at: Set(None),
        created_at: Set(now),
        revoked_at: Set(None),
        revoked_reason: Set(None),
    }
    .insert(&transaction)
    .await
    // The read above gives the friendly answer; the partial unique index
    // on the live identity is what actually holds against a concurrent
    // mint, and its violation gets the same refusal.
    .map_err(|error| {
        if matches!(
            error.sql_err(),
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
        ) {
            AgentError::Store("this identity already holds a live grant; revoke it first".into())
        } else {
            store_err(error)
        }
    })?;
    transaction.commit().await.map_err(store_err)?;
    Ok(grant)
}

/// The live grant a presented access token authenticates, when one does.
/// A revoked grant matches nothing: its next call fails here.
///
/// `_all_owners` because this is the credential-to-owner resolution: the
/// adapter presents a token without knowing whose it is, and the grant row
/// answers with its owner.
pub async fn grant_by_token_hash_all_owners(
    store: &DbStore,
    token_hash: &str,
) -> Result<Option<CodeExternalGrant>> {
    entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::TokenHash.eq(token_hash))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(grant_from_model)
        .transpose()
}

/// Trade the current refresh hash for a new token pair, detecting reuse.
///
/// A hash matching a live grant's current refresh rotates the pair and
/// retires the presented hash. A hash matching any retired generation was
/// already traded away — the adapter discarded that token, so its
/// reappearance is theft, and the grant revokes in the same transaction.
/// Anything else answers `Unknown`.
///
/// `_all_owners` for the same reason as
/// [`grant_by_token_hash_all_owners`]: the adapter presents only the
/// token, and the hash resolves the grant across owners.
pub async fn rotate_external_grant_all_owners(
    store: &DbStore,
    presented_refresh_hash: &str,
    new_token_hash: &str,
    new_refresh_hash: &str,
) -> Result<GrantRotation> {
    if !hash_like(new_token_hash) || !hash_like(new_refresh_hash) {
        return Err(AgentError::Store(
            "a grant stores 64-hex-digit token hashes, never secrets".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    // Compare-and-swap on the presented hash: of two concurrent rotations
    // presenting the same refresh token, exactly one matches. The loser
    // falls through to the retired-hash check below and is classified as
    // reuse — a shared refresh token is the theft the contract names.
    let swapped = entities::code_external_grant::Entity::update_many()
        .col_expr(
            entities::code_external_grant::Column::TokenHash,
            sea_orm::sea_query::Expr::value(new_token_hash),
        )
        .col_expr(
            entities::code_external_grant::Column::RefreshHash,
            sea_orm::sea_query::Expr::value(new_refresh_hash),
        )
        .col_expr(
            entities::code_external_grant::Column::RotatedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::code_external_grant::Column::RefreshHash.eq(presented_refresh_hash))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if swapped.rows_affected == 1 {
        let updated = entities::code_external_grant::Entity::find()
            .filter(entities::code_external_grant::Column::RefreshHash.eq(new_refresh_hash))
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("grant disappeared mid-rotation".into()))?;
        entities::code_external_grant_retired_refresh::ActiveModel {
            hash: Set(presented_refresh_hash.to_owned()),
            grant_id: Set(updated.id),
            retired_at: Set(now),
        }
        .insert(&transaction)
        .await
        .map_err(store_err)?;
        let grant = grant_from_model(updated)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(GrantRotation::Rotated(Box::new(grant)));
    }
    let retired =
        entities::code_external_grant_retired_refresh::Entity::find_by_id(presented_refresh_hash)
            .one(&transaction)
            .await
            .map_err(store_err)?;
    if let Some(stolen) = match retired {
        Some(retired) => entities::code_external_grant::Entity::find_by_id(retired.grant_id)
            .filter(entities::code_external_grant::Column::RevokedAt.is_null())
            .one(&transaction)
            .await
            .map_err(store_err)?,
        None => None,
    } {
        let id = stolen.id;
        entities::code_external_grant::ActiveModel {
            id: Set(id),
            revoked_at: Set(Some(now)),
            revoked_reason: Set(Some(
                "a rotated refresh token was replayed; the credential is treated as stolen"
                    .to_owned(),
            )),
            ..Default::default()
        }
        .update(&transaction)
        .await
        .map_err(store_err)?;
        let updated = entities::code_external_grant::Entity::find_by_id(id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("grant disappeared mid-revoke".into()))?;
        let grant = grant_from_model(updated)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(GrantRotation::ReuseDetected(Box::new(grant)));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(GrantRotation::Unknown)
}

/// Revoke one grant. Idempotent: an already-revoked grant keeps its first
/// reason and timestamp. Returns the row, or `None` for a grant the owner
/// does not hold.
pub async fn revoke_external_grant(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    reason: &str,
) -> Result<Option<CodeExternalGrant>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(row) = entities::code_external_grant::Entity::find_by_id(grant_id.0)
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if row.revoked_at.is_some() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(grant_from_model(row)?));
    }
    let now = database_now(&transaction).await?;
    entities::code_external_grant::ActiveModel {
        id: Set(grant_id.0),
        revoked_at: Set(Some(now)),
        revoked_reason: Set(Some(reason.to_owned())),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::code_external_grant::Entity::find_by_id(grant_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("grant disappeared mid-revoke".into()))?;
    let grant = grant_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(grant))
}

/// Every grant the owner holds, live and revoked, newest first. The
/// desktop grants list renders this, revocation reasons included.
pub async fn list_external_grants(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<CodeExternalGrant>> {
    use sea_orm::QueryOrder;
    entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_external_grant::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(grant_from_model)
        .collect()
}

/// One grant by id, for the owner.
pub async fn get_external_grant(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
) -> Result<Option<CodeExternalGrant>> {
    entities::code_external_grant::Entity::find_by_id(grant_id.0)
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(grant_from_model)
        .transpose()
}
