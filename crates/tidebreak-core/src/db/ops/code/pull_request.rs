//! Persistence for observed pull requests and workspace attribution.
//!
//! A `code_pull_request` row is a confirmed observation of one pull request,
//! keyed by full repository identity so a pull request in a repository with
//! no local checkout is representable. An attribution row ties a workspace
//! to a pull request it authored or contributed to (decision 62). GitHub
//! stays authoritative; these rows record what was observed and when.

use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
};

use crate::code::{
    CodePullRequestAttribution, CodePullRequestDiscovery, CodePullRequestFact, CodePullRequestId,
    CodePullRequestRelation, CodePullRequestState, WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// Insert or refresh one observed pull request, returning the canonical id.
///
/// Conflict is on the identity `(owner, host, repo_owner, repo_name,
/// number)`: an existing row keeps its id and its `first_seen_at`, and takes
/// the fresh snapshot plus `last_seen_at`. Trigger `pr_opened` edges key on
/// `first_seen_at`, which is why an upsert never moves it.
pub async fn upsert_pull_request_fact(
    store: &DbStore,
    fact: &CodePullRequestFact,
) -> Result<CodePullRequestId> {
    let number = i64::try_from(fact.number)
        .map_err(|_| AgentError::Store(format!("pull request number {} overflows", fact.number)))?;
    entities::code_pull_request::Entity::insert(entities::code_pull_request::ActiveModel {
        id: Set(fact.id.0),
        owner: Set(fact.owner.as_str().to_owned()),
        host: Set(fact.host.clone()),
        repo_owner: Set(fact.repo_owner.clone()),
        repo_name: Set(fact.repo_name.clone()),
        number: Set(number),
        url: Set(fact.url.clone()),
        title: Set(fact.title.clone()),
        state: Set(fact.state.as_str().to_owned()),
        draft: Set(fact.draft),
        author: Set(fact.author.clone()),
        head_branch: Set(fact.head_branch.clone()),
        base_branch: Set(fact.base_branch.clone()),
        head_sha: Set(fact.head_sha.clone()),
        created_at: Set(fact.created_at),
        updated_at: Set(fact.updated_at),
        merged_at: Set(fact.merged_at),
        closed_at: Set(fact.closed_at),
        first_seen_at: Set(fact.first_seen_at),
        last_seen_at: Set(fact.last_seen_at),
    })
    .on_conflict(
        OnConflict::columns([
            entities::code_pull_request::Column::Owner,
            entities::code_pull_request::Column::Host,
            entities::code_pull_request::Column::RepoOwner,
            entities::code_pull_request::Column::RepoName,
            entities::code_pull_request::Column::Number,
        ])
        .update_columns([
            entities::code_pull_request::Column::Url,
            entities::code_pull_request::Column::Title,
            entities::code_pull_request::Column::State,
            entities::code_pull_request::Column::Draft,
            entities::code_pull_request::Column::Author,
            entities::code_pull_request::Column::HeadBranch,
            entities::code_pull_request::Column::BaseBranch,
            entities::code_pull_request::Column::HeadSha,
            entities::code_pull_request::Column::CreatedAt,
            entities::code_pull_request::Column::UpdatedAt,
            entities::code_pull_request::Column::MergedAt,
            entities::code_pull_request::Column::ClosedAt,
            entities::code_pull_request::Column::LastSeenAt,
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
        number,
    )
    .await?
    .ok_or_else(|| {
        AgentError::Store(format!(
            "pull request {}/{}/{}#{} disappeared after upsert",
            fact.host, fact.repo_owner, fact.repo_name, fact.number
        ))
    })?;
    Ok(CodePullRequestId(row.id))
}

/// Load one observed pull request by identity.
pub async fn get_pull_request_fact(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: u64,
) -> Result<Option<CodePullRequestFact>> {
    let number = i64::try_from(number)
        .map_err(|_| AgentError::Store(format!("pull request number {number} overflows")))?;
    let Some(row) = find_fact_row(store, owner, host, repo_owner, repo_name, number).await? else {
        return Ok(None);
    };
    Ok(Some(fact_from_row(row)?))
}

/// Every observed pull request on one repository identity.
pub async fn list_pull_request_facts_for_repo(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<Vec<CodePullRequestFact>> {
    entities::code_pull_request::Entity::find()
        .filter(entities::code_pull_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request::Column::Host.eq(host))
        .filter(entities::code_pull_request::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_pull_request::Column::RepoName.eq(repo_name))
        .order_by_desc(entities::code_pull_request::Column::UpdatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fact_from_row)
        .collect()
}

/// Every distinct repository identity holding at least one fact row.
///
/// The reconcile sweep reads this to keep cross-repo facts fresh: a
/// repository discovered through a detected command keeps itself on the
/// sweep's list without a local checkout.
pub async fn list_fact_repo_identities(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<(String, String, String)>> {
    let rows: Vec<(String, String, String)> = entities::code_pull_request::Entity::find()
        .select_only()
        .column(entities::code_pull_request::Column::Host)
        .column(entities::code_pull_request::Column::RepoOwner)
        .column(entities::code_pull_request::Column::RepoName)
        .distinct()
        .filter(entities::code_pull_request::Column::Owner.eq(owner.as_str()))
        .into_tuple()
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows)
}

/// Every owner holding at least one fact row. A system path for the sweep.
pub async fn list_fact_owners_all(store: &DbStore) -> Result<Vec<String>> {
    let rows: Vec<String> = entities::code_pull_request::Entity::find()
        .select_only()
        .column(entities::code_pull_request::Column::Owner)
        .distinct()
        .into_tuple()
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(rows)
}

/// Mint one workspace's tie to a pull request. Returns `true` when this call
/// created the row; an existing `(pull_request, workspace)` row wins and the
/// call reports `false` without touching it.
pub async fn claim_pull_request_attribution(
    store: &DbStore,
    attribution: &CodePullRequestAttribution,
) -> Result<bool> {
    let result = entities::code_pull_request_attribution::Entity::insert(
        entities::code_pull_request_attribution::ActiveModel {
            owner: Set(attribution.owner.as_str().to_owned()),
            pull_request_id: Set(attribution.pull_request_id.0),
            workspace_id: Set(attribution.workspace_id.0),
            relation: Set(attribution.relation.as_str().to_owned()),
            discovered_via: Set(attribution.discovered_via.as_str().to_owned()),
            session_id: Set(attribution.session_id.map(|id| id.0)),
            parent_call_id: Set(attribution.parent_call_id.clone()),
            created_at: Set(attribution.created_at),
        },
    )
    .on_conflict(
        OnConflict::columns([
            entities::code_pull_request_attribution::Column::PullRequestId,
            entities::code_pull_request_attribution::Column::WorkspaceId,
        ])
        .do_nothing()
        .to_owned(),
    )
    .exec_without_returning(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(result == 1)
}

/// Upgrade an existing contributed attribution to authored.
///
/// The relation holds the strongest claim: a push observed before the create
/// leaves a contributed row, and the create's confirmation upgrades it.
pub async fn promote_attribution_to_authored(
    store: &DbStore,
    owner: &OwnerId,
    pull_request_id: CodePullRequestId,
    workspace_id: WorkspaceId,
) -> Result<()> {
    entities::code_pull_request_attribution::Entity::update_many()
        .col_expr(
            entities::code_pull_request_attribution::Column::Relation,
            sea_orm::sea_query::Expr::value(CodePullRequestRelation::Authored.as_str()),
        )
        .filter(entities::code_pull_request_attribution::Column::Owner.eq(owner.as_str()))
        .filter(
            entities::code_pull_request_attribution::Column::PullRequestId.eq(pull_request_id.0),
        )
        .filter(entities::code_pull_request_attribution::Column::WorkspaceId.eq(workspace_id.0))
        .filter(
            entities::code_pull_request_attribution::Column::Relation
                .eq(CodePullRequestRelation::Contributed.as_str()),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// Every pull request attributed to one workspace, with the relation.
pub async fn list_attributed_facts_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<Vec<(CodePullRequestFact, CodePullRequestRelation)>> {
    let attributions = entities::code_pull_request_attribution::Entity::find()
        .filter(entities::code_pull_request_attribution::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request_attribution::Column::WorkspaceId.eq(workspace_id.0))
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    if attributions.is_empty() {
        return Ok(Vec::new());
    }
    let relations: std::collections::HashMap<uuid::Uuid, CodePullRequestRelation> = attributions
        .iter()
        .map(|row| {
            let relation = CodePullRequestRelation::from_str(&row.relation).ok_or_else(|| {
                AgentError::Store(format!(
                    "pull request attribution relation {} is unknown",
                    row.relation
                ))
            })?;
            Ok((row.pull_request_id, relation))
        })
        .collect::<Result<_>>()?;
    let ids: Vec<uuid::Uuid> = relations.keys().copied().collect();
    let mut facts = entities::code_pull_request::Entity::find()
        .filter(entities::code_pull_request::Column::Id.is_in(ids))
        .order_by_desc(entities::code_pull_request::Column::UpdatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fact_from_row)
        .collect::<Result<Vec<_>>>()?;
    facts.retain(|fact| relations.contains_key(&fact.id.0));
    Ok(facts
        .into_iter()
        .map(|fact| {
            let relation = relations[&fact.id.0];
            (fact, relation)
        })
        .collect())
}

/// How many pull requests are attributed to one workspace.
pub async fn count_attributed_prs_for_workspace(
    store: &DbStore,
    owner: &OwnerId,
    workspace_id: WorkspaceId,
) -> Result<u64> {
    entities::code_pull_request_attribution::Entity::find()
        .filter(entities::code_pull_request_attribution::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request_attribution::Column::WorkspaceId.eq(workspace_id.0))
        .count(&store.conn)
        .await
        .map_err(store_err)
}

/// Every attribution row on a set of pull requests.
pub async fn list_attributions_for_pull_requests(
    store: &DbStore,
    owner: &OwnerId,
    ids: &[CodePullRequestId],
) -> Result<Vec<CodePullRequestAttribution>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let raw: Vec<uuid::Uuid> = ids.iter().map(|id| id.0).collect();
    entities::code_pull_request_attribution::Entity::find()
        .filter(entities::code_pull_request_attribution::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request_attribution::Column::PullRequestId.is_in(raw))
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(attribution_from_row)
        .collect()
}

async fn find_fact_row(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<Option<entities::code_pull_request::Model>> {
    entities::code_pull_request::Entity::find()
        .filter(entities::code_pull_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request::Column::Host.eq(host))
        .filter(entities::code_pull_request::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_pull_request::Column::RepoName.eq(repo_name))
        .filter(entities::code_pull_request::Column::Number.eq(number))
        .one(&store.conn)
        .await
        .map_err(store_err)
}

fn fact_from_row(row: entities::code_pull_request::Model) -> Result<CodePullRequestFact> {
    let state = CodePullRequestState::from_str(&row.state).ok_or_else(|| {
        AgentError::Store(format!(
            "pull request {} state {} is unknown",
            row.id, row.state
        ))
    })?;
    let number = u64::try_from(row.number)
        .map_err(|_| AgentError::Store(format!("pull request {} number is negative", row.id)))?;
    Ok(CodePullRequestFact {
        id: CodePullRequestId(row.id),
        owner: OwnerId::new(&row.owner)?,
        host: row.host,
        repo_owner: row.repo_owner,
        repo_name: row.repo_name,
        number,
        url: row.url,
        title: row.title,
        state,
        draft: row.draft,
        author: row.author,
        head_branch: row.head_branch,
        base_branch: row.base_branch,
        head_sha: row.head_sha,
        created_at: row.created_at,
        updated_at: row.updated_at,
        merged_at: row.merged_at,
        closed_at: row.closed_at,
        first_seen_at: row.first_seen_at,
        last_seen_at: row.last_seen_at,
    })
}

fn attribution_from_row(
    row: entities::code_pull_request_attribution::Model,
) -> Result<CodePullRequestAttribution> {
    let relation = CodePullRequestRelation::from_str(&row.relation).ok_or_else(|| {
        AgentError::Store(format!(
            "pull request attribution relation {} is unknown",
            row.relation
        ))
    })?;
    let discovered_via =
        CodePullRequestDiscovery::from_str(&row.discovered_via).ok_or_else(|| {
            AgentError::Store(format!(
                "pull request attribution discovery {} is unknown",
                row.discovered_via
            ))
        })?;
    Ok(CodePullRequestAttribution {
        owner: OwnerId::new(&row.owner)?,
        pull_request_id: CodePullRequestId(row.pull_request_id),
        workspace_id: WorkspaceId(row.workspace_id),
        relation,
        discovered_via,
        session_id: row.session_id.map(crate::code::CodeSessionId),
        parent_call_id: row.parent_call_id,
        created_at: row.created_at,
    })
}
