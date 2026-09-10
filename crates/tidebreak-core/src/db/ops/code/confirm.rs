//! Channel repository-confirm rows kept for older clients.
//!
//! Decision 0096 removed the administrator repository-scope gate. These
//! rows no longer authorize anything.

use sea_orm::sea_query::{Expr, ExprTrait, Func};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::code::{
    CodeChannelRepositoryConfirm, CodeChannelRepositoryState, CodeGrantId, CodeGrantKind,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

async fn grant_owned_by(
    conn: &impl sea_orm::ConnectionTrait,
    owner: &OwnerId,
    grant_id: CodeGrantId,
) -> Result<bool> {
    Ok(
        entities::code_external_grant::Entity::find_by_id(grant_id.0)
            .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
            .one(conn)
            .await
            .map_err(store_err)?
            .is_some(),
    )
}

/// Serialize scope changes with grant revocation on both SQLite and PostgreSQL.
async fn lock_live_workspace_grant(
    conn: &impl sea_orm::ConnectionTrait,
    owner: &OwnerId,
    grant_id: CodeGrantId,
) -> Result<bool> {
    let locked = entities::code_external_grant::Entity::update_many()
        .col_expr(
            entities::code_external_grant::Column::Id,
            sea_orm::sea_query::Expr::col(entities::code_external_grant::Column::Id),
        )
        .filter(entities::code_external_grant::Column::Id.eq(grant_id.0))
        .filter(entities::code_external_grant::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_external_grant::Column::Kind.eq(CodeGrantKind::Workspace.as_str()))
        .filter(entities::code_external_grant::Column::RevokedAt.is_null())
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

fn confirm_from_model(
    model: entities::code_channel_repository_confirm::Model,
) -> Result<CodeChannelRepositoryConfirm> {
    Ok(CodeChannelRepositoryConfirm {
        grant_id: CodeGrantId(model.grant_id),
        channel_id: model.channel_id,
        repository: model.repository,
        set_by_identity: model.set_by_identity,
        set_by_display: model.set_by_display,
        state: CodeChannelRepositoryState::from_str(&model.state).ok_or_else(|| {
            AgentError::Store("invalid stored repository confirmation state".into())
        })?,
        confirmed_by: model
            .confirmed_by
            .as_deref()
            .map(OwnerId::new)
            .transpose()?,
        created_at: model.created_at,
    })
}

/// Every confirmation row one grant holds, newest first.
pub async fn list_channel_repository_confirms(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
) -> Result<Vec<CodeChannelRepositoryConfirm>> {
    if !grant_owned_by(&store.conn, owner, grant_id).await? {
        return Ok(Vec::new());
    }
    entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .order_by_desc(entities::code_channel_repository_confirm::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(confirm_from_model)
        .collect()
}

/// Whether `(grant, channel, repository)` is confirmed.
pub async fn channel_repository_is_confirmed(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
) -> Result<bool> {
    if !grant_owned_by(&store.conn, owner, grant_id).await? {
        return Ok(false);
    }
    Ok(entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        // Older approvals can retain the forge's mixed-case spelling.
        .filter(
            Func::lower(Expr::col(
                entities::code_channel_repository_confirm::Column::Repository,
            ))
            .eq(repository.to_ascii_lowercase()),
        )
        .filter(
            entities::code_channel_repository_confirm::Column::State
                .eq(CodeChannelRepositoryState::Confirmed.as_str()),
        )
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// Record a repository request without replacing other requests in the channel.
/// An approval that wins a concurrent request stays confirmed.
pub async fn ensure_pending_channel_repository(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
    set_by_identity: &str,
    set_by_display: &str,
) -> Result<CodeChannelRepositoryConfirm> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !lock_live_workspace_grant(&transaction, owner, grant_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Err(AgentError::Store("live workspace grant not found".into()));
    }
    let existing = entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        .filter(
            Func::lower(Expr::col(
                entities::code_channel_repository_confirm::Column::Repository,
            ))
            .eq(repository.to_ascii_lowercase()),
        )
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(row) = existing {
        if row.state != CodeChannelRepositoryState::Superseded.as_str() {
            let confirm = confirm_from_model(row)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(confirm);
        }
        // Older releases superseded the first request when a second repository
        // was selected. A retry must revive that request instead of colliding
        // with its primary key.
        let updated = entities::code_channel_repository_confirm::ActiveModel {
            grant_id: Set(row.grant_id),
            channel_id: Set(row.channel_id),
            repository: Set(row.repository),
            state: Set(CodeChannelRepositoryState::Pending.as_str().to_owned()),
            set_by_identity: Set(set_by_identity.to_owned()),
            set_by_display: Set(set_by_display.to_owned()),
            confirmed_by: Set(None),
            ..Default::default()
        }
        .update(&transaction)
        .await
        .map_err(store_err)?;
        transaction.commit().await.map_err(store_err)?;
        return confirm_from_model(updated);
    }
    let now = database_now(&transaction).await?;
    let model = entities::code_channel_repository_confirm::ActiveModel {
        grant_id: Set(grant_id.0),
        channel_id: Set(channel_id.to_owned()),
        repository: Set(repository.to_owned()),
        set_by_identity: Set(set_by_identity.to_owned()),
        set_by_display: Set(set_by_display.to_owned()),
        state: Set(CodeChannelRepositoryState::Pending.as_str().to_owned()),
        confirmed_by: Set(None),
        created_at: Set(now),
    };
    let inserted = model.insert(&transaction).await.map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    confirm_from_model(inserted)
}

/// Approve an explicit repository scope before or after a channel requests it.
/// The caller validates canonical repository names and administrator authority.
/// Existing approvals remain valid; the whole batch commits together.
pub async fn approve_channel_repositories(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_id: &str,
    repositories: &[String],
    admin: &OwnerId,
) -> Result<bool> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !lock_live_workspace_grant(&transaction, owner, grant_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(false);
    }
    let now = database_now(&transaction).await?;
    for repository in repositories {
        use entities::code_channel_repository_confirm::{ActiveModel, Column, Entity};
        let aliases = Entity::find()
            .filter(Column::GrantId.eq(grant_id.0))
            .filter(Column::ChannelId.eq(channel_id))
            .filter(Func::lower(Expr::col(Column::Repository)).eq(repository))
            .order_by_asc(Column::CreatedAt)
            .order_by_asc(Column::Repository)
            .all(&transaction)
            .await
            .map_err(store_err)?;
        let previous = aliases
            .iter()
            .find(|row| row.repository == *repository)
            .or_else(|| aliases.first());
        Entity::insert(ActiveModel {
            grant_id: Set(grant_id.0),
            channel_id: Set(channel_id.to_owned()),
            repository: Set(repository.clone()),
            set_by_identity: Set(previous.map_or_else(
                || admin.as_str().to_owned(),
                |row| row.set_by_identity.clone(),
            )),
            set_by_display: Set(previous.map_or_else(
                || "Administrator".to_owned(),
                |row| row.set_by_display.clone(),
            )),
            state: Set(CodeChannelRepositoryState::Confirmed.as_str().to_owned()),
            confirmed_by: Set(Some(admin.as_str().to_owned())),
            created_at: Set(previous.map_or(now, |row| row.created_at)),
        })
        .on_conflict(
            sea_orm::sea_query::OnConflict::columns([
                Column::GrantId,
                Column::ChannelId,
                Column::Repository,
            ])
            .update_columns([Column::State, Column::ConfirmedBy])
            .to_owned(),
        )
        .exec_without_returning(&transaction)
        .await
        .map_err(store_err)?;
        // Collapse historical casing aliases so a completed approval cannot
        // leave the same repository displayed as pending.
        Entity::delete_many()
            .filter(Column::GrantId.eq(grant_id.0))
            .filter(Column::ChannelId.eq(channel_id))
            .filter(Func::lower(Expr::col(Column::Repository)).eq(repository))
            .filter(Column::Repository.ne(repository))
            .exec(&transaction)
            .await
            .map_err(store_err)?;
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

/// An admin confirms a pending `(grant, channel, repository)` pair.
pub async fn confirm_channel_repository(
    store: &DbStore,
    owner: &OwnerId,
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
    admin: &OwnerId,
) -> Result<Option<CodeChannelRepositoryConfirm>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !lock_live_workspace_grant(&transaction, owner, grant_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    let Some(row) = entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        .filter(entities::code_channel_repository_confirm::Column::Repository.eq(repository))
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if row.state == CodeChannelRepositoryState::Confirmed.as_str() {
        let confirm = confirm_from_model(row)?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(confirm));
    }
    if row.state != CodeChannelRepositoryState::Pending.as_str() {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }
    entities::code_channel_repository_confirm::ActiveModel {
        grant_id: Set(row.grant_id),
        channel_id: Set(row.channel_id.clone()),
        repository: Set(row.repository.clone()),
        state: Set(CodeChannelRepositoryState::Confirmed.as_str().to_owned()),
        confirmed_by: Set(Some(admin.as_str().to_owned())),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let updated = entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        .filter(entities::code_channel_repository_confirm::Column::Repository.eq(repository))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("repository confirmation disappeared".into()))?;
    let confirm = confirm_from_model(updated)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(confirm))
}
