//! Persistence for standing trigger rules and their fire fingerprints.
//!
//! Triggers bind per repository (decision 60). The fire table is the
//! fingerprint that makes a trigger fire on an edge rather than on every sweep
//! that still finds its condition true.

use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::code::{
    CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFire, CodeTriggerId, RepoId,
    WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert a trigger, or update the one already armed for the same
/// `(owner, repository, condition)`.
///
/// Arming a condition twice is an edit, not a second rule: the unique index
/// says so and this is the write that honors it. Named `save_` because the
/// owner rides the record, the way `save_watch` does.
pub async fn save_trigger(store: &DbStore, trigger: &CodeTrigger) -> Result<()> {
    entities::code_trigger::Entity::insert(entities::code_trigger::ActiveModel {
        id: Set(trigger.id.0),
        owner: Set(trigger.owner.as_str().to_owned()),
        repo_id: Set(trigger.repo_id.0),
        condition: Set(trigger.condition.as_str().to_owned()),
        action: Set(trigger.action.as_str().to_owned()),
        enabled: Set(trigger.enabled),
        created_at: Set(trigger.created_at),
        updated_at: Set(trigger.updated_at),
    })
    .on_conflict(
        OnConflict::columns([
            entities::code_trigger::Column::Owner,
            entities::code_trigger::Column::RepoId,
            entities::code_trigger::Column::Condition,
        ])
        .update_columns([
            entities::code_trigger::Column::Action,
            entities::code_trigger::Column::Enabled,
            entities::code_trigger::Column::UpdatedAt,
        ])
        .to_owned(),
    )
    .exec(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

/// Every trigger the owner armed on one repository, enabled or not.
pub async fn list_triggers_for_repo(
    store: &DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
) -> Result<Vec<CodeTrigger>> {
    entities::code_trigger::Entity::find()
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger::Column::RepoId.eq(repo_id.0))
        .order_by_asc(entities::code_trigger::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(trigger_from_row)
        .collect()
}

/// Every enabled trigger on the machine.
///
/// A system path, not a request path: the trigger sweep drives every owner's
/// triggers, the way the watch sweep drives every owner's watches. Nothing
/// reachable from a route may call it.
pub async fn list_enabled_triggers_all_owners(store: &DbStore) -> Result<Vec<CodeTrigger>> {
    entities::code_trigger::Entity::find()
        .filter(entities::code_trigger::Column::Enabled.eq(true))
        .order_by_asc(entities::code_trigger::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(trigger_from_row)
        .collect()
}

/// Delete one trigger. Its fire rows go with it through the cascade.
///
/// Returns `false` when the owner had no such trigger.
pub async fn delete_trigger(store: &DbStore, owner: &OwnerId, id: CodeTriggerId) -> Result<bool> {
    let result = entities::code_trigger::Entity::delete_many()
        .filter(entities::code_trigger::Column::Id.eq(id.0))
        .filter(entities::code_trigger::Column::Owner.eq(owner.as_str()))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result.rows_affected == 1)
}

/// Record a fire, reporting whether this call is the one that claimed it.
///
/// `false` means the trigger already fired for that `(workspace, head SHA)`
/// and the caller must not act. The claim *is* the insert: two sweeps racing
/// the same edge both call this, and exactly one gets `true`. The owner rides
/// the record, denormalized from the trigger.
pub async fn insert_trigger_fire(store: &DbStore, fire: &CodeTriggerFire) -> Result<bool> {
    let result =
        entities::code_trigger_fire::Entity::insert(entities::code_trigger_fire::ActiveModel {
            trigger_id: Set(fire.trigger_id.0),
            owner: Set(fire.owner.as_str().to_owned()),
            workspace_id: Set(fire.workspace_id.0),
            head_sha: Set(fire.head_sha.clone()),
            pr_number: Set(i64::try_from(fire.pr_number).map_err(|_| {
                AgentError::Store(format!("code_trigger_fire {} pr number", fire.trigger_id))
            })?),
            fired_at: Set(fire.fired_at),
        })
        .on_conflict(
            OnConflict::columns([
                entities::code_trigger_fire::Column::TriggerId,
                entities::code_trigger_fire::Column::WorkspaceId,
                entities::code_trigger_fire::Column::HeadSha,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(result == 1)
}

/// Fires already recorded for one workspace, newest first.
pub async fn list_fires_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Vec<CodeTriggerFire>> {
    entities::code_trigger_fire::Entity::find()
        .filter(entities::code_trigger_fire::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_trigger_fire::Column::WorkspaceId.eq(workspace_id.0))
        .order_by_desc(entities::code_trigger_fire::Column::FiredAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fire_from_row)
        .collect()
}

fn trigger_from_row(row: entities::code_trigger::Model) -> Result<CodeTrigger> {
    let condition = CodeTriggerCondition::from_str(&row.condition).ok_or_else(|| {
        AgentError::Store(format!(
            "code_trigger {} has unknown condition {}",
            row.id, row.condition
        ))
    })?;
    let action = CodeTriggerAction::from_str(&row.action).ok_or_else(|| {
        AgentError::Store(format!(
            "code_trigger {} has unknown action {}",
            row.id, row.action
        ))
    })?;
    Ok(CodeTrigger {
        id: CodeTriggerId(row.id),
        owner: OwnerId::new(&row.owner)?,
        repo_id: RepoId(row.repo_id),
        condition,
        action,
        enabled: row.enabled,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn fire_from_row(row: entities::code_trigger_fire::Model) -> Result<CodeTriggerFire> {
    let pr_number = u64::try_from(row.pr_number).map_err(|_| {
        AgentError::Store(format!("code_trigger_fire {} pr number", row.trigger_id))
    })?;
    Ok(CodeTriggerFire {
        trigger_id: CodeTriggerId(row.trigger_id),
        owner: OwnerId::new(&row.owner)?,
        workspace_id: WorkspaceId(row.workspace_id),
        pr_number,
        head_sha: row.head_sha,
        fired_at: row.fired_at,
    })
}
