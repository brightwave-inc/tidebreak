//! Connect handshakes: the human half of minting a grant
//! (docs/slack-sessions.md, stage 2).
//!
//! The approval nonce and adapter confirmation token are separate
//! capabilities. The approval link carries only the nonce. The adapter keeps
//! the confirmation token, polls with it, and presents it after the channel DM
//! proves control of the external account. Approval and completion are
//! compare-and-set transitions, and completion mints the grant in the same
//! transaction that consumes the handshake.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
    Set, TransactionTrait,
};

use crate::code::{
    CodeConnectHandshake, CodeConnectState, CodeExternalGrant, CodeGrantId, CodeGrantProfile,
    CodeHandshakeId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;
use super::grant::hash_like;

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
        approval_owner: model
            .approval_owner
            .as_deref()
            .map(OwnerId::new)
            .transpose()?,
        grant_id: model.grant_id.map(CodeGrantId),
        created_at: model.created_at,
        expires_at: model.expires_at,
    })
}

/// Park one handshake. The caller hashes both capabilities and mints the CSRF
/// token; no secret other than the CSRF synchronizer reaches this layer.
#[allow(clippy::too_many_arguments)]
pub async fn insert_connect_handshake(
    store: &DbStore,
    nonce_hash: &str,
    confirm_hash: &str,
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
    if !hash_like(nonce_hash) || !hash_like(confirm_hash) || csrf.trim().is_empty() {
        return Err(AgentError::Store(
            "a connect handshake needs hashed capabilities and a CSRF token".into(),
        ));
    }
    if ttl <= chrono::Duration::zero() {
        return Err(AgentError::Store(
            "a connect handshake needs a positive lifetime".into(),
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
        approval_owner: None,
        grant_id: None,
        created_at: now,
        expires_at: now + ttl,
    };
    entities::code_connect_handshake::ActiveModel {
        id: Set(handshake.id.0),
        nonce_hash: Set(nonce_hash.to_owned()),
        confirm_hash: Set(confirm_hash.to_owned()),
        csrf: Set(csrf.to_owned()),
        channel_kind: Set(handshake.channel_kind.clone()),
        external_identity: Set(handshake.external_identity.clone()),
        workspace_identity: Set(handshake.workspace_identity.clone()),
        display_name: Set(handshake.display_name.clone()),
        workspace_name: Set(handshake.workspace_name.clone()),
        avatar_url: Set(handshake.avatar_url.clone()),
        state: Set(CodeConnectState::Pending.as_str().to_owned()),
        approval_owner: Set(None),
        grant_id: Set(None),
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

/// The handshake an authenticated owner may view, with its CSRF token.
///
/// The first authenticated view binds the approval surface to that owner. A
/// different owner sees the same not-found shape as an invalid nonce. Two
/// owners racing the first view cannot both claim it because the claim is a
/// compare-and-set on the null owner column.
pub async fn view_connect_handshake_all_owners(
    store: &DbStore,
    nonce_hash: &str,
    owner: &OwnerId,
) -> Result<Option<(CodeConnectHandshake, String)>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(mut model) = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let now = database_now(&transaction).await?;
    if model.completed_at.is_some() || model.expires_at <= now {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if model.approval_owner.is_none() {
        let claimed = entities::code_connect_handshake::Entity::update_many()
            .col_expr(
                entities::code_connect_handshake::Column::ApprovalOwner,
                sea_orm::sea_query::Expr::value(owner.as_str()),
            )
            .filter(entities::code_connect_handshake::Column::Id.eq(model.id))
            .filter(entities::code_connect_handshake::Column::ApprovalOwner.is_null())
            .filter(
                entities::code_connect_handshake::Column::State
                    .eq(CodeConnectState::Pending.as_str()),
            )
            .filter(entities::code_connect_handshake::Column::ExpiresAt.gt(now))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
        if claimed.rows_affected > 1 {
            return Err(AgentError::Store(
                "a connect handshake claim changed more than one row".into(),
            ));
        }
        model = entities::code_connect_handshake::Entity::find_by_id(model.id)
            .one(&transaction)
            .await
            .map_err(store_err)?
            .ok_or_else(|| AgentError::Store("connect handshake disappeared".into()))?;
    }
    if model.approval_owner.as_deref() != Some(owner.as_str()) {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let csrf = model.csrf.clone();
    let handshake = handshake_from_model(model)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some((handshake, csrf)))
}

/// The state the adapter may poll while it holds both handshake capabilities.
/// The approval link alone cannot observe or complete the adapter side.
pub async fn connect_handshake_status_all_owners(
    store: &DbStore,
    nonce_hash: &str,
    confirm_hash: &str,
) -> Result<Option<CodeConnectHandshake>> {
    let now = database_now(&store.conn).await?;
    entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .filter(entities::code_connect_handshake::Column::ConfirmHash.eq(confirm_hash))
        .filter(entities::code_connect_handshake::Column::ExpiresAt.gt(now))
        .filter(
            entities::code_connect_handshake::Column::State
                .ne(CodeConnectState::Completed.as_str()),
        )
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(handshake_from_model)
        .transpose()
}

/// The owner's "is this you?" transition. The nonce, owner-bound CSRF token,
/// current state, and expiry are one compare-and-set predicate, so two approval
/// posts cannot both take effect.
pub async fn approve_connect_handshake_all_owners(
    store: &DbStore,
    nonce_hash: &str,
    csrf: &str,
    owner: &OwnerId,
) -> Result<Option<CodeConnectHandshake>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    let moved = entities::code_connect_handshake::Entity::update_many()
        .col_expr(
            entities::code_connect_handshake::Column::State,
            sea_orm::sea_query::Expr::value(CodeConnectState::Approved.as_str()),
        )
        .col_expr(
            entities::code_connect_handshake::Column::ApprovedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .filter(entities::code_connect_handshake::Column::Csrf.eq(csrf))
        .filter(entities::code_connect_handshake::Column::ApprovalOwner.eq(owner.as_str()))
        .filter(
            entities::code_connect_handshake::Column::State.eq(CodeConnectState::Pending.as_str()),
        )
        .filter(entities::code_connect_handshake::Column::ExpiresAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if moved.rows_affected == 0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if moved.rows_affected != 1 {
        return Err(AgentError::Store(
            "a connect approval changed more than one row".into(),
        ));
    }
    let updated = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("connect handshake disappeared".into()))?;
    let handshake = handshake_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(handshake))
}

/// Consume one approved handshake and mint its grant atomically.
///
/// A failed insert rolls back the completed state and any replacement revoke.
/// A concurrent completion loses the state compare-and-set and returns `None`.
pub async fn complete_connect_handshake_and_mint_grant_all_owners(
    store: &DbStore,
    nonce_hash: &str,
    confirm_hash: &str,
    token_hash: &str,
    refresh_hash: &str,
) -> Result<Option<(CodeExternalGrant, Vec<CodeGrantId>)>> {
    if !hash_like(token_hash) || !hash_like(refresh_hash) {
        return Err(AgentError::Store(
            "a grant stores 64-hex-digit token hashes, never secrets".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    let Some(model) = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::NonceHash.eq(nonce_hash))
        .filter(entities::code_connect_handshake::Column::ConfirmHash.eq(confirm_hash))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(owner) = model
        .approval_owner
        .as_deref()
        .map(OwnerId::new)
        .transpose()?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let consumed = entities::code_connect_handshake::Entity::update_many()
        .col_expr(
            entities::code_connect_handshake::Column::State,
            sea_orm::sea_query::Expr::value(CodeConnectState::Completed.as_str()),
        )
        .col_expr(
            entities::code_connect_handshake::Column::CompletedAt,
            sea_orm::sea_query::Expr::value(Some(now)),
        )
        .filter(entities::code_connect_handshake::Column::Id.eq(model.id))
        .filter(entities::code_connect_handshake::Column::ConfirmHash.eq(confirm_hash))
        .filter(
            entities::code_connect_handshake::Column::State.eq(CodeConnectState::Approved.as_str()),
        )
        .filter(entities::code_connect_handshake::Column::GrantId.is_null())
        .filter(entities::code_connect_handshake::Column::ExpiresAt.gt(now))
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if consumed.rows_affected == 0 {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    if consumed.rows_affected != 1 {
        return Err(AgentError::Store(
            "a connect completion changed more than one row".into(),
        ));
    }

    let replaced = entities::code_external_grant::Entity::find()
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_grant::Column::ChannelKind.eq(model.channel_kind.clone()))
        .filter(
            entities::code_external_grant::Column::ExternalIdentity
                .eq(model.external_identity.clone()),
        )
        .filter(
            entities::code_external_grant::Column::WorkspaceIdentity
                .eq(model.workspace_identity.clone()),
        )
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .all(&transaction)
        .await
        .map_err(store_err)?;
    let replaced_ids: Vec<_> = replaced.iter().map(|grant| CodeGrantId(grant.id)).collect();
    if !replaced_ids.is_empty() {
        entities::code_external_grant::Entity::update_many()
            .col_expr(
                entities::code_external_grant::Column::RevokedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::code_external_grant::Column::RevokedReason,
                sea_orm::sea_query::Expr::value(Some(
                    "replaced by a new connect approval".to_owned(),
                )),
            )
            .filter(
                entities::code_external_grant::Column::Id.is_in(replaced_ids.iter().map(|id| id.0)),
            )
            .filter(entities::code_external_grant::Column::RevokedAt.is_null())
            .exec(&transaction)
            .await
            .map_err(store_err)?;
    }

    let grant = CodeExternalGrant {
        id: CodeGrantId::new(),
        owner: owner.clone(),
        channel_kind: model.channel_kind,
        external_identity: model.external_identity,
        workspace_identity: model.workspace_identity,
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
    .map_err(store_err)?;
    let linked = entities::code_connect_handshake::Entity::update_many()
        .col_expr(
            entities::code_connect_handshake::Column::GrantId,
            sea_orm::sea_query::Expr::value(Some(grant.id.0)),
        )
        .filter(entities::code_connect_handshake::Column::Id.eq(model.id))
        .filter(
            entities::code_connect_handshake::Column::State
                .eq(CodeConnectState::Completed.as_str()),
        )
        .filter(entities::code_connect_handshake::Column::GrantId.is_null())
        .exec(&transaction)
        .await
        .map_err(store_err)?;
    if linked.rows_affected != 1 {
        return Err(AgentError::Store(
            "a completed handshake could not retain its grant identity".into(),
        ));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(Some((grant, replaced_ids)))
}

/// Human-facing profiles for the grants one owner minted through connect.
pub async fn list_connect_grant_profiles(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<CodeGrantProfile>> {
    let rows = entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::ApprovalOwner.eq(owner.as_str()))
        .filter(
            entities::code_connect_handshake::Column::State
                .eq(CodeConnectState::Completed.as_str()),
        )
        .filter(entities::code_connect_handshake::Column::GrantId.is_not_null())
        .order_by_desc(entities::code_connect_handshake::Column::CompletedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            row.grant_id.map(|grant_id| CodeGrantProfile {
                grant_id: CodeGrantId(grant_id),
                display_name: row.display_name,
                workspace_name: row.workspace_name,
                avatar_url: row.avatar_url,
            })
        })
        .collect())
}

/// The completed consent that owns one grant's gateway delegation.
pub async fn completed_connect_handshake_for_grant(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
) -> Result<Option<CodeConnectHandshake>> {
    entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::ApprovalOwner.eq(owner.as_str()))
        .filter(entities::code_connect_handshake::Column::GrantId.eq(grant_id.0))
        .filter(
            entities::code_connect_handshake::Column::State
                .eq(CodeConnectState::Completed.as_str()),
        )
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(handshake_from_model)
        .transpose()
}

/// Revoked external consent retained for retrying gateway revocation after a restart.
pub async fn revoked_connect_handshakes_all_owners(
    store: &DbStore,
) -> Result<Vec<CodeConnectHandshake>> {
    let revoked = entities::code_external_grant::Entity::find()
        .select_only()
        .column(entities::code_external_grant::Column::Id)
        .filter(entities::code_external_grant::Column::RevokedAt.is_not_null())
        .into_query();
    entities::code_connect_handshake::Entity::find()
        .filter(entities::code_connect_handshake::Column::GrantId.in_subquery(revoked))
        .filter(
            entities::code_connect_handshake::Column::State
                .eq(CodeConnectState::Completed.as_str()),
        )
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(handshake_from_model)
        .collect()
}
