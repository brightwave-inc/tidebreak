//! Per-channel repository confirmation under a workspace grant.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::code::{CodeChannelRepositoryConfirm, CodeChannelRepositoryState, CodeGrantId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};
use super::super::agent_run::database_now;

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
    grant_id: CodeGrantId,
) -> Result<Vec<CodeChannelRepositoryConfirm>> {
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
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
) -> Result<bool> {
    Ok(entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        .filter(entities::code_channel_repository_confirm::Column::Repository.eq(repository))
        .filter(
            entities::code_channel_repository_confirm::Column::State
                .eq(CodeChannelRepositoryState::Confirmed.as_str()),
        )
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .is_some())
}

/// Record a pending confirmation. A different pending repository for the same
/// channel is superseded. Returns the pending row.
pub async fn ensure_pending_channel_repository(
    store: &DbStore,
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
    set_by_identity: &str,
    set_by_display: &str,
) -> Result<CodeChannelRepositoryConfirm> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let now = database_now(&transaction).await?;
    let pending = entities::code_channel_repository_confirm::Entity::find()
        .filter(entities::code_channel_repository_confirm::Column::GrantId.eq(grant_id.0))
        .filter(entities::code_channel_repository_confirm::Column::ChannelId.eq(channel_id))
        .filter(
            entities::code_channel_repository_confirm::Column::State
                .eq(CodeChannelRepositoryState::Pending.as_str()),
        )
        .all(&transaction)
        .await
        .map_err(store_err)?;
    for row in pending {
        if row.repository == repository {
            let confirm = confirm_from_model(row)?;
            transaction.commit().await.map_err(store_err)?;
            return Ok(confirm);
        }
        entities::code_channel_repository_confirm::ActiveModel {
            grant_id: Set(row.grant_id),
            channel_id: Set(row.channel_id.clone()),
            repository: Set(row.repository.clone()),
            state: Set(CodeChannelRepositoryState::Superseded.as_str().to_owned()),
            ..Default::default()
        }
        .update(&transaction)
        .await
        .map_err(store_err)?;
    }
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

/// An admin confirms a pending `(grant, channel, repository)` pair.
pub async fn confirm_channel_repository(
    store: &DbStore,
    grant_id: CodeGrantId,
    channel_id: &str,
    repository: &str,
    admin: &OwnerId,
) -> Result<Option<CodeChannelRepositoryConfirm>> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
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
