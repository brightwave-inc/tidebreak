//! Persistence for observed GitHub Actions workflow runs.
//!
//! A `code_workflow_run` row is a confirmed observation of one run, keyed
//! by full repository identity so a run in a repository with no local
//! checkout is representable. `code_workflow_run_fetch` holds the list
//! endpoint's ETag so the next read can send `If-None-Match`.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::code::{CodeWorkflowRunFact, CodeWorkflowRunId};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert or refresh one observed workflow run.
///
/// Conflict is on the identity `(owner, host, repo_owner, repo_name,
/// github_id)`: an existing row keeps its id and its `first_seen_at`, and
/// takes the fresh snapshot plus `last_seen_at`. Returns the canonical id
/// and whether any snapshot field other than the seen timestamps moved.
pub async fn save_workflow_run_fact(
    store: &DbStore,
    fact: &CodeWorkflowRunFact,
) -> Result<(CodeWorkflowRunId, bool)> {
    let github_id = i64::try_from(fact.github_id)
        .map_err(|_| AgentError::Store(format!("workflow run id {} overflows", fact.github_id)))?;
    let run_attempt = fact
        .run_attempt
        .map(i64::try_from)
        .transpose()
        .map_err(|_| AgentError::Store("workflow run attempt overflows".into()))?;
    let existing = get_workflow_run_fact(
        store,
        &fact.owner,
        &fact.host,
        &fact.repo_owner,
        &fact.repo_name,
        fact.github_id,
    )
    .await?;
    let changed = existing
        .as_ref()
        .is_none_or(|stored| stored.snapshot_differs(fact));
    entities::code_workflow_run::Entity::insert(entities::code_workflow_run::ActiveModel {
        id: Set(fact.id.0),
        owner: Set(fact.owner.as_str().to_owned()),
        host: Set(fact.host.clone()),
        repo_owner: Set(fact.repo_owner.clone()),
        repo_name: Set(fact.repo_name.clone()),
        github_id: Set(github_id),
        run_attempt: Set(run_attempt),
        name: Set(fact.name.clone()),
        url: Set(fact.url.clone()),
        status: Set(fact.status.clone()),
        conclusion: Set(fact.conclusion.clone()),
        workflow: Set(fact.workflow.clone()),
        branch: Set(fact.branch.clone()),
        sha: Set(fact.sha.clone()),
        event: Set(fact.event.clone()),
        actor: Set(fact.actor.clone()),
        created_at: Set(fact.created_at),
        updated_at: Set(fact.updated_at),
        first_seen_at: Set(fact.first_seen_at),
        last_seen_at: Set(fact.last_seen_at),
    })
    .on_conflict(
        OnConflict::columns([
            entities::code_workflow_run::Column::Owner,
            entities::code_workflow_run::Column::Host,
            entities::code_workflow_run::Column::RepoOwner,
            entities::code_workflow_run::Column::RepoName,
            entities::code_workflow_run::Column::GithubId,
        ])
        .update_columns([
            entities::code_workflow_run::Column::RunAttempt,
            entities::code_workflow_run::Column::Name,
            entities::code_workflow_run::Column::Url,
            entities::code_workflow_run::Column::Status,
            entities::code_workflow_run::Column::Conclusion,
            entities::code_workflow_run::Column::Workflow,
            entities::code_workflow_run::Column::Branch,
            entities::code_workflow_run::Column::Sha,
            entities::code_workflow_run::Column::Event,
            entities::code_workflow_run::Column::Actor,
            entities::code_workflow_run::Column::CreatedAt,
            entities::code_workflow_run::Column::UpdatedAt,
            entities::code_workflow_run::Column::LastSeenAt,
        ])
        .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    let row = find_fact_row(
        store,
        &fact.owner,
        &fact.host,
        &fact.repo_owner,
        &fact.repo_name,
        github_id,
    )
    .await?
    .ok_or_else(|| {
        AgentError::Store(format!(
            "workflow run {}/{}/{}/{} disappeared after upsert",
            fact.host, fact.repo_owner, fact.repo_name, fact.github_id
        ))
    })?;
    Ok((CodeWorkflowRunId(row.id), changed))
}

/// Load one observed workflow run by identity.
pub async fn get_workflow_run_fact(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    github_id: u64,
) -> Result<Option<CodeWorkflowRunFact>> {
    let github_id = i64::try_from(github_id)
        .map_err(|_| AgentError::Store(format!("workflow run id {github_id} overflows")))?;
    let Some(row) = find_fact_row(store, owner, host, repo_owner, repo_name, github_id).await?
    else {
        return Ok(None);
    };
    Ok(Some(fact_from_row(row)?))
}

/// Every observed workflow run on one repository identity, newest first.
pub async fn list_workflow_run_facts_for_repo(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<Vec<CodeWorkflowRunFact>> {
    entities::code_workflow_run::Entity::find()
        .filter(entities::code_workflow_run::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workflow_run::Column::Host.eq(host))
        .filter(entities::code_workflow_run::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_workflow_run::Column::RepoName.eq(repo_name))
        .order_by_desc(entities::code_workflow_run::Column::UpdatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fact_from_row)
        .collect()
}

/// The list-endpoint ETag last stored for one repository identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunFetchState {
    pub list_etag: Option<String>,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}

/// The condition that protects one list-fetch write.
#[derive(Debug, Clone, Copy)]
pub enum WorkflowRunFetchCondition<'a> {
    /// A fresh 200 response replaces the validator.
    Unconditional,
    /// A 304 response writes only if the row still holds the validator sent.
    ListEtag(Option<&'a str>),
}

/// Load the stored list ETag for one repository identity.
pub async fn get_workflow_run_fetch_state(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<Option<WorkflowRunFetchState>> {
    let Some(row) = entities::code_workflow_run_fetch::Entity::find()
        .filter(entities::code_workflow_run_fetch::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workflow_run_fetch::Column::Host.eq(host))
        .filter(entities::code_workflow_run_fetch::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_workflow_run_fetch::Column::RepoName.eq(repo_name))
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(None);
    };
    Ok(Some(WorkflowRunFetchState {
        list_etag: row.list_etag,
        observed_at: row.observed_at,
    }))
}

/// Store one conditional list-fetch pass.
///
/// A 304 passes `WorkflowRunFetchCondition::ListEtag` so a concurrent 200
/// that already moved the validator is not rolled back. `Ok(false)` when no
/// fetch row matches the identity and condition.
pub async fn set_workflow_run_fetch_state(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    list_etag: Option<&str>,
    observed_at: chrono::DateTime<chrono::Utc>,
    condition: WorkflowRunFetchCondition<'_>,
) -> Result<bool> {
    match condition {
        WorkflowRunFetchCondition::Unconditional => {
            entities::code_workflow_run_fetch::Entity::insert(
                entities::code_workflow_run_fetch::ActiveModel {
                    owner: Set(owner.as_str().to_owned()),
                    host: Set(host.to_owned()),
                    repo_owner: Set(repo_owner.to_owned()),
                    repo_name: Set(repo_name.to_owned()),
                    list_etag: Set(list_etag.map(ToOwned::to_owned)),
                    observed_at: Set(observed_at),
                },
            )
            .on_conflict(
                OnConflict::columns([
                    entities::code_workflow_run_fetch::Column::Owner,
                    entities::code_workflow_run_fetch::Column::Host,
                    entities::code_workflow_run_fetch::Column::RepoOwner,
                    entities::code_workflow_run_fetch::Column::RepoName,
                ])
                .update_columns([
                    entities::code_workflow_run_fetch::Column::ListEtag,
                    entities::code_workflow_run_fetch::Column::ObservedAt,
                ])
                .to_owned(),
            )
            .exec_without_returning(&store.conn)
            .await
            .map_err(store_err)?;
            Ok(true)
        }
        WorkflowRunFetchCondition::ListEtag(expected) => {
            let mut update = entities::code_workflow_run_fetch::Entity::update_many()
                .col_expr(
                    entities::code_workflow_run_fetch::Column::ObservedAt,
                    Expr::value(observed_at),
                )
                .filter(entities::code_workflow_run_fetch::Column::Owner.eq(owner.as_str()))
                .filter(entities::code_workflow_run_fetch::Column::Host.eq(host))
                .filter(entities::code_workflow_run_fetch::Column::RepoOwner.eq(repo_owner))
                .filter(entities::code_workflow_run_fetch::Column::RepoName.eq(repo_name));
            update = match expected {
                Some(etag) => {
                    update.filter(entities::code_workflow_run_fetch::Column::ListEtag.eq(etag))
                }
                None => {
                    update.filter(entities::code_workflow_run_fetch::Column::ListEtag.is_null())
                }
            };
            let result = update.exec(&store.conn).await.map_err(store_err)?;
            Ok(result.rows_affected == 1)
        }
    }
}

async fn find_fact_row(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    github_id: i64,
) -> Result<Option<entities::code_workflow_run::Model>> {
    entities::code_workflow_run::Entity::find()
        .filter(entities::code_workflow_run::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_workflow_run::Column::Host.eq(host))
        .filter(entities::code_workflow_run::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_workflow_run::Column::RepoName.eq(repo_name))
        .filter(entities::code_workflow_run::Column::GithubId.eq(github_id))
        .one(&store.conn)
        .await
        .map_err(store_err)
}

fn fact_from_row(row: entities::code_workflow_run::Model) -> Result<CodeWorkflowRunFact> {
    let github_id = u64::try_from(row.github_id)
        .map_err(|_| AgentError::Store(format!("workflow run {} id is negative", row.id)))?;
    let run_attempt = row
        .run_attempt
        .map(u64::try_from)
        .transpose()
        .map_err(|_| AgentError::Store(format!("workflow run {} attempt is negative", row.id)))?;
    Ok(CodeWorkflowRunFact {
        id: CodeWorkflowRunId(row.id),
        owner: OwnerId::new(&row.owner)?,
        host: row.host,
        repo_owner: row.repo_owner,
        repo_name: row.repo_name,
        github_id,
        run_attempt,
        name: row.name,
        url: row.url,
        status: row.status,
        conclusion: row.conclusion,
        workflow: row.workflow,
        branch: row.branch,
        sha: row.sha,
        event: row.event,
        actor: row.actor,
        created_at: row.created_at,
        updated_at: row.updated_at,
        first_seen_at: row.first_seen_at,
        last_seen_at: row.last_seen_at,
    })
}
