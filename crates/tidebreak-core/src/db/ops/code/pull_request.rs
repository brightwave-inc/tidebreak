//! Persistence for observed pull requests and workspace attribution.
//!
//! A `code_pull_request` row is a confirmed observation of one pull request,
//! keyed by full repository identity so a pull request in a repository with
//! no local checkout is representable. An attribution row ties a workspace
//! to a pull request it authored or contributed to (decision 77). GitHub
//! stays authoritative; these rows record what was observed and when.
//!
//! Every write of pull-request state goes through [`save_pull_request_read`]:
//! one transaction that locks the row, merges the read into it with
//! [`merge_pull_request_read`], writes the result, and rewrites the
//! pull-request column of every active workspace that shows it. The column is
//! a projection of the row, so no reader can leave it older than the row.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::code::{
    merge_pull_request_read, CodePullRequestAttribution, CodePullRequestDiscovery,
    CodePullRequestFact, CodePullRequestId, CodePullRequestLiveState, CodePullRequestRelation,
    CodePullRequestState, CodeWorkspaceStatus, PullRequestDigest, PullRequestEtags,
    PullRequestObservedTimes, PullRequestRead, StoredPullRequest, WorkspaceId,
};
use crate::error::{AgentError, Result};
use crate::OwnerId;

use super::super::super::{entities, store_err, DbStore};

/// How [`save_pull_request_read`] treats a pull request with no row yet, and
/// which workspace takes the result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullRequestReadOptions {
    /// Create the row when none exists. Readers that track a pull request
    /// (decision 77) mint; the conditional fetcher does not, so a workspace
    /// looking at a pull request never makes it tracked by looking.
    pub mint_row: bool,
    /// The workspace whose pull-request column should show this pull
    /// request even when it shows another one now. Every other active
    /// workspace takes the result only when its column already shows this
    /// pull request.
    pub adopt: Option<WorkspaceId>,
}

/// What one applied read did.
#[derive(Debug, Clone)]
pub struct AppliedPullRequestRead {
    /// The pull request after the merge. When `stored` is false this is the
    /// read's own first sighting, held nowhere but the adopting workspace's
    /// column.
    pub fact: CodePullRequestFact,
    /// Whether the row exists: it did already, or this read minted it.
    pub stored: bool,
    /// Whether a field a reader sees moved. Confirmations report no change.
    pub changed: bool,
    /// The active workspaces whose pull-request column this read rewrote.
    pub workspaces: Vec<WorkspaceId>,
}

/// Merge one read into its pull request's row, in one transaction.
///
/// The transaction locks the row, merges the read with
/// [`merge_pull_request_read`], writes the merged row, and projects it into
/// the pull-request column of every active workspace that shows this pull
/// request, plus the adopting workspace. Reads therefore land in commit
/// order, and a late or partial read cannot erase what a newer or fuller
/// one stored.
///
/// With no row yet, a read that loaded the pull request object mints one
/// when [`PullRequestReadOptions::mint_row`] allows. Otherwise the read's own
/// first sighting goes to the adopting workspace's column only, and nothing
/// is stored. `Ok(None)` when there is no row and the read cannot make one.
pub async fn save_pull_request_read(
    store: &DbStore,
    read: &PullRequestRead,
    options: PullRequestReadOptions,
) -> Result<Option<AppliedPullRequestRead>> {
    let number = i64::try_from(read.number)
        .map_err(|_| AgentError::Store(format!("pull request number {} overflows", read.number)))?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    match apply_on(&transaction, read, number, options).await {
        Ok(applied) => {
            transaction.commit().await.map_err(store_err)?;
            Ok(applied)
        }
        Err(err) => {
            let _ = transaction.rollback().await;
            Err(err)
        }
    }
}

async fn apply_on<C>(
    conn: &C,
    read: &PullRequestRead,
    number: i64,
    options: PullRequestReadOptions,
) -> Result<Option<AppliedPullRequestRead>>
where
    C: ConnectionTrait,
{
    let (before, merged) = match lock_fact_row(conn, read, number).await? {
        Some(row) => merge_row(conn, row, read).await?,
        None => {
            let Some(first) = StoredPullRequest::first_sighting(read, CodePullRequestId::new())
            else {
                return Ok(None);
            };
            if !options.mint_row {
                let workspaces =
                    project_into_workspaces(conn, &first.fact, options.adopt, true).await?;
                return Ok(Some(AppliedPullRequestRead {
                    fact: first.fact,
                    stored: false,
                    changed: true,
                    workspaces,
                }));
            }
            if insert_stored(conn, &first).await? {
                (None, first)
            } else {
                // Another writer minted the row first; merge onto theirs.
                let row = lock_fact_row(conn, read, number).await?.ok_or_else(|| {
                    AgentError::Store(format!(
                        "pull request {}/{}/{}#{} disappeared after a conflicting insert",
                        read.host, read.repo_owner, read.repo_name, read.number
                    ))
                })?;
                merge_row(conn, row, read).await?
            }
        }
    };
    let changed = before
        .as_ref()
        .is_none_or(|before| merged.visibly_differs(before));
    let workspaces = if changed || options.adopt.is_some() {
        project_into_workspaces(conn, &merged.fact, options.adopt, false).await?
    } else {
        Vec::new()
    };
    Ok(Some(AppliedPullRequestRead {
        fact: merged.fact,
        stored: true,
        changed,
        workspaces,
    }))
}

/// Merge `read` into the locked `row` and write the result when it moved.
async fn merge_row<C>(
    conn: &C,
    row: entities::code_pull_request::Model,
    read: &PullRequestRead,
) -> Result<(Option<StoredPullRequest>, StoredPullRequest)>
where
    C: ConnectionTrait,
{
    let before = stored_from_row(row)?;
    let merged = merge_pull_request_read(&before, read);
    if merged != before {
        write_stored(conn, &merged).await?;
    }
    Ok((Some(before), merged))
}

/// Lock one pull request's row for the rest of the transaction and load it.
///
/// A no-op update takes PostgreSQL's row lock; SQLite's `BEGIN IMMEDIATE`
/// already holds the database write lock. `None` when no row exists.
async fn lock_fact_row<C>(
    conn: &C,
    read: &PullRequestRead,
    number: i64,
) -> Result<Option<entities::code_pull_request::Model>>
where
    C: ConnectionTrait,
{
    let locked = entities::code_pull_request::Entity::update_many()
        .col_expr(
            entities::code_pull_request::Column::Number,
            Expr::col(entities::code_pull_request::Column::Number),
        )
        .filter(entities::code_pull_request::Column::Owner.eq(read.owner.as_str()))
        .filter(entities::code_pull_request::Column::Host.eq(read.host.as_str()))
        .filter(entities::code_pull_request::Column::RepoOwner.eq(read.repo_owner.as_str()))
        .filter(entities::code_pull_request::Column::RepoName.eq(read.repo_name.as_str()))
        .filter(entities::code_pull_request::Column::Number.eq(number))
        .exec(conn)
        .await
        .map_err(store_err)?;
    if locked.rows_affected == 0 {
        return Ok(None);
    }
    find_fact_row(
        conn,
        &read.owner,
        &read.host,
        &read.repo_owner,
        &read.repo_name,
        number,
    )
    .await
}

/// Insert a first sighting. `false` when another writer inserted the same
/// identity first.
async fn insert_stored<C>(conn: &C, stored: &StoredPullRequest) -> Result<bool>
where
    C: ConnectionTrait,
{
    let fact = &stored.fact;
    let number = i64::try_from(fact.number)
        .map_err(|_| AgentError::Store(format!("pull request number {} overflows", fact.number)))?;
    let live = LiveColumns::of(stored)?;
    let inserted =
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
            checks_summary: Set(live.checks_summary),
            checks: Set(live.checks),
            review_decision: Set(live.review_decision),
            mergeable: Set(live.mergeable),
            merge_state_status: Set(live.merge_state_status),
            auto_merge_enabled: Set(live.auto_merge_enabled),
            in_merge_queue: Set(live.in_merge_queue),
            live_observed_at: Set(live.observed_at),
            pull_etag: Set(stored.etags.pull.clone()),
            checks_etag: Set(stored.etags.checks.clone()),
            reviews_etag: Set(stored.etags.reviews.clone()),
            checks_observed_at: Set(stored.observed.checks),
            review_observed_at: Set(stored.observed.review),
            mergeability_observed_at: Set(stored.observed.mergeability),
            auto_merge_observed_at: Set(stored.observed.auto_merge),
            queue_observed_at: Set(stored.observed.queue),
        })
        .on_conflict(
            OnConflict::columns([
                entities::code_pull_request::Column::Owner,
                entities::code_pull_request::Column::Host,
                entities::code_pull_request::Column::RepoOwner,
                entities::code_pull_request::Column::RepoName,
                entities::code_pull_request::Column::Number,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    Ok(inserted == 1)
}

/// Write every column the merge owns. Identity, `id`, and `first_seen_at`
/// never move.
async fn write_stored<C>(conn: &C, stored: &StoredPullRequest) -> Result<()>
where
    C: ConnectionTrait,
{
    use entities::code_pull_request::Column;
    let fact = &stored.fact;
    let live = LiveColumns::of(stored)?;
    entities::code_pull_request::Entity::update_many()
        .col_expr(Column::Url, Expr::value(fact.url.clone()))
        .col_expr(Column::Title, Expr::value(fact.title.clone()))
        .col_expr(Column::State, Expr::value(fact.state.as_str().to_owned()))
        .col_expr(Column::Draft, Expr::value(fact.draft))
        .col_expr(Column::Author, Expr::value(fact.author.clone()))
        .col_expr(Column::HeadBranch, Expr::value(fact.head_branch.clone()))
        .col_expr(Column::BaseBranch, Expr::value(fact.base_branch.clone()))
        .col_expr(Column::HeadSha, Expr::value(fact.head_sha.clone()))
        .col_expr(Column::CreatedAt, Expr::value(fact.created_at))
        .col_expr(Column::UpdatedAt, Expr::value(fact.updated_at))
        .col_expr(Column::MergedAt, Expr::value(fact.merged_at))
        .col_expr(Column::ClosedAt, Expr::value(fact.closed_at))
        .col_expr(Column::LastSeenAt, Expr::value(fact.last_seen_at))
        .col_expr(Column::ChecksSummary, Expr::value(live.checks_summary))
        .col_expr(Column::Checks, Expr::value(live.checks))
        .col_expr(Column::ReviewDecision, Expr::value(live.review_decision))
        .col_expr(Column::Mergeable, Expr::value(live.mergeable))
        .col_expr(
            Column::MergeStateStatus,
            Expr::value(live.merge_state_status),
        )
        .col_expr(
            Column::AutoMergeEnabled,
            Expr::value(live.auto_merge_enabled),
        )
        .col_expr(Column::InMergeQueue, Expr::value(live.in_merge_queue))
        .col_expr(Column::LiveObservedAt, Expr::value(live.observed_at))
        .col_expr(Column::PullEtag, Expr::value(stored.etags.pull.clone()))
        .col_expr(Column::ChecksEtag, Expr::value(stored.etags.checks.clone()))
        .col_expr(
            Column::ReviewsEtag,
            Expr::value(stored.etags.reviews.clone()),
        )
        .col_expr(
            Column::ChecksObservedAt,
            Expr::value(stored.observed.checks),
        )
        .col_expr(
            Column::ReviewObservedAt,
            Expr::value(stored.observed.review),
        )
        .col_expr(
            Column::MergeabilityObservedAt,
            Expr::value(stored.observed.mergeability),
        )
        .col_expr(
            Column::AutoMergeObservedAt,
            Expr::value(stored.observed.auto_merge),
        )
        .col_expr(Column::QueueObservedAt, Expr::value(stored.observed.queue))
        .filter(Column::Id.eq(fact.id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

/// The live tier as stored columns. The check list travels as JSON text.
struct LiveColumns {
    checks_summary: Option<String>,
    checks: Option<String>,
    review_decision: Option<String>,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    auto_merge_enabled: Option<bool>,
    in_merge_queue: Option<bool>,
    observed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl LiveColumns {
    fn of(stored: &StoredPullRequest) -> Result<Self> {
        let Some(live) = stored.fact.live.as_ref() else {
            return Ok(Self {
                checks_summary: None,
                checks: None,
                review_decision: None,
                mergeable: None,
                merge_state_status: None,
                auto_merge_enabled: None,
                in_merge_queue: None,
                observed_at: None,
            });
        };
        let checks = live
            .checks
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| {
                AgentError::Store(format!(
                    "pull request {} live checks are unwritable: {err}",
                    stored.fact.id.0
                ))
            })?;
        Ok(Self {
            checks_summary: live.checks_summary.clone(),
            checks,
            review_decision: live.review_decision.clone(),
            mergeable: live.mergeable.clone(),
            merge_state_status: live.merge_state_status.clone(),
            auto_merge_enabled: live.auto_merge_enabled,
            in_merge_queue: live.in_merge_queue,
            observed_at: Some(live.observed_at),
        })
    }
}

/// Rewrite the pull-request column of every active workspace that shows
/// `fact`, and of `adopt`, with the fact's digest. With `adopt_only`, only
/// `adopt` takes it. Returns the workspaces whose column changed.
async fn project_into_workspaces<C>(
    conn: &C,
    fact: &CodePullRequestFact,
    adopt: Option<WorkspaceId>,
    adopt_only: bool,
) -> Result<Vec<WorkspaceId>>
where
    C: ConnectionTrait,
{
    if adopt_only && adopt.is_none() {
        return Ok(Vec::new());
    }
    let digest = fact.digest();
    let encoded = serde_json::to_value(&digest)?;
    let mut query = entities::code_workspace::Entity::find()
        .filter(entities::code_workspace::Column::Owner.eq(fact.owner.as_str()))
        .filter(entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Active.as_str()));
    if adopt_only {
        if let Some(adopt) = adopt {
            query = query.filter(entities::code_workspace::Column::Id.eq(adopt.0));
        }
    }
    let rows = query.all(conn).await.map_err(store_err)?;
    let mut rewritten = Vec::new();
    for row in rows {
        let id = WorkspaceId(row.id);
        let shows =
            digest_url(row.pr.as_ref()).is_some_and(|url| url.eq_ignore_ascii_case(&fact.url));
        if adopt != Some(id) && (adopt_only || !shows) {
            continue;
        }
        let current: Option<PullRequestDigest> = row
            .pr
            .as_ref()
            .and_then(|value| serde_json::from_value(value.clone()).ok());
        if current.as_ref() == Some(&digest) {
            continue;
        }
        let result = entities::code_workspace::Entity::update_many()
            .col_expr(
                entities::code_workspace::Column::Pr,
                Expr::value(Some(encoded.clone())),
            )
            .filter(entities::code_workspace::Column::Id.eq(row.id))
            .filter(entities::code_workspace::Column::Owner.eq(fact.owner.as_str()))
            .filter(
                entities::code_workspace::Column::Status.eq(CodeWorkspaceStatus::Active.as_str()),
            )
            .exec(conn)
            .await
            .map_err(store_err)?;
        if result.rows_affected == 1 {
            rewritten.push(id);
        }
    }
    Ok(rewritten)
}

/// The pull request URL a stored workspace column names.
pub(super) fn digest_url(pr: Option<&serde_json::Value>) -> Option<&str> {
    pr?.get("url")?.as_str()
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
    let Some(row) = find_fact_row(&store.conn, owner, host, repo_owner, repo_name, number).await?
    else {
        return Ok(None);
    };
    Ok(Some(fact_from_row(row)?))
}

/// Load one observed pull request with the observation times and validators
/// the merge and the conditional fetcher use.
pub async fn get_stored_pull_request(
    store: &DbStore,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: u64,
) -> Result<Option<StoredPullRequest>> {
    let number = i64::try_from(number)
        .map_err(|_| AgentError::Store(format!("pull request number {number} overflows")))?;
    let Some(row) = find_fact_row(&store.conn, owner, host, repo_owner, repo_name, number).await?
    else {
        return Ok(None);
    };
    Ok(Some(stored_from_row(row)?))
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

/// Every pull request observed for one owner, newest first.
pub async fn list_pull_request_facts(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<CodePullRequestFact>> {
    entities::code_pull_request::Entity::find()
        .filter(entities::code_pull_request::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_pull_request::Column::UpdatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(fact_from_row)
        .collect()
}

/// Every distinct `(owner, host, repo_owner, repo_name)` holding at least
/// one fact row.
///
/// A system path, not a request path: the reconcile sweep walks every
/// tracked repository identity regardless of who observed it, then scopes
/// each read to the row's owner. Nothing reachable from a route may call it.
pub async fn list_fact_repo_identities_all_owners(
    store: &DbStore,
) -> Result<Vec<(String, String, String, String)>> {
    let rows: Vec<(String, String, String, String)> = entities::code_pull_request::Entity::find()
        .select_only()
        .column(entities::code_pull_request::Column::Owner)
        .column(entities::code_pull_request::Column::Host)
        .column(entities::code_pull_request::Column::RepoOwner)
        .column(entities::code_pull_request::Column::RepoName)
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
pub async fn insert_pull_request_attribution(
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

/// Every pull-request attribution that belongs to one owner.
pub async fn list_pull_request_attributions(
    store: &DbStore,
    owner: &OwnerId,
) -> Result<Vec<CodePullRequestAttribution>> {
    entities::code_pull_request_attribution::Entity::find()
        .filter(entities::code_pull_request_attribution::Column::Owner.eq(owner.as_str()))
        .order_by_desc(entities::code_pull_request_attribution::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(attribution_from_row)
        .collect()
}

async fn find_fact_row<C>(
    conn: &C,
    owner: &OwnerId,
    host: &str,
    repo_owner: &str,
    repo_name: &str,
    number: i64,
) -> Result<Option<entities::code_pull_request::Model>>
where
    C: ConnectionTrait,
{
    entities::code_pull_request::Entity::find()
        .filter(entities::code_pull_request::Column::Owner.eq(owner.as_str()))
        .filter(entities::code_pull_request::Column::Host.eq(host))
        .filter(entities::code_pull_request::Column::RepoOwner.eq(repo_owner))
        .filter(entities::code_pull_request::Column::RepoName.eq(repo_name))
        .filter(entities::code_pull_request::Column::Number.eq(number))
        .one(conn)
        .await
        .map_err(store_err)
}

fn stored_from_row(row: entities::code_pull_request::Model) -> Result<StoredPullRequest> {
    let observed = PullRequestObservedTimes {
        checks: row.checks_observed_at,
        review: row.review_observed_at,
        mergeability: row.mergeability_observed_at,
        auto_merge: row.auto_merge_observed_at,
        queue: row.queue_observed_at,
    };
    let etags = PullRequestEtags {
        pull: row.pull_etag.clone(),
        checks: row.checks_etag.clone(),
        reviews: row.reviews_etag.clone(),
    };
    Ok(StoredPullRequest {
        fact: fact_from_row(row)?,
        observed,
        etags,
    })
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
    let live = match row.live_observed_at {
        Some(observed_at) => Some(CodePullRequestLiveState {
            checks_summary: row.checks_summary,
            checks: match row.checks.as_deref() {
                Some(raw) => Some(serde_json::from_str(raw).map_err(|err| {
                    AgentError::Store(format!(
                        "pull request {} live checks are unreadable: {err}",
                        row.id
                    ))
                })?),
                None => None,
            },
            review_decision: row.review_decision,
            mergeable: row.mergeable,
            merge_state_status: row.merge_state_status,
            auto_merge_enabled: row.auto_merge_enabled,
            in_merge_queue: row.in_merge_queue,
            observed_at,
        }),
        None => None,
    };
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
        live,
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
        session_id: row.session_id.map(crate::code::SessionId),
        parent_call_id: row.parent_call_id,
        created_at: row.created_at,
    })
}
