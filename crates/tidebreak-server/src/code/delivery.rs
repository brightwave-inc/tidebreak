//! Install-wide GitHub delivery reads and guarded user actions.
//!
//! The database remains the source of truth for registered repositories and
//! Tidebreak workspaces. Remote pull requests, Actions runs, and deployments
//! are live GitHub observations held only in short in-memory caches.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::{stream, StreamExt};
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use tidebreak_core::{
    CodeRepo, CodeWorkspace, CodeWorkspaceStatus, OwnerId, PullRequestCheckBucket,
    PullRequestComment, PullRequestCommentKind, RepoId,
};

use super::gh::{self, GhObservation};
use super::runtime::CodeRuntime;
use crate::error::ServerError;
use crate::routes::code::types::{
    CodeDeliveryActionResult, CodeDeliveryCheck, CodeDeliveryDeploymentStatus,
    CodeDeliveryPrAttentionReason, CodeDeliveryPullRequestAction,
    CodeDeliveryPullRequestActionBody, CodeDeliveryPullRequestDetail, CodeDeliveryPullRequestFile,
    CodeDeliveryPullRequestQuery, CodeDeliveryPullRequestSummary, CodeDeliveryPullRequestTarget,
    CodeDeliveryPullRequestsPage, CodeDeliveryRepositoriesSnapshot, CodeDeliveryRerunOutcome,
    CodeDeliveryRunAction, CodeDeliveryRunActionBody, CodeDeliveryRunAttentionReason,
    CodeDeliveryRunDetail, CodeDeliveryRunKind, CodeDeliveryRunQuery, CodeDeliveryRunSummary,
    CodeDeliveryRunTarget, CodeDeliveryRunsPage, CodeDeliverySourceError, CodeDeliveryWorkflowJob,
    CodeDeliveryWorkspaceLink, CodeGitHubCapability, CodeGitHubRepositoryRef,
    CodeGitHubRepositoryTarget, CodePrMergeMethod, ResolveCodeDeliveryRepositoriesBody,
};

const GH_READ_TIMEOUT: Duration = Duration::from_secs(45);
const GIT_READ_TIMEOUT: Duration = Duration::from_secs(10);
const LIST_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_REPOSITORIES: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
const MAX_REMOTE_ITEMS_PER_REPO: usize = 100;
const DELIVERY_CONCURRENCY: usize = 4;
const MAX_COMMENT_BYTES: usize = 60_000;
/// Files rendered in the detail panel. The panel is a review aid rather than
/// a diff viewer, and GitHub itself stops rendering a diff well past this.
const MAX_DETAIL_FILES: usize = 300;
const GITHUB_DETAIL_PAGE_SIZE: usize = 100;
/// Transient GitHub failures (502/503/504, gateway timeouts) get one retry
/// after a short pause. A cross-repository list fans out far enough that one
/// unlucky repository would otherwise blank a whole column.
const TRANSIENT_RETRY_DELAY: Duration = Duration::from_millis(700);

const PR_LIST_FIELDS: &str = "number,url,state,title,isDraft,author,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRefName,headRefOid,baseRefName,updatedAt,createdAt,mergedAt,closedAt,labels";
const PR_LIST_FIELDS_WITH_CHECKS: &str = "number,url,state,title,isDraft,author,reviewDecision,mergeable,mergeStateStatus,autoMergeRequest,headRefName,headRefOid,baseRefName,updatedAt,createdAt,mergedAt,closedAt,labels,statusCheckRollup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PullRequestRemotePlan {
    state: &'static str,
    fields: &'static str,
    checks_loaded: bool,
}

impl PullRequestRemotePlan {
    fn cache_scope(self) -> String {
        format!(
            "{}:{}",
            self.state,
            if self.checks_loaded {
                "checks"
            } else {
                "summary"
            }
        )
    }
}

#[derive(Debug, Clone)]
struct CachedAggregate<T> {
    fetched_at: Instant,
    items: Vec<T>,
    errors: Vec<CodeDeliverySourceError>,
}

#[derive(Debug, Clone)]
struct CachedValue<T> {
    fetched_at: Instant,
    value: T,
}

#[derive(Debug, Clone)]
struct OwnerRepositoryEntry {
    repo: CodeRepo,
    target: CodeGitHubRepositoryTarget,
}

#[derive(Debug, Clone, Default)]
struct OwnerRepositoryCatalog {
    entries: Vec<OwnerRepositoryEntry>,
    errors: Vec<CodeDeliverySourceError>,
}

#[derive(Debug, Default)]
struct FetchedRuns {
    items: Vec<CodeDeliveryRunSummary>,
    errors: Vec<CodeDeliverySourceError>,
}

/// Short-lived owner/query caches. No GitHub response is durable.
#[derive(Debug, Default)]
pub(crate) struct DeliveryCache {
    pull_requests: Mutex<HashMap<String, CachedAggregate<CodeDeliveryPullRequestSummary>>>,
    runs: Mutex<HashMap<String, CachedAggregate<CodeDeliveryRunSummary>>>,
    pull_request_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    run_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    repositories: Mutex<HashMap<String, CachedValue<CodeGitHubRepositoryRef>>>,
    repository_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    owner_repositories: Mutex<HashMap<String, CachedValue<OwnerRepositoryCatalog>>>,
    owner_repository_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    workspace_indexes: Mutex<HashMap<String, CachedValue<Vec<WorkspaceIndexEntry>>>>,
    workspace_index_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    owner_cache_generations: Mutex<HashMap<String, u64>>,
}

impl DeliveryCache {
    fn pull_requests(&self, key: &str) -> Option<CachedAggregate<CodeDeliveryPullRequestSummary>> {
        let mut cache = self.pull_requests.lock().expect("delivery PR cache");
        cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
        cache.get(key).cloned()
    }

    fn put_pull_requests(
        &self,
        key: String,
        items: Vec<CodeDeliveryPullRequestSummary>,
        errors: Vec<CodeDeliverySourceError>,
    ) {
        self.pull_requests
            .lock()
            .expect("delivery PR cache")
            .insert(
                key,
                CachedAggregate {
                    fetched_at: Instant::now(),
                    items,
                    errors,
                },
            );
    }

    fn pull_request_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.pull_request_reads
            .lock()
            .expect("delivery PR read locks")
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn runs(&self, key: &str) -> Option<CachedAggregate<CodeDeliveryRunSummary>> {
        let mut cache = self.runs.lock().expect("delivery run cache");
        cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
        cache.get(key).cloned()
    }

    fn put_runs(
        &self,
        key: String,
        items: Vec<CodeDeliveryRunSummary>,
        errors: Vec<CodeDeliverySourceError>,
    ) {
        self.runs.lock().expect("delivery run cache").insert(
            key,
            CachedAggregate {
                fetched_at: Instant::now(),
                items,
                errors,
            },
        );
    }

    fn run_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.run_reads
            .lock()
            .expect("delivery run read locks")
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn repository(&self, key: &str) -> Option<CachedValue<CodeGitHubRepositoryRef>> {
        cached_value(&self.repositories, key, "delivery repository cache")
    }

    fn put_repository(&self, key: String, value: CodeGitHubRepositoryRef) {
        put_cached_value(&self.repositories, key, value, "delivery repository cache");
    }

    fn repository_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.repository_reads,
            key,
            "delivery repository read locks",
        )
    }

    fn owner_repositories(&self, key: &str) -> Option<CachedValue<OwnerRepositoryCatalog>> {
        self.owner_repositories
            .lock()
            .expect("delivery owner repository cache")
            .get(key)
            .cloned()
    }

    fn owner_cache_generation(&self, key: &str) -> u64 {
        self.owner_cache_generations
            .lock()
            .expect("delivery owner cache generations")
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    fn put_owner_repositories_if_current(
        &self,
        key: &str,
        generation: u64,
        value: OwnerRepositoryCatalog,
    ) -> bool {
        let generations = self
            .owner_cache_generations
            .lock()
            .expect("delivery owner cache generations");
        if generations.get(key).copied().unwrap_or_default() != generation {
            return false;
        }
        put_cached_value(
            &self.owner_repositories,
            key.to_owned(),
            value,
            "delivery owner repository cache",
        );
        true
    }

    fn owner_repository_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.owner_repository_reads,
            key,
            "delivery owner repository read locks",
        )
    }

    fn workspace_index(&self, key: &str) -> Option<CachedValue<Vec<WorkspaceIndexEntry>>> {
        cached_value(
            &self.workspace_indexes,
            key,
            "delivery workspace index cache",
        )
    }

    fn put_workspace_index_if_current(
        &self,
        key: &str,
        generation: u64,
        value: Vec<WorkspaceIndexEntry>,
    ) -> bool {
        let generations = self
            .owner_cache_generations
            .lock()
            .expect("delivery owner cache generations");
        if generations.get(key).copied().unwrap_or_default() != generation {
            return false;
        }
        put_cached_value(
            &self.workspace_indexes,
            key.to_owned(),
            value,
            "delivery workspace index cache",
        );
        true
    }

    fn workspace_index_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.workspace_index_reads,
            key,
            "delivery workspace index read locks",
        )
    }

    pub(crate) fn invalidate(&self) {
        self.pull_requests
            .lock()
            .expect("delivery PR cache")
            .clear();
        self.runs.lock().expect("delivery run cache").clear();
    }

    pub(crate) fn invalidate_owner(&self, owner: &OwnerId) {
        let owner_key = owner.to_string();
        let aggregate_prefix = format!("{owner_key}:");
        let mut generations = self
            .owner_cache_generations
            .lock()
            .expect("delivery owner cache generations");
        let generation = generations.entry(owner_key.clone()).or_default();
        *generation = generation
            .checked_add(1)
            .expect("delivery owner cache generation overflow");
        self.owner_repositories
            .lock()
            .expect("delivery owner repository cache")
            .remove(&owner_key);
        self.workspace_indexes
            .lock()
            .expect("delivery workspace index cache")
            .remove(&owner_key);
        drop(generations);
        self.pull_requests
            .lock()
            .expect("delivery PR cache")
            .retain(|key, _| !key.starts_with(&aggregate_prefix));
        self.runs
            .lock()
            .expect("delivery run cache")
            .retain(|key, _| !key.starts_with(&aggregate_prefix));
    }
}

fn cached_value<T: Clone>(
    cache: &Mutex<HashMap<String, CachedValue<T>>>,
    key: &str,
    label: &str,
) -> Option<CachedValue<T>> {
    let mut cache = cache.lock().expect(label);
    cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
    cache.get(key).cloned()
}

fn put_cached_value<T>(
    cache: &Mutex<HashMap<String, CachedValue<T>>>,
    key: String,
    value: T,
    label: &str,
) {
    cache.lock().expect(label).insert(
        key,
        CachedValue {
            fetched_at: Instant::now(),
            value,
        },
    );
}

fn cache_read_lock(
    cache: &Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    key: &str,
    label: &str,
) -> Arc<tokio::sync::Mutex<()>> {
    cache
        .lock()
        .expect(label)
        .entry(key.to_owned())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

#[derive(Debug, Clone)]
struct WorkspaceIndexEntry {
    workspace: CodeWorkspace,
    repository_key: String,
    head_sha: Option<String>,
}

/// Read the exact pull requests that local workspaces already identify.
///
/// The trigger sweep uses this path instead of a bounded remote list. One
/// owner-wide workspace index serves every read, and the shared concurrency
/// limit bounds repository resolution and pull-request fetches separately.
pub(crate) async fn query_pull_requests_by_number(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    repositories: Vec<(CodeGitHubRepositoryTarget, Vec<u64>)>,
) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
    let observation = gh::observe_gh(runtime.gh_search_path_owned().as_deref()).await;
    let capability = github_capability(&observation);
    let repositories = dedupe_numbered_targets(repositories)?;
    if observation.authenticated != Some(true) {
        return Ok(CodeDeliveryPullRequestsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: Vec::new(),
            fetched_at: Utc::now(),
        });
    }

    let binary = observation
        .binary
        .clone()
        .expect("authenticated gh has a binary");
    let workspaces = Arc::new(workspace_index(runtime, owner, false).await?);
    let resolved = stream::iter(repositories)
        .map(|(target, numbers)| {
            let binary = binary.clone();
            async move {
                let repository = resolve_repository(&binary, &target, None)
                    .await
                    .map_err(|message| (target.clone(), message))?;
                Ok((target, repository, numbers))
            }
        })
        .buffer_unordered(DELIVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut reads = Vec::new();
    let mut errors = Vec::new();
    for result in resolved {
        match result {
            Ok((target, repository, numbers)) => {
                reads.extend(
                    numbers
                        .into_iter()
                        .map(|number| (target.clone(), repository.clone(), number)),
                );
            }
            Err((target, message)) => errors.push(source_error(Some(target), message)),
        }
    }

    let results = stream::iter(reads)
        .map(|(target, repository, number)| {
            let binary = binary.clone();
            let workspaces = Arc::clone(&workspaces);
            async move {
                with_transient_retry(|| {
                    fetch_pull_request(&binary, &repository, number, &workspaces)
                })
                .await
                .map_err(|message| (target, format!("pull request #{number}: {message}")))
            }
        })
        .buffer_unordered(DELIVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut items = Vec::new();
    for result in results {
        match result {
            Ok(item) => items.push(item),
            Err((target, message)) => errors.push(source_error(Some(target), message)),
        }
    }
    items.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(CodeDeliveryPullRequestsPage {
        capability,
        items,
        next_cursor: None,
        errors,
        fetched_at: Utc::now(),
    })
}

pub(crate) async fn discover_repositories(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    refresh: bool,
) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
    let search_path = runtime.gh_search_path_owned();
    let observation = if refresh {
        gh::refresh_gh_observation(search_path.as_deref()).await
    } else {
        gh::observe_gh(search_path.as_deref()).await
    };
    let capability = github_capability(&observation);
    let catalog = owner_repository_catalog(runtime, owner, refresh).await?;
    let mut errors = catalog.errors;

    let resolved = if observation.authenticated == Some(true) {
        let binary = observation
            .binary
            .clone()
            .expect("authenticated gh has a binary");
        stream::iter(catalog.entries)
            .map(|entry| {
                let binary = binary.clone();
                let runtime = runtime;
                async move {
                    resolve_repository_cached(
                        runtime,
                        &binary,
                        &entry.target,
                        Some(entry.repo.id),
                        refresh,
                    )
                    .await
                    .map_err(|message| (entry.target, message))
                }
            })
            .buffer_unordered(DELIVERY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await
    } else {
        catalog
            .entries
            .into_iter()
            .map(|entry| {
                Ok(repository_ref_from_target(
                    &entry.target,
                    Some(entry.repo.id),
                ))
            })
            .collect()
    };

    let mut repositories = Vec::new();
    for result in resolved {
        match result {
            Ok(repository) => repositories.push(repository),
            Err((target, message)) => errors.push(source_error(Some(target), message)),
        }
    }
    repositories.sort_by(|left, right| left.name_with_owner.cmp(&right.name_with_owner));
    Ok(CodeDeliveryRepositoriesSnapshot {
        capability,
        repositories,
        errors,
        fetched_at: Utc::now(),
    })
}

pub(crate) async fn resolve_repositories(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: ResolveCodeDeliveryRepositoriesBody,
) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
    if body.repositories.len() > MAX_REPOSITORIES {
        return Err(ServerError::bad_request(format!(
            "at most {MAX_REPOSITORIES} repositories may be resolved at once"
        )));
    }

    let mut targets = Vec::new();
    let mut errors = Vec::new();
    for input in body.repositories {
        match parse_repository_input(&input) {
            Ok(target) => targets.push(target),
            Err(message) => errors.push(CodeDeliverySourceError {
                repository: None,
                kind: "invalid_repository".into(),
                message,
                retry_at: None,
            }),
        }
    }
    targets = dedupe_targets(targets)?;
    ensure_delivery_targets(runtime, owner, allow_unscoped_delivery, &targets).await?;

    let observation = gh::observe_gh(runtime.gh_search_path_owned().as_deref()).await;
    let capability = github_capability(&observation);

    let mut repositories = Vec::new();
    if observation.authenticated == Some(true) {
        let binary = observation
            .binary
            .clone()
            .expect("authenticated gh has a binary");
        let results = stream::iter(targets)
            .map(|target| {
                let binary = binary.clone();
                let runtime = runtime;
                async move {
                    resolve_repository_cached(runtime, &binary, &target, None, false)
                        .await
                        .map_err(|message| (target, message))
                }
            })
            .buffer_unordered(DELIVERY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            match result {
                Ok(repository) => repositories.push(repository),
                Err((target, message)) => errors.push(source_error(Some(target), message)),
            }
        }
    } else {
        let kind = observation_error_kind(&observation);
        errors.extend(targets.into_iter().map(|target| CodeDeliverySourceError {
            repository: Some(target),
            kind: kind.into(),
            message: capability.remediation.clone(),
            retry_at: None,
        }));
    }

    repositories.sort_by(|left, right| left.name_with_owner.cmp(&right.name_with_owner));
    Ok(CodeDeliveryRepositoriesSnapshot {
        capability,
        repositories,
        errors,
        fetched_at: Utc::now(),
    })
}

pub(crate) async fn query_pull_requests(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    query: CodeDeliveryPullRequestQuery,
) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
    let force_refresh = query.refresh && query.cursor.is_none();
    let targets = dedupe_targets(query.repositories.clone())?;
    ensure_delivery_targets(runtime, owner, allow_unscoped_delivery, &targets).await?;
    let search_path = runtime.gh_search_path_owned();
    let observation = if force_refresh {
        gh::refresh_gh_observation(search_path.as_deref()).await
    } else {
        gh::observe_gh(search_path.as_deref()).await
    };
    let capability = github_capability(&observation);
    if observation.authenticated != Some(true) {
        return Ok(CodeDeliveryPullRequestsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: vec![observation_source_error(&observation)],
            fetched_at: Utc::now(),
        });
    }

    let remote_plan = pull_request_remote_plan(&query);
    let cache_key = aggregate_cache_key(
        owner,
        &format!("prs:{}", remote_plan.cache_scope()),
        &targets,
    );
    let request_started = Instant::now();
    // A user refresh must reach GitHub. Paging must not: following a cursor
    // against a freshly reread aggregate would renumber the offsets underneath
    // the reader and skip or repeat rows.
    let cached = if force_refresh {
        None
    } else {
        runtime.delivery_cache.pull_requests(&cache_key)
    };
    let aggregate = match cached {
        Some(cached) => cached,
        None => {
            let read = runtime.delivery_cache.pull_request_read(&cache_key);
            let _guard = read.lock().await;
            if let Some(cached) = runtime.delivery_cache.pull_requests(&cache_key) {
                if !force_refresh || cached.fetched_at >= request_started {
                    return pull_request_page(capability, cached, &query);
                }
            }
            let binary = observation
                .binary
                .clone()
                .expect("authenticated gh has a binary");
            let workspace_index = workspace_index(runtime, owner, force_refresh).await?;
            let results = stream::iter(targets.clone())
                .map(|target| {
                    let binary = binary.clone();
                    let workspace_index = workspace_index.clone();
                    let runtime = runtime;
                    async move {
                        fetch_pull_requests(
                            runtime,
                            &binary,
                            &target,
                            &workspace_index,
                            remote_plan,
                            force_refresh,
                        )
                        .await
                        .map_err(|message| (target, message))
                    }
                })
                .buffer_unordered(DELIVERY_CONCURRENCY)
                .collect::<Vec<_>>()
                .await;
            let mut items = Vec::new();
            let mut errors = Vec::new();
            for result in results {
                match result {
                    Ok(mut repository_items) => items.append(&mut repository_items),
                    Err((target, message)) => errors.push(source_error(Some(target), message)),
                }
            }
            items.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            runtime.delivery_cache.put_pull_requests(
                cache_key.clone(),
                items.clone(),
                errors.clone(),
            );
            CachedAggregate {
                fetched_at: Instant::now(),
                items,
                errors,
            }
        }
    };
    pull_request_page(capability, aggregate, &query)
}

fn pull_request_page(
    capability: CodeGitHubCapability,
    aggregate: CachedAggregate<CodeDeliveryPullRequestSummary>,
    query: &CodeDeliveryPullRequestQuery,
) -> Result<CodeDeliveryPullRequestsPage, ServerError> {
    let filtered = aggregate
        .items
        .into_iter()
        .filter(|item| pull_request_matches(item, query))
        .collect::<Vec<_>>();
    let (items, next_cursor) = paginate(filtered, query.cursor.as_deref(), query.limit)?;
    Ok(CodeDeliveryPullRequestsPage {
        capability,
        items,
        next_cursor,
        errors: aggregate.errors,
        fetched_at: Utc::now(),
    })
}

pub(crate) async fn pull_request_detail(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    target: CodeDeliveryPullRequestTarget,
) -> Result<CodeDeliveryPullRequestDetail, ServerError> {
    ensure_delivery_targets(
        runtime,
        owner,
        allow_unscoped_delivery,
        std::slice::from_ref(&target.repository),
    )
    .await?;
    let observation = require_authenticated(runtime).await?;
    let binary = observation
        .binary
        .as_ref()
        .expect("authenticated gh has binary");
    let repository = resolve_repository_cached(runtime, binary, &target.repository, None, false)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let workspace_index = workspace_index(runtime, owner, false).await?;
    let summary = fetch_pull_request(binary, &repository, target.number, &workspace_index)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;

    let pull_endpoint = api_endpoint(&target.repository, &format!("pulls/{}", target.number));
    let issue_comments_endpoint = api_endpoint(
        &target.repository,
        &format!("issues/{}/comments?per_page=100", target.number),
    );
    let reviews_endpoint = api_endpoint(
        &target.repository,
        &format!("pulls/{}/reviews?per_page=100", target.number),
    );
    let inline_endpoint = api_endpoint(
        &target.repository,
        &format!("pulls/{}/comments?per_page=100", target.number),
    );
    let files_endpoint = api_endpoint(
        &target.repository,
        &format!("pulls/{}/files?per_page=100", target.number),
    );
    let (pull, issue_comments, reviews, inline_comments, changed) = tokio::join!(
        run_api_json(binary, &target.repository.host, &pull_endpoint),
        run_api_json(binary, &target.repository.host, &issue_comments_endpoint),
        run_api_json(binary, &target.repository.host, &reviews_endpoint),
        run_api_json(binary, &target.repository.host, &inline_endpoint),
        run_api_json(binary, &target.repository.host, &files_endpoint),
    );
    let pull = pull.map_err(|message| ServerError::bad_request_kind("github", message))?;
    let mut comments = Vec::new();
    let mut errors = Vec::new();
    match issue_comments {
        Ok(value) => {
            record_full_detail_page(
                &mut errors,
                &target.repository,
                "issue comments",
                value.as_array().map(Vec::len),
            );
            comments.extend(parse_issue_comments(&value));
        }
        Err(message) => errors.push(detail_source_error(
            &target.repository,
            "issue comments",
            message,
        )),
    }
    match reviews {
        Ok(value) => {
            record_full_detail_page(
                &mut errors,
                &target.repository,
                "reviews",
                value.as_array().map(Vec::len),
            );
            comments.extend(parse_reviews(&value));
        }
        Err(message) => errors.push(detail_source_error(&target.repository, "reviews", message)),
    }
    match inline_comments {
        Ok(value) => {
            record_full_detail_page(
                &mut errors,
                &target.repository,
                "inline comments",
                value.as_array().map(Vec::len),
            );
            comments.extend(parse_inline_comments(&value));
        }
        Err(message) => errors.push(detail_source_error(
            &target.repository,
            "inline comments",
            message,
        )),
    }
    comments.sort_by(|left, right| left.created_at.cmp(&right.created_at));

    let changed_files = u64_field(&pull, "changed_files").unwrap_or(0);
    let mut files = match changed {
        Ok(value) => parse_pull_request_files(&value),
        Err(message) => {
            errors.push(detail_source_error(
                &target.repository,
                "changed files",
                message,
            ));
            Vec::new()
        }
    };
    let files_truncated = pull_request_files_truncated(files.len(), changed_files);
    files.truncate(MAX_DETAIL_FILES);

    let open = summary.state == "open";
    Ok(CodeDeliveryPullRequestDetail {
        can_mark_ready: open && summary.draft,
        can_merge: open && !summary.draft,
        can_rerun_failed: summary.checks.iter().any(|check| {
            check.bucket == PullRequestCheckBucket::Fail && check.workflow_run_id.is_some()
        }),
        can_close: open,
        // A merged pull request cannot be reopened; a closed unmerged one can.
        can_reopen: summary.state == "closed",
        can_comment: true,
        body: text_field(&pull, "body").unwrap_or_default(),
        labels: string_array_path(&pull, &["labels"], "name"),
        assignees: string_array_path(&pull, &["assignees"], "login"),
        requested_reviewers: string_array_path(&pull, &["requested_reviewers"], "login"),
        changed_files,
        additions: u64_field(&pull, "additions").unwrap_or(0),
        deletions: u64_field(&pull, "deletions").unwrap_or(0),
        commits: u64_field(&pull, "commits").unwrap_or(0),
        merged_by: pull
            .get("merged_by")
            .and_then(|author| text_field(author, "login")),
        files,
        files_truncated,
        comments,
        errors,
        summary,
    })
}

/// Map `GET /pulls/{n}/files` onto the panel's file rows.
///
/// `patch` is absent for binary files and for diffs GitHub declines to render;
/// the panel says so rather than showing an empty diff.
fn parse_pull_request_files(value: &Value) -> Vec<CodeDeliveryPullRequestFile> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some(CodeDeliveryPullRequestFile {
                path: text_field(item, "filename")?,
                status: text_field(item, "status").unwrap_or_else(|| "changed".into()),
                additions: u64_field(item, "additions").unwrap_or(0),
                deletions: u64_field(item, "deletions").unwrap_or(0),
                previous_path: text_field(item, "previous_filename"),
                patch: text_field(item, "patch"),
            })
        })
        .collect()
}

fn pull_request_files_truncated(returned: usize, changed_files: u64) -> bool {
    returned > MAX_DETAIL_FILES || (returned as u64) < changed_files
}

fn delivery_action_result(message: String) -> CodeDeliveryActionResult {
    CodeDeliveryActionResult {
        success: true,
        message,
        rerun_outcomes: Vec::new(),
    }
}

fn rerun_action_result(mut outcomes: Vec<CodeDeliveryRerunOutcome>) -> CodeDeliveryActionResult {
    outcomes.sort_by_key(|outcome| outcome.workflow_run_id);
    let succeeded = outcomes.iter().filter(|outcome| outcome.success).count();
    let failed = outcomes.len().saturating_sub(succeeded);
    let message = match (succeeded, failed) {
        (1, 0) => "Failed jobs queued for one workflow run".into(),
        (succeeded, 0) => format!("Failed jobs queued for {succeeded} workflow runs"),
        (0, 1) => "Could not queue failed jobs for one workflow run".into(),
        (0, failed) => format!("Could not queue failed jobs for {failed} workflow runs"),
        (succeeded, failed) => format!(
            "Failed jobs queued for {}; {} failed",
            workflow_run_count(succeeded),
            workflow_run_count(failed)
        ),
    };
    CodeDeliveryActionResult {
        success: failed == 0,
        message,
        rerun_outcomes: outcomes,
    }
}

fn workflow_run_count(count: usize) -> String {
    if count == 1 {
        "one workflow run".into()
    } else {
        format!("{count} workflow runs")
    }
}

pub(crate) async fn act_on_pull_request(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: CodeDeliveryPullRequestActionBody,
) -> Result<CodeDeliveryActionResult, ServerError> {
    ensure_delivery_targets(
        runtime,
        owner,
        allow_unscoped_delivery,
        std::slice::from_ref(&body.target.repository),
    )
    .await?;
    let search_path = runtime.gh_search_path_owned();
    let target = body.target;
    match body.action {
        CodeDeliveryPullRequestAction::MarkReady => {
            gh::mark_pull_request_ready(
                &target.repository.host,
                &target.repository.owner,
                &target.repository.name,
                target.number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(format!(
                "Pull request #{} is ready for review",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Merge {
            method,
            auto,
            expected_head_sha,
        } => {
            gh::merge_pull_request_target(
                &target.repository.host,
                &target.repository.owner,
                &target.repository.name,
                target.number,
                merge_method(method),
                auto,
                &expected_head_sha,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(if auto {
                format!("Auto-merge enabled for pull request #{}", target.number)
            } else {
                format!("Pull request #{} merged", target.number)
            }))
        }
        CodeDeliveryPullRequestAction::RerunFailed { workflow_run_ids } => {
            let unique = workflow_run_ids.into_iter().collect::<HashSet<_>>();
            if unique.is_empty() {
                return Err(ServerError::bad_request(
                    "at least one workflow run id is required",
                ));
            }
            let observation = require_authenticated(runtime).await?;
            let results = stream::iter(unique)
                .map(|run_id| {
                    let observation = observation.clone();
                    let repository = target.repository.clone();
                    async move {
                        (
                            run_id,
                            gh::rerun_failed_jobs_with_observation(
                                &observation,
                                &repository.host,
                                &repository.owner,
                                &repository.name,
                                run_id,
                            )
                            .await,
                        )
                    }
                })
                .buffer_unordered(DELIVERY_CONCURRENCY)
                .collect::<Vec<_>>()
                .await;
            let any_success = results.iter().any(|(_, result)| result.is_ok());
            if any_success {
                runtime.delivery_cache.invalidate();
            }
            let outcomes = results
                .into_iter()
                .map(|(workflow_run_id, result)| match result {
                    Ok(()) => CodeDeliveryRerunOutcome {
                        workflow_run_id,
                        success: true,
                        error: None,
                    },
                    Err(error) => {
                        tracing::warn!(
                            workflow_run_id,
                            partial_success = any_success,
                            "GitHub workflow rerun failed"
                        );
                        CodeDeliveryRerunOutcome {
                            workflow_run_id,
                            success: false,
                            error: Some(error.to_string()),
                        }
                    }
                })
                .collect();
            Ok(rerun_action_result(outcomes))
        }
        CodeDeliveryPullRequestAction::Close => {
            gh::close_pull_request_target(
                &target.repository.host,
                &target.repository.owner,
                &target.repository.name,
                target.number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(format!(
                "Pull request #{} closed",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Reopen => {
            gh::reopen_pull_request_target(
                &target.repository.host,
                &target.repository.owner,
                &target.repository.name,
                target.number,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(format!(
                "Pull request #{} reopened",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Comment { body } => {
            let body = body.trim();
            if body.is_empty() {
                return Err(ServerError::bad_request("a comment needs a body"));
            }
            if body.len() > MAX_COMMENT_BYTES {
                return Err(ServerError::bad_request(format!(
                    "a comment may be at most {MAX_COMMENT_BYTES} bytes"
                )));
            }
            gh::comment_on_pull_request_target(
                &target.repository.host,
                &target.repository.owner,
                &target.repository.name,
                target.number,
                body,
                search_path.as_deref(),
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(format!(
                "Comment posted on pull request #{}",
                target.number
            )))
        }
    }
}

pub(crate) async fn query_runs(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    query: CodeDeliveryRunQuery,
) -> Result<CodeDeliveryRunsPage, ServerError> {
    let force_refresh = query.refresh && query.cursor.is_none();
    let targets = dedupe_targets(query.repositories.clone())?;
    ensure_delivery_targets(runtime, owner, allow_unscoped_delivery, &targets).await?;
    let search_path = runtime.gh_search_path_owned();
    let observation = if force_refresh {
        gh::refresh_gh_observation(search_path.as_deref()).await
    } else {
        gh::observe_gh(search_path.as_deref()).await
    };
    let capability = github_capability(&observation);
    if observation.authenticated != Some(true) {
        return Ok(CodeDeliveryRunsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: vec![observation_source_error(&observation)],
            fetched_at: Utc::now(),
        });
    }

    let (remote_scope, fetch_workflows, fetch_deployments) = run_remote_scope(&query);
    let cache_key = aggregate_cache_key(owner, &format!("runs:{remote_scope}"), &targets);
    let request_started = Instant::now();
    let cached = if force_refresh {
        None
    } else {
        runtime.delivery_cache.runs(&cache_key)
    };
    let aggregate = match cached {
        Some(cached) => cached,
        None => {
            let read = runtime.delivery_cache.run_read(&cache_key);
            let _guard = read.lock().await;
            if let Some(cached) = runtime.delivery_cache.runs(&cache_key) {
                if !force_refresh || cached.fetched_at >= request_started {
                    return run_page(capability, cached, &query);
                }
            }
            let binary = observation
                .binary
                .clone()
                .expect("authenticated gh has a binary");
            let workspace_index = workspace_index(runtime, owner, force_refresh).await?;
            let results = stream::iter(targets.clone())
                .map(|target| {
                    let binary = binary.clone();
                    let workspace_index = workspace_index.clone();
                    let runtime = runtime;
                    async move {
                        fetch_runs(
                            runtime,
                            &binary,
                            &target,
                            &workspace_index,
                            fetch_workflows,
                            fetch_deployments,
                            force_refresh,
                        )
                        .await
                        .map_err(|message| (target, message))
                    }
                })
                .buffer_unordered(DELIVERY_CONCURRENCY)
                .collect::<Vec<_>>()
                .await;
            let mut items = Vec::new();
            let mut errors = Vec::new();
            for result in results {
                match result {
                    Ok(mut fetched) => {
                        items.append(&mut fetched.items);
                        errors.append(&mut fetched.errors);
                    }
                    Err((target, message)) => errors.push(source_error(Some(target), message)),
                }
            }
            items.sort_by(|left, right| {
                right
                    .updated_at
                    .cmp(&left.updated_at)
                    .then_with(|| left.id.cmp(&right.id))
            });
            runtime
                .delivery_cache
                .put_runs(cache_key.clone(), items.clone(), errors.clone());
            CachedAggregate {
                fetched_at: Instant::now(),
                items,
                errors,
            }
        }
    };
    run_page(capability, aggregate, &query)
}

fn run_page(
    capability: CodeGitHubCapability,
    aggregate: CachedAggregate<CodeDeliveryRunSummary>,
    query: &CodeDeliveryRunQuery,
) -> Result<CodeDeliveryRunsPage, ServerError> {
    let filtered = aggregate
        .items
        .into_iter()
        .filter(|item| run_matches(item, query))
        .collect::<Vec<_>>();
    let (items, next_cursor) = paginate(filtered, query.cursor.as_deref(), query.limit)?;
    Ok(CodeDeliveryRunsPage {
        capability,
        items,
        next_cursor,
        errors: aggregate.errors,
        fetched_at: Utc::now(),
    })
}

pub(crate) async fn run_detail(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    target: CodeDeliveryRunTarget,
) -> Result<CodeDeliveryRunDetail, ServerError> {
    ensure_delivery_targets(
        runtime,
        owner,
        allow_unscoped_delivery,
        std::slice::from_ref(&target.repository),
    )
    .await?;
    let observation = require_authenticated(runtime).await?;
    let binary = observation
        .binary
        .as_ref()
        .expect("authenticated gh has binary");
    let repository = resolve_repository_cached(runtime, binary, &target.repository, None, false)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let workspace_index = workspace_index(runtime, owner, false).await?;

    match target.kind {
        CodeDeliveryRunKind::WorkflowRun => {
            let run_endpoint =
                api_endpoint(&target.repository, &format!("actions/runs/{}", target.id));
            let jobs_endpoint = api_endpoint(
                &target.repository,
                &format!("actions/runs/{}/jobs?per_page=100", target.id),
            );
            let (run, jobs) = tokio::join!(
                run_api_json(binary, &target.repository.host, &run_endpoint),
                run_api_json(binary, &target.repository.host, &jobs_endpoint),
            );
            let run = run.map_err(|message| ServerError::bad_request_kind("github", message))?;
            let summary = parse_workflow_run(&repository, &run, &workspace_index)
                .ok_or_else(|| ServerError::not_found("workflow run not found"))?;
            let mut errors = Vec::new();
            let jobs = match jobs {
                Ok(value) => value
                    .get("jobs")
                    .and_then(Value::as_array)
                    .map(|jobs| {
                        record_full_detail_page(
                            &mut errors,
                            &target.repository,
                            "jobs",
                            Some(jobs.len()),
                        );
                        jobs.iter()
                            .filter_map(parse_workflow_job)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                Err(message) => {
                    errors.push(detail_source_error(&target.repository, "jobs", message));
                    Vec::new()
                }
            };
            Ok(CodeDeliveryRunDetail {
                can_rerun_failed: jobs.iter().any(|job| {
                    matches!(
                        job.conclusion.as_deref(),
                        Some("failure" | "timed_out" | "action_required" | "startup_failure")
                    )
                }),
                summary,
                jobs,
                deployment_statuses: Vec::new(),
                errors,
            })
        }
        CodeDeliveryRunKind::Deployment => {
            let deployment_endpoint =
                api_endpoint(&target.repository, &format!("deployments/{}", target.id));
            let statuses_endpoint = api_endpoint(
                &target.repository,
                &format!("deployments/{}/statuses?per_page=100", target.id),
            );
            let (deployment, statuses) = tokio::join!(
                run_api_json(binary, &target.repository.host, &deployment_endpoint),
                run_api_json(binary, &target.repository.host, &statuses_endpoint),
            );
            let deployment =
                deployment.map_err(|message| ServerError::bad_request_kind("github", message))?;
            let mut errors = Vec::new();
            let statuses = match statuses {
                Ok(value) => value
                    .as_array()
                    .map(|statuses| {
                        record_full_detail_page(
                            &mut errors,
                            &target.repository,
                            "deployment statuses",
                            Some(statuses.len()),
                        );
                        statuses
                            .iter()
                            .filter_map(parse_deployment_status)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                Err(message) => {
                    errors.push(detail_source_error(
                        &target.repository,
                        "deployment statuses",
                        message,
                    ));
                    Vec::new()
                }
            };
            let summary =
                parse_deployment(&repository, &deployment, statuses.first(), &workspace_index)
                    .ok_or_else(|| ServerError::not_found("deployment not found"))?;
            Ok(CodeDeliveryRunDetail {
                summary,
                jobs: Vec::new(),
                deployment_statuses: statuses,
                can_rerun_failed: false,
                errors,
            })
        }
    }
}

pub(crate) async fn act_on_run(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    body: CodeDeliveryRunActionBody,
) -> Result<CodeDeliveryActionResult, ServerError> {
    ensure_delivery_targets(
        runtime,
        owner,
        allow_unscoped_delivery,
        std::slice::from_ref(&body.target.repository),
    )
    .await?;
    match body.action {
        CodeDeliveryRunAction::RerunFailed => {
            if body.target.kind != CodeDeliveryRunKind::WorkflowRun {
                return Err(ServerError::bad_request(
                    "only GitHub Actions workflow runs can be rerun",
                ));
            }
            let observation = require_authenticated(runtime).await?;
            gh::rerun_failed_jobs_with_observation(
                &observation,
                &body.target.repository.host,
                &body.target.repository.owner,
                &body.target.repository.name,
                body.target.id,
            )
            .await
            .map_err(map_gh_error)?;
            runtime.delivery_cache.invalidate();
            Ok(rerun_action_result(vec![CodeDeliveryRerunOutcome {
                workflow_run_id: body.target.id,
                success: true,
                error: None,
            }]))
        }
    }
}

fn github_capability(observation: &GhObservation) -> CodeGitHubCapability {
    CodeGitHubCapability {
        found: observation.found,
        authenticated: observation.authenticated,
        viewer_login: observation.viewer_login.clone(),
        remediation: observation.remediation.clone(),
    }
}

async fn require_authenticated(runtime: &CodeRuntime) -> Result<GhObservation, ServerError> {
    let observation = gh::observe_gh(runtime.gh_search_path_owned().as_deref()).await;
    if !observation.found {
        return Err(ServerError::conflict_kind(
            "gh_absent",
            observation.remediation,
        ));
    }
    match observation.authenticated {
        Some(true) => {}
        Some(false) => {
            return Err(ServerError::conflict_kind(
                "gh_signed_out",
                observation.remediation,
            ));
        }
        None => {
            return Err(ServerError::conflict_kind(
                "gh_unavailable",
                observation.remediation,
            ));
        }
    }
    Ok(observation)
}

pub(crate) async fn repository_target_from_local(
    repo: &CodeRepo,
) -> Result<CodeGitHubRepositoryTarget, String> {
    let remote = git_read(Path::new(&repo.root_path), &["remote", "get-url", "origin"])
        .await
        .map_err(|message| format!("could not read origin remote: {message}"))?;
    parse_repository_input(&remote)
}

async fn owner_repository_catalog(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    force_refresh: bool,
) -> Result<OwnerRepositoryCatalog, ServerError> {
    let key = owner.to_string();
    let request_started = Instant::now();
    if !force_refresh {
        if let Some(cached) = runtime.delivery_cache.owner_repositories(&key) {
            return Ok(cached.value);
        }
    }

    let read = runtime.delivery_cache.owner_repository_read(&key);
    let _guard = read.lock().await;
    if let Some(cached) = runtime.delivery_cache.owner_repositories(&key) {
        if !force_refresh || cached.fetched_at >= request_started {
            return Ok(cached.value);
        }
    }

    loop {
        let generation = runtime.delivery_cache.owner_cache_generation(&key);
        let results = stream::iter(runtime.list_repos(owner).await?)
            .map(|repo| async move {
                match repository_target_from_local(&repo).await {
                    Ok(target) => Ok(OwnerRepositoryEntry { repo, target }),
                    Err(message) => Err(CodeDeliverySourceError {
                        repository: None,
                        kind: "not_github".into(),
                        message: format!("{}: {message}", repo.display_name),
                        retry_at: None,
                    }),
                }
            })
            .buffer_unordered(DELIVERY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let mut catalog = OwnerRepositoryCatalog::default();
        for result in results {
            match result {
                Ok(entry) => catalog.entries.push(entry),
                Err(error) => catalog.errors.push(error),
            }
        }
        if runtime.delivery_cache.put_owner_repositories_if_current(
            &key,
            generation,
            catalog.clone(),
        ) {
            return Ok(catalog);
        }
    }
}

async fn ensure_delivery_targets(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    targets: &[CodeGitHubRepositoryTarget],
) -> Result<(), ServerError> {
    if allow_unscoped_delivery || targets.is_empty() {
        return Ok(());
    }
    // The target mapping may use its short cache, but membership may not.
    // A database read is enough to remove stale catalog entries without
    // spawning one git process for every registered repository.
    let catalog = owner_repository_catalog(runtime, owner, false).await?;
    let live_repo_ids = runtime
        .list_repos(owner)
        .await?
        .into_iter()
        .map(|repo| repo.id)
        .collect::<HashSet<_>>();
    let allowed = live_catalog_target_keys(&catalog, &live_repo_ids);
    if let Some(target) = targets
        .iter()
        .find(|target| !allowed.contains(&repository_key(target)))
    {
        return Err(ServerError::not_found(format!(
            "GitHub repository {}/{} is not registered for this account",
            target.owner, target.name
        )));
    }
    Ok(())
}

fn live_catalog_target_keys(
    catalog: &OwnerRepositoryCatalog,
    live_repo_ids: &HashSet<RepoId>,
) -> HashSet<String> {
    catalog
        .entries
        .iter()
        .filter(|entry| live_repo_ids.contains(&entry.repo.id))
        .map(|entry| repository_key(&entry.target))
        .collect()
}

fn parse_repository_input(input: &str) -> Result<CodeGitHubRepositoryTarget, String> {
    let value = input.trim().trim_end_matches('/').trim_end_matches(".git");
    if value.is_empty() {
        return Err("repository cannot be empty".into());
    }

    let (host, path) = if let Some(rest) = value.strip_prefix("git@") {
        rest.split_once(':')
            .map(|(host, path)| (host.to_owned(), path.to_owned()))
            .ok_or_else(|| "SSH repository must include owner/repo".to_owned())?
    } else if value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("ssh://")
    {
        let parsed = url::Url::parse(value).map_err(|_| "repository URL is invalid".to_owned())?;
        let host = parsed
            .host_str()
            .ok_or_else(|| "repository URL has no host".to_owned())?
            .to_owned();
        (host, parsed.path().trim_matches('/').to_owned())
    } else {
        let parts = value.split('/').collect::<Vec<_>>();
        match parts.as_slice() {
            [owner, name] => ("github.com".to_owned(), format!("{owner}/{name}")),
            [host, owner, name] if host.contains('.') => {
                ((*host).to_owned(), format!("{owner}/{name}"))
            }
            _ => return Err("use owner/repo, host/owner/repo, or a GitHub URL".into()),
        }
    };
    let parts = path.trim_matches('/').split('/').collect::<Vec<_>>();
    if parts.len() != 2 || !valid_repo_segment(parts[0]) || !valid_repo_segment(parts[1]) {
        return Err("repository must contain a valid owner and name".into());
    }
    Ok(CodeGitHubRepositoryTarget {
        host: host.to_ascii_lowercase(),
        owner: parts[0].to_owned(),
        name: parts[1].trim_end_matches(".git").to_owned(),
    })
}

fn valid_repo_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn dedupe_targets(
    targets: Vec<CodeGitHubRepositoryTarget>,
) -> Result<Vec<CodeGitHubRepositoryTarget>, ServerError> {
    if targets.len() > MAX_REPOSITORIES {
        return Err(ServerError::bad_request(format!(
            "at most {MAX_REPOSITORIES} repositories may be queried at once"
        )));
    }
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for mut target in targets {
        target.host = target.host.trim().to_ascii_lowercase();
        target.owner = target.owner.trim().to_owned();
        target.name = target.name.trim().trim_end_matches(".git").to_owned();
        if target.host.is_empty()
            || !valid_repo_segment(&target.owner)
            || !valid_repo_segment(&target.name)
        {
            return Err(ServerError::bad_request("invalid GitHub repository target"));
        }
        let key = repository_key(&target);
        if seen.insert(key) {
            deduped.push(target);
        }
    }
    Ok(deduped)
}

fn dedupe_numbered_targets(
    targets: Vec<(CodeGitHubRepositoryTarget, Vec<u64>)>,
) -> Result<Vec<(CodeGitHubRepositoryTarget, Vec<u64>)>, ServerError> {
    let mut grouped: HashMap<String, (CodeGitHubRepositoryTarget, HashSet<u64>)> = HashMap::new();
    for (mut target, numbers) in targets {
        target.host = target.host.trim().to_ascii_lowercase();
        target.owner = target.owner.trim().to_owned();
        target.name = target.name.trim().trim_end_matches(".git").to_owned();
        if target.host.is_empty()
            || !valid_repo_segment(&target.owner)
            || !valid_repo_segment(&target.name)
        {
            return Err(ServerError::bad_request("invalid GitHub repository target"));
        }
        grouped
            .entry(repository_key(&target))
            .and_modify(|(_, existing)| existing.extend(numbers.iter().copied()))
            .or_insert_with(|| (target, numbers.into_iter().collect()));
    }

    let mut grouped = grouped.into_values().collect::<Vec<_>>();
    for (_, numbers) in &mut grouped {
        numbers.remove(&0);
    }
    let mut grouped = grouped
        .into_iter()
        .filter_map(|(target, numbers)| {
            if numbers.is_empty() {
                return None;
            }
            let mut numbers = numbers.into_iter().collect::<Vec<_>>();
            numbers.sort_unstable();
            Some((target, numbers))
        })
        .collect::<Vec<_>>();
    grouped.sort_by(|(left, _), (right, _)| repository_key(left).cmp(&repository_key(right)));
    Ok(grouped)
}

async fn resolve_repository(
    binary: &Path,
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
) -> Result<CodeGitHubRepositoryRef, String> {
    let endpoint = format!("repos/{}/{}", target.owner, target.name);
    let value = run_api_json(binary, &target.host, &endpoint).await?;
    let owner = value
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .unwrap_or(&target.owner)
        .to_owned();
    let name = text_field(&value, "name").unwrap_or_else(|| target.name.clone());
    let name_with_owner =
        text_field(&value, "full_name").unwrap_or_else(|| format!("{owner}/{name}"));
    Ok(CodeGitHubRepositoryRef {
        host: target.host.clone(),
        owner,
        name,
        name_with_owner,
        url: text_field(&value, "html_url")
            .unwrap_or_else(|| format!("https://{}/{}/{}", target.host, target.owner, target.name)),
        default_branch: text_field(&value, "default_branch"),
        tidebreak_repo_id,
    })
}

async fn resolve_repository_cached(
    runtime: &CodeRuntime,
    binary: &Path,
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
    force_refresh: bool,
) -> Result<CodeGitHubRepositoryRef, String> {
    let key = repository_key(target);
    let request_started = Instant::now();
    if !force_refresh {
        if let Some(cached) = runtime.delivery_cache.repository(&key) {
            return Ok(repository_with_id(cached.value, tidebreak_repo_id));
        }
    }

    let read = runtime.delivery_cache.repository_read(&key);
    let _guard = read.lock().await;
    if let Some(cached) = runtime.delivery_cache.repository(&key) {
        if !force_refresh || cached.fetched_at >= request_started {
            return Ok(repository_with_id(cached.value, tidebreak_repo_id));
        }
    }

    let repository = resolve_repository(binary, target, None).await?;
    runtime
        .delivery_cache
        .put_repository(key, repository.clone());
    Ok(repository_with_id(repository, tidebreak_repo_id))
}

fn repository_with_id(
    mut repository: CodeGitHubRepositoryRef,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
) -> CodeGitHubRepositoryRef {
    repository.tidebreak_repo_id = tidebreak_repo_id;
    repository
}

fn repository_ref_from_target(
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
) -> CodeGitHubRepositoryRef {
    CodeGitHubRepositoryRef {
        host: target.host.clone(),
        owner: target.owner.clone(),
        name: target.name.clone(),
        name_with_owner: format!("{}/{}", target.owner, target.name),
        url: format!("https://{}/{}/{}", target.host, target.owner, target.name),
        default_branch: None,
        tidebreak_repo_id,
    }
}

async fn workspace_index(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    force_refresh: bool,
) -> Result<Vec<WorkspaceIndexEntry>, ServerError> {
    let key = owner.to_string();
    let request_started = Instant::now();
    if !force_refresh {
        if let Some(cached) = runtime.delivery_cache.workspace_index(&key) {
            return Ok(cached.value);
        }
    }

    let read = runtime.delivery_cache.workspace_index_read(&key);
    let _guard = read.lock().await;
    if let Some(cached) = runtime.delivery_cache.workspace_index(&key) {
        if !force_refresh || cached.fetched_at >= request_started {
            return Ok(cached.value);
        }
    }

    loop {
        let generation = runtime.delivery_cache.owner_cache_generation(&key);
        let catalog = owner_repository_catalog(runtime, owner, force_refresh).await?;
        let workspaces = runtime.list_workspaces(owner, None).await?;
        let mut repository_targets = HashMap::new();
        let mut roots = HashMap::new();
        for entry in catalog.entries {
            roots.insert(entry.repo.id, PathBuf::from(&entry.repo.root_path));
            repository_targets.insert(entry.repo.id, entry.target);
        }

        let index: Vec<WorkspaceIndexEntry> = stream::iter(workspaces)
            .map(|workspace| {
                let target = repository_targets.get(&workspace.repo_id).cloned();
                let root = roots.get(&workspace.repo_id).cloned();
                async move {
                    let target = target?;
                    let head_sha = match root {
                        Some(root) => git_read(&root, &["rev-parse", &workspace.branch_name])
                            .await
                            .ok()
                            .filter(|value| !value.is_empty()),
                        None => None,
                    };
                    Some(WorkspaceIndexEntry {
                        repository_key: repository_key(&target),
                        workspace,
                        head_sha,
                    })
                }
            })
            .buffer_unordered(DELIVERY_CONCURRENCY)
            .filter_map(async move |entry| entry)
            .collect()
            .await;
        if runtime
            .delivery_cache
            .put_workspace_index_if_current(&key, generation, index.clone())
        {
            return Ok(index);
        }
    }
}

/// True for GitHub failures that are worth one more attempt.
///
/// A cross-repository list is one `gh` invocation per repository, and the
/// list query is heavy enough that GitHub's gateway sheds it under load. One
/// shed request used to blank that repository's rows and surface a raw
/// `HTTP 504` banner over otherwise good results.
fn is_transient_github_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "502",
        "503",
        "504",
        "timeout",
        "timed out",
        "connection reset",
    ]
    .iter()
    .any(|token| message.contains(token))
}

/// Run one GitHub read, retrying a single time on a transient failure.
async fn with_transient_retry<T, F, Fut>(operation: F) -> Result<T, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    match operation().await {
        Ok(value) => Ok(value),
        Err(message) if is_transient_github_error(&message) => {
            tokio::time::sleep(TRANSIENT_RETRY_DELAY).await;
            operation().await
        }
        Err(message) => Err(message),
    }
}

async fn fetch_pull_requests(
    runtime: &CodeRuntime,
    binary: &Path,
    target: &CodeGitHubRepositoryTarget,
    workspaces: &[WorkspaceIndexEntry],
    plan: PullRequestRemotePlan,
    force_refresh: bool,
) -> Result<Vec<CodeDeliveryPullRequestSummary>, String> {
    let repository =
        resolve_repository_cached(runtime, binary, target, None, force_refresh).await?;
    let cli_repository = gh::cli_repository(&target.host, &target.owner, &target.name);
    let limit = MAX_REMOTE_ITEMS_PER_REPO.to_string();
    let args = [
        "pr",
        "list",
        "--repo",
        cli_repository.as_str(),
        "--state",
        plan.state,
        "--limit",
        limit.as_str(),
        "--json",
        plan.fields,
    ];
    let raw =
        with_transient_retry(|| gh::run_gh(Path::new("."), binary, &args, GH_READ_TIMEOUT)).await?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("could not parse pull requests: {error}"))?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|value| parse_pull_request(&repository, value, workspaces))
        .collect())
}

fn pull_request_remote_plan(query: &CodeDeliveryPullRequestQuery) -> PullRequestRemotePlan {
    let state = if query.attention_only
        || query.ready_only
        || (query.states.len() == 1 && query.states[0].eq_ignore_ascii_case("open"))
    {
        "open"
    } else if query.states.len() == 1 && query.states[0].eq_ignore_ascii_case("closed") {
        "closed"
    } else if query.states.len() == 1 && query.states[0].eq_ignore_ascii_case("merged") {
        "merged"
    } else {
        "all"
    };
    let checks_loaded = state == "open" || !query.check_states.is_empty();
    PullRequestRemotePlan {
        state,
        fields: if checks_loaded {
            PR_LIST_FIELDS_WITH_CHECKS
        } else {
            PR_LIST_FIELDS
        },
        checks_loaded,
    }
}

async fn fetch_pull_request(
    binary: &Path,
    repository: &CodeGitHubRepositoryRef,
    number: u64,
    workspaces: &[WorkspaceIndexEntry],
) -> Result<CodeDeliveryPullRequestSummary, String> {
    let cli_repository = gh::cli_repository(&repository.host, &repository.owner, &repository.name);
    let number = number.to_string();
    let raw = gh::run_gh(
        Path::new("."),
        binary,
        &[
            "pr",
            "view",
            &number,
            "--repo",
            &cli_repository,
            "--json",
            PR_LIST_FIELDS_WITH_CHECKS,
        ],
        GH_READ_TIMEOUT,
    )
    .await?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("could not parse pull request: {error}"))?;
    parse_pull_request(repository, &value, workspaces)
        .ok_or_else(|| "GitHub returned an incomplete pull request".into())
}

fn parse_pull_request(
    repository: &CodeGitHubRepositoryRef,
    value: &Value,
    workspaces: &[WorkspaceIndexEntry],
) -> Option<CodeDeliveryPullRequestSummary> {
    let number = u64_field(value, "number")?;
    let title = text_field(value, "title")?;
    let state = text_field(value, "state")?.to_ascii_lowercase();
    let url = text_field(value, "url")?;
    let draft = bool_field(value, "isDraft").unwrap_or(false);
    let author = value
        .get("author")
        .and_then(|author| author.get("login"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let author_avatar_url = value
        .get("author")
        .and_then(|author| author.get("avatarUrl"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let head_branch = text_field(value, "headRefName").unwrap_or_default();
    let base_branch = text_field(value, "baseRefName").unwrap_or_default();
    let head_sha = text_field(value, "headRefOid");
    let review_decision = normalized_optional(value, "reviewDecision");
    let mergeable = normalized_optional(value, "mergeable");
    let merge_state_status = normalized_optional(value, "mergeStateStatus");
    let merged_at = datetime_field(value, "mergedAt");
    let closed_at = datetime_field(value, "closedAt");
    // `gh` reports MERGED as its own state, but a host that only reports
    // OPEN/CLOSED still carries `mergedAt`. Trust the timestamp either way so
    // a merged pull request never renders as merely closed.
    let state = if merged_at.is_some() {
        "merged".to_owned()
    } else {
        state
    };
    let labels = string_array_path(value, &["labels"], "name");
    let checks: Vec<CodeDeliveryCheck> = value
        .get("statusCheckRollup")
        .and_then(Value::as_array)
        .map(|checks| checks.iter().filter_map(parse_check).collect::<Vec<_>>())
        .unwrap_or_default();
    let checks_loaded = value.get("statusCheckRollup").is_some();
    let attention_reasons = pull_request_attention(
        &state,
        draft,
        review_decision.as_deref(),
        mergeable.as_deref(),
        merge_state_status.as_deref(),
        &checks,
    );
    let ready_to_merge = state == "open"
        && !draft
        && checks_loaded
        && attention_reasons.is_empty()
        && !checks
            .iter()
            .any(|check| check.bucket == PullRequestCheckBucket::Pending)
        && !matches!(review_decision.as_deref(), Some("review_required"));
    let workspace_links = links_for_pr(
        repository,
        number,
        head_sha.as_deref(),
        &head_branch,
        workspaces,
    );
    Some(CodeDeliveryPullRequestSummary {
        id: format!("{}#{number}", repository_key_ref(repository)),
        repository: repository.clone(),
        number,
        url,
        title,
        state,
        draft,
        author,
        author_avatar_url,
        head_branch,
        base_branch,
        head_sha,
        review_decision,
        mergeable,
        merge_state_status,
        auto_merge_enabled: value
            .get("autoMergeRequest")
            .is_some_and(|request| !request.is_null()),
        checks,
        attention_reasons,
        ready_to_merge,
        workspace_links,
        labels,
        created_at: datetime_field(value, "createdAt").unwrap_or_else(Utc::now),
        updated_at: datetime_field(value, "updatedAt").unwrap_or_else(Utc::now),
        merged_at,
        closed_at,
    })
}

fn parse_check(value: &Value) -> Option<CodeDeliveryCheck> {
    let name = text_field(value, "name")
        .or_else(|| text_field(value, "context"))
        .or_else(|| text_field(value, "workflowName"))?;
    let token = text_field(value, "conclusion")
        .or_else(|| text_field(value, "state"))
        .or_else(|| text_field(value, "status"))
        .unwrap_or_else(|| "pending".into())
        .to_ascii_lowercase();
    let bucket = match token.as_str() {
        "success" | "neutral" => PullRequestCheckBucket::Pass,
        "skipped" | "cancelled" | "canceled" => PullRequestCheckBucket::Skipped,
        "queued" | "in_progress" | "pending" | "expected" | "requested" | "waiting" => {
            PullRequestCheckBucket::Pending
        }
        _ => PullRequestCheckBucket::Fail,
    };
    let url = text_field(value, "detailsUrl").or_else(|| text_field(value, "targetUrl"));
    Some(CodeDeliveryCheck {
        name,
        bucket,
        detail: Some(token),
        workflow_run_id: url.as_deref().and_then(workflow_run_id_from_url),
        url,
    })
}

fn workflow_run_id_from_url(url: &str) -> Option<u64> {
    let (_, tail) = url.split_once("/actions/runs/")?;
    tail.split('/').next()?.parse().ok()
}

fn pull_request_attention(
    state: &str,
    draft: bool,
    review_decision: Option<&str>,
    mergeable: Option<&str>,
    merge_state_status: Option<&str>,
    checks: &[CodeDeliveryCheck],
) -> Vec<CodeDeliveryPrAttentionReason> {
    if state != "open" || draft {
        return Vec::new();
    }
    let mut reasons = Vec::new();
    if review_decision == Some("changes_requested") {
        reasons.push(CodeDeliveryPrAttentionReason::ChangesRequested);
    }
    if checks
        .iter()
        .any(|check| check.bucket == PullRequestCheckBucket::Fail)
    {
        reasons.push(CodeDeliveryPrAttentionReason::ChecksFailed);
    }
    if mergeable == Some("conflicting") || merge_state_status == Some("dirty") {
        reasons.push(CodeDeliveryPrAttentionReason::Conflicts);
    }
    if merge_state_status == Some("behind") {
        reasons.push(CodeDeliveryPrAttentionReason::Behind);
    }
    if merge_state_status == Some("blocked") {
        reasons.push(CodeDeliveryPrAttentionReason::Blocked);
    }
    reasons
}

async fn fetch_runs(
    runtime: &CodeRuntime,
    binary: &Path,
    target: &CodeGitHubRepositoryTarget,
    workspaces: &[WorkspaceIndexEntry],
    fetch_workflows: bool,
    fetch_deployments: bool,
    force_refresh: bool,
) -> Result<FetchedRuns, String> {
    let repository =
        resolve_repository_cached(runtime, binary, target, None, force_refresh).await?;
    let workflow_endpoint = api_endpoint(target, "actions/runs?per_page=100");
    let deployments_endpoint = api_endpoint(target, "deployments?per_page=100");
    let workflow_read = async {
        if fetch_workflows {
            run_api_json(binary, &target.host, &workflow_endpoint)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let deployment_read = async {
        if fetch_deployments {
            run_api_json(binary, &target.host, &deployments_endpoint)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let (workflow_runs, deployments) = tokio::join!(workflow_read, deployment_read,);
    Ok(collect_run_sources(
        target,
        &repository,
        workspaces,
        workflow_runs,
        deployments,
    ))
}

fn collect_run_sources(
    target: &CodeGitHubRepositoryTarget,
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
    workflow_runs: Result<Option<Value>, String>,
    deployments: Result<Option<Value>, String>,
) -> FetchedRuns {
    let mut fetched = FetchedRuns::default();
    match workflow_runs {
        Ok(Some(value)) => {
            if let Some(runs) = value.get("workflow_runs").and_then(Value::as_array) {
                fetched.items.extend(
                    runs.iter()
                        .filter_map(|run| parse_workflow_run(repository, run, workspaces)),
                );
            }
        }
        Ok(None) => {}
        Err(message) => fetched
            .errors
            .push(detail_source_error(target, "workflow runs", message)),
    }
    match deployments {
        Ok(Some(value)) => fetched.items.extend(
            value
                .as_array()
                .into_iter()
                .flatten()
                .take(MAX_REMOTE_ITEMS_PER_REPO)
                .filter_map(|deployment| {
                    parse_deployment(repository, deployment, None, workspaces)
                }),
        ),
        Ok(None) => {}
        Err(message) => fetched
            .errors
            .push(detail_source_error(target, "deployments", message)),
    }
    fetched
}

fn run_remote_scope(query: &CodeDeliveryRunQuery) -> (&'static str, bool, bool) {
    let fetch_workflows =
        query.kinds.is_empty() || query.kinds.contains(&CodeDeliveryRunKind::WorkflowRun);
    let fetch_deployments =
        query.kinds.is_empty() || query.kinds.contains(&CodeDeliveryRunKind::Deployment);
    let scope = match (fetch_workflows, fetch_deployments) {
        (true, false) => "workflows",
        (false, true) => "deployments",
        _ => "all",
    };
    (scope, fetch_workflows, fetch_deployments)
}

fn parse_workflow_run(
    repository: &CodeGitHubRepositoryRef,
    value: &Value,
    workspaces: &[WorkspaceIndexEntry],
) -> Option<CodeDeliveryRunSummary> {
    let id = u64_field(value, "id")?;
    let status = text_field(value, "status")?.to_ascii_lowercase();
    let conclusion = normalized_optional(value, "conclusion");
    let branch = text_field(value, "head_branch");
    let sha = text_field(value, "head_sha");
    let attention_reasons = run_attention(conclusion.as_deref());
    Some(CodeDeliveryRunSummary {
        id: format!("{}:workflow:{id}", repository_key_ref(repository)),
        repository: repository.clone(),
        kind: CodeDeliveryRunKind::WorkflowRun,
        github_id: id,
        run_attempt: u64_field(value, "run_attempt"),
        name: text_field(value, "display_title")
            .or_else(|| text_field(value, "name"))
            .unwrap_or_else(|| format!("Workflow run {id}")),
        url: text_field(value, "html_url").unwrap_or_else(|| repository.url.clone()),
        status,
        conclusion,
        workflow: text_field(value, "name").or_else(|| text_field(value, "path")),
        environment: None,
        branch: branch.clone(),
        sha: sha.clone(),
        event: text_field(value, "event"),
        actor: value
            .get("actor")
            .and_then(|actor| actor.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        attention_reasons,
        workspace_links: links_for_run(repository, sha.as_deref(), branch.as_deref(), workspaces),
        created_at: datetime_field(value, "created_at").unwrap_or_else(Utc::now),
        updated_at: datetime_field(value, "updated_at").unwrap_or_else(Utc::now),
    })
}

fn parse_deployment(
    repository: &CodeGitHubRepositoryRef,
    value: &Value,
    latest_status: Option<&CodeDeliveryDeploymentStatus>,
    workspaces: &[WorkspaceIndexEntry],
) -> Option<CodeDeliveryRunSummary> {
    let id = u64_field(value, "id")?;
    let branch = text_field(value, "ref");
    let sha = text_field(value, "sha");
    let status = latest_status
        .map(|status| status.state.clone())
        .unwrap_or_else(|| "unknown".into());
    let conclusion = (!matches!(
        status.as_str(),
        "unknown" | "pending" | "queued" | "in_progress"
    ))
    .then_some(status.clone());
    let environment = text_field(value, "environment");
    let url = latest_status
        .and_then(|status| {
            status
                .environment_url
                .clone()
                .or_else(|| status.log_url.clone())
        })
        .unwrap_or_else(|| format!("{}/deployments", repository.url));
    Some(CodeDeliveryRunSummary {
        id: format!("{}:deployment:{id}", repository_key_ref(repository)),
        repository: repository.clone(),
        kind: CodeDeliveryRunKind::Deployment,
        github_id: id,
        run_attempt: None,
        name: environment
            .clone()
            .map(|environment| format!("Deploy to {environment}"))
            .unwrap_or_else(|| format!("Deployment {id}")),
        url,
        status: status.clone(),
        conclusion: conclusion.clone(),
        workflow: None,
        environment,
        branch: branch.clone(),
        sha: sha.clone(),
        event: Some("deployment".into()),
        actor: value
            .get("creator")
            .and_then(|creator| creator.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        attention_reasons: run_attention(conclusion.as_deref()),
        workspace_links: links_for_run(repository, sha.as_deref(), branch.as_deref(), workspaces),
        created_at: datetime_field(value, "created_at").unwrap_or_else(Utc::now),
        updated_at: latest_status
            .map(|status| status.created_at)
            .or_else(|| datetime_field(value, "updated_at"))
            .unwrap_or_else(Utc::now),
    })
}

fn run_attention(conclusion: Option<&str>) -> Vec<CodeDeliveryRunAttentionReason> {
    match conclusion {
        Some("failure" | "error") => vec![CodeDeliveryRunAttentionReason::Failure],
        Some("timed_out") => vec![CodeDeliveryRunAttentionReason::TimedOut],
        Some("action_required") => vec![CodeDeliveryRunAttentionReason::ActionRequired],
        Some("startup_failure") => vec![CodeDeliveryRunAttentionReason::StartupFailure],
        _ => Vec::new(),
    }
}

fn parse_workflow_job(value: &Value) -> Option<CodeDeliveryWorkflowJob> {
    let steps = value
        .get("steps")
        .and_then(Value::as_array)
        .map(|steps| {
            steps
                .iter()
                .filter(|step| {
                    normalized_optional(step, "conclusion")
                        .is_some_and(|token| matches!(token.as_str(), "failure" | "timed_out"))
                })
                .filter_map(|step| text_field(step, "name"))
                .collect()
        })
        .unwrap_or_default();
    Some(CodeDeliveryWorkflowJob {
        id: u64_field(value, "id")?,
        name: text_field(value, "name")?,
        status: text_field(value, "status")?.to_ascii_lowercase(),
        conclusion: normalized_optional(value, "conclusion"),
        url: text_field(value, "html_url").unwrap_or_default(),
        started_at: datetime_field(value, "started_at"),
        completed_at: datetime_field(value, "completed_at"),
        failed_steps: steps,
    })
}

fn parse_deployment_status(value: &Value) -> Option<CodeDeliveryDeploymentStatus> {
    Some(CodeDeliveryDeploymentStatus {
        id: u64_field(value, "id")?,
        state: text_field(value, "state")?.to_ascii_lowercase(),
        description: text_field(value, "description").unwrap_or_default(),
        environment_url: text_field(value, "environment_url").filter(|value| !value.is_empty()),
        log_url: text_field(value, "log_url").filter(|value| !value.is_empty()),
        created_at: datetime_field(value, "created_at").unwrap_or_else(Utc::now),
    })
}

fn links_for_pr(
    repository: &CodeGitHubRepositoryRef,
    number: u64,
    head_sha: Option<&str>,
    head_branch: &str,
    workspaces: &[WorkspaceIndexEntry],
) -> Vec<CodeDeliveryWorkspaceLink> {
    workspace_links(repository, workspaces, |entry| {
        if entry
            .workspace
            .pr
            .as_ref()
            .is_some_and(|pr| pr.number == number)
        {
            return Some(true);
        }
        if head_sha.is_some_and(|sha| entry.head_sha.as_deref() == Some(sha)) {
            return Some(true);
        }
        (entry.workspace.branch_name == head_branch).then_some(false)
    })
}

fn links_for_run(
    repository: &CodeGitHubRepositoryRef,
    sha: Option<&str>,
    branch: Option<&str>,
    workspaces: &[WorkspaceIndexEntry],
) -> Vec<CodeDeliveryWorkspaceLink> {
    workspace_links(repository, workspaces, |entry| {
        if sha.is_some_and(|sha| entry.head_sha.as_deref() == Some(sha)) {
            return Some(true);
        }
        branch
            .is_some_and(|branch| entry.workspace.branch_name == branch)
            .then_some(false)
    })
}

fn workspace_links(
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
    matches: impl Fn(&WorkspaceIndexEntry) -> Option<bool>,
) -> Vec<CodeDeliveryWorkspaceLink> {
    let key = repository_key_ref(repository);
    let mut links = workspaces
        .iter()
        .filter(|entry| entry.repository_key == key)
        .filter_map(|entry| {
            let exact = matches(entry)?;
            Some((
                entry.workspace.created_at,
                CodeDeliveryWorkspaceLink {
                    workspace_id: entry.workspace.id,
                    repo_id: entry.workspace.repo_id,
                    title: entry.workspace.title.clone(),
                    branch_name: entry.workspace.branch_name.clone(),
                    status: entry.workspace.status,
                    exact,
                },
            ))
        })
        .collect::<Vec<_>>();
    links.sort_by(|(left_time, left), (right_time, right)| {
        workspace_status_rank(left.status)
            .cmp(&workspace_status_rank(right.status))
            .then_with(|| right.exact.cmp(&left.exact))
            .then_with(|| right_time.cmp(left_time))
    });
    links.into_iter().map(|(_, link)| link).collect()
}

fn workspace_status_rank(status: CodeWorkspaceStatus) -> u8 {
    if status == CodeWorkspaceStatus::Archived {
        1
    } else {
        0
    }
}

fn pull_request_matches(
    item: &CodeDeliveryPullRequestSummary,
    query: &CodeDeliveryPullRequestQuery,
) -> bool {
    if let Some(after) = query.updated_after {
        if item.updated_at < after {
            return false;
        }
    }
    if query.attention_only && item.attention_reasons.is_empty() {
        return false;
    }
    if query.ready_only && !item.ready_to_merge {
        return false;
    }
    if let Some(linked) = query.tidebreak_linked {
        if item.workspace_links.is_empty() == linked {
            return false;
        }
    }
    if !query.states.is_empty() && !contains_token(&query.states, &item.state) {
        return false;
    }
    if !query.review_states.is_empty()
        && !item
            .review_decision
            .as_deref()
            .is_some_and(|state| contains_token(&query.review_states, state))
    {
        return false;
    }
    if !query.authors.is_empty()
        && !item
            .author
            .as_deref()
            .is_some_and(|author| contains_token(&query.authors, author))
    {
        return false;
    }
    if !query.check_states.is_empty() {
        let has = item.checks.iter().any(|check| {
            let token = match check.bucket {
                PullRequestCheckBucket::Pass => "pass",
                PullRequestCheckBucket::Pending => "pending",
                PullRequestCheckBucket::Fail => "fail",
                PullRequestCheckBucket::Skipped => "skipped",
            };
            contains_token(&query.check_states, token)
        });
        if !has {
            return false;
        }
    }
    if let Some(search) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let needle = search.to_ascii_lowercase();
        let haystack = format!(
            "{} {} {} {} {} {}",
            item.title,
            item.number,
            item.repository.name_with_owner,
            item.author.as_deref().unwrap_or_default(),
            item.head_branch,
            item.base_branch,
        )
        .to_ascii_lowercase();
        if !haystack.contains(&needle) {
            return false;
        }
    }
    true
}

fn run_matches(item: &CodeDeliveryRunSummary, query: &CodeDeliveryRunQuery) -> bool {
    if let Some(after) = query.created_after {
        if item.created_at < after {
            return false;
        }
    }
    if query.attention_only && item.attention_reasons.is_empty() {
        return false;
    }
    if let Some(linked) = query.tidebreak_linked {
        if item.workspace_links.is_empty() == linked {
            return false;
        }
    }
    if !query.kinds.is_empty() && !query.kinds.contains(&item.kind) {
        return false;
    }
    if !query.statuses.is_empty() && !contains_token(&query.statuses, &item.status) {
        return false;
    }
    if !query.conclusions.is_empty()
        && !item
            .conclusion
            .as_deref()
            .is_some_and(|value| contains_token(&query.conclusions, value))
    {
        return false;
    }
    if !optional_filter(&query.workflows, item.workflow.as_deref())
        || !optional_filter(&query.environments, item.environment.as_deref())
        || !optional_filter(&query.branches, item.branch.as_deref())
        || !optional_filter(&query.events, item.event.as_deref())
        || !optional_filter(&query.actors, item.actor.as_deref())
    {
        return false;
    }
    if let Some(search) = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let needle = search.to_ascii_lowercase();
        let haystack = format!(
            "{} {} {} {} {} {}",
            item.name,
            item.repository.name_with_owner,
            item.workflow.as_deref().unwrap_or_default(),
            item.environment.as_deref().unwrap_or_default(),
            item.branch.as_deref().unwrap_or_default(),
            item.actor.as_deref().unwrap_or_default(),
        )
        .to_ascii_lowercase();
        if !haystack.contains(&needle) {
            return false;
        }
    }
    true
}

fn optional_filter(filters: &[String], value: Option<&str>) -> bool {
    filters.is_empty() || value.is_some_and(|value| contains_token(filters, value))
}

fn contains_token(values: &[String], target: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(target))
}

fn paginate<T>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: Option<u16>,
) -> Result<(Vec<T>, Option<String>), ServerError> {
    let offset = match cursor {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| ServerError::bad_request("invalid delivery cursor"))?,
        None => 0,
    };
    let limit = usize::from(limit.unwrap_or(50)).clamp(1, MAX_PAGE_SIZE);
    if offset > items.len() {
        return Err(ServerError::bad_request("delivery cursor is out of range"));
    }
    let end = (offset + limit).min(items.len());
    let next = (end < items.len()).then(|| end.to_string());
    Ok((items.into_iter().skip(offset).take(limit).collect(), next))
}

fn parse_issue_comments(value: &Value) -> Vec<PullRequestComment> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comment| parse_comment(comment, PullRequestCommentKind::Issue, "created_at"))
        .collect()
}

fn parse_reviews(value: &Value) -> Vec<PullRequestComment> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comment| {
            let mut parsed =
                parse_comment(comment, PullRequestCommentKind::Review, "submitted_at")?;
            parsed.review_state = normalized_optional(comment, "state");
            Some(parsed)
        })
        .collect()
}

fn parse_inline_comments(value: &Value) -> Vec<PullRequestComment> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comment| {
            let mut parsed = parse_comment(comment, PullRequestCommentKind::Inline, "created_at")?;
            parsed.path = text_field(comment, "path");
            parsed.line =
                u64_field(comment, "line").or_else(|| u64_field(comment, "original_line"));
            Some(parsed)
        })
        .collect()
}

fn parse_comment(
    value: &Value,
    kind: PullRequestCommentKind,
    created_field: &str,
) -> Option<PullRequestComment> {
    let body = text_field(value, "body")?;
    if body.trim().is_empty() {
        return None;
    }
    Some(PullRequestComment {
        kind,
        id: text_field(value, "node_id")
            .or_else(|| u64_field(value, "id").map(|id| id.to_string())),
        author: value
            .get("user")
            .and_then(|user| user.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        avatar_url: value
            .get("user")
            .and_then(|user| user.get("avatar_url"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        url: text_field(value, "html_url"),
        body,
        review_state: None,
        path: None,
        line: None,
        created_at: text_field(value, created_field),
    })
}

fn string_array_path(value: &Value, path: &[&str], field: &str) -> Vec<String> {
    let mut current = value;
    for segment in path {
        let Some(next) = current.get(*segment) else {
            return Vec::new();
        };
        current = next;
    }
    current
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| text_field(item, field))
        .collect()
}

fn source_error(
    repository: Option<CodeGitHubRepositoryTarget>,
    message: String,
) -> CodeDeliverySourceError {
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("rate limit") {
        "rate_limited"
    } else if lower.contains("authentication") || lower.contains("auth login") {
        "gh_signed_out"
    } else if lower.contains("not found") || lower.contains("http 404") {
        "not_found"
    } else if lower.contains("forbidden") || lower.contains("http 403") {
        "forbidden"
    } else if is_transient_github_error(&message) {
        "transient"
    } else {
        "github"
    };
    // A shed gateway request already survived a retry by the time it lands
    // here. `HTTP 504: 504 Gateway Timeout (https://api.github.com/graphql)`
    // tells a reader nothing they can act on, so say what it means instead.
    let message = if kind == "transient" {
        let name = repository
            .as_ref()
            .map(|target| format!("{}/{}", target.owner, target.name))
            .unwrap_or_else(|| "A repository".into());
        format!("{name} did not answer in time. The next refresh retries it.")
    } else {
        message
    };
    CodeDeliverySourceError {
        repository,
        kind: kind.into(),
        message,
        retry_at: None,
    }
}

fn detail_source_error(
    repository: &CodeGitHubRepositoryTarget,
    section: &str,
    message: String,
) -> CodeDeliverySourceError {
    let mut error = source_error(Some(repository.clone()), message);
    error.message = format!("Could not load {section}: {}", error.message);
    error
}

fn record_full_detail_page(
    errors: &mut Vec<CodeDeliverySourceError>,
    repository: &CodeGitHubRepositoryTarget,
    section: &str,
    item_count: Option<usize>,
) {
    if item_count != Some(GITHUB_DETAIL_PAGE_SIZE) {
        return;
    }
    errors.push(CodeDeliverySourceError {
        repository: Some(repository.clone()),
        kind: "truncated".into(),
        message: format!(
            "{section} may be incomplete because GitHub returned the one-page limit of {GITHUB_DETAIL_PAGE_SIZE} items"
        ),
        retry_at: None,
    });
}

fn observation_error_kind(observation: &GhObservation) -> &'static str {
    if !observation.found {
        "gh_absent"
    } else if observation.authenticated == Some(false) {
        "gh_signed_out"
    } else {
        "gh_unavailable"
    }
}

fn observation_source_error(observation: &GhObservation) -> CodeDeliverySourceError {
    CodeDeliverySourceError {
        repository: None,
        kind: observation_error_kind(observation).into(),
        message: observation.remediation.clone(),
        retry_at: None,
    }
}

fn map_gh_error(error: gh::GhError) -> ServerError {
    match error {
        gh::GhError::GhAbsent { instructions } => {
            ServerError::conflict_kind("gh_absent", instructions)
        }
        gh::GhError::GhSignedOut { instructions } => {
            ServerError::conflict_kind("gh_signed_out", instructions)
        }
        gh::GhError::MergeBlocked(message) => {
            ServerError::conflict_kind("pr_not_mergeable", message)
        }
        gh::GhError::AuthFailed(message) => ServerError::conflict_kind("git_auth_failed", message),
        gh::GhError::PushFailed(message) => ServerError::conflict_kind("git_push_failed", message),
        gh::GhError::NothingToCommit => ServerError::conflict("nothing to commit"),
        gh::GhError::User(message) => {
            if let Some(message) = message.strip_prefix(gh::GH_UNAVAILABLE_PREFIX) {
                ServerError::conflict_kind("gh_unavailable", message)
            } else if let Some(message) = message.strip_prefix(gh::PR_HEAD_CHANGED_PREFIX) {
                ServerError::conflict_kind("pr_head_changed", message)
            } else {
                ServerError::bad_request_kind("github", message)
            }
        }
        gh::GhError::Internal(message) => ServerError::internal(message),
    }
}

fn merge_method(method: CodePrMergeMethod) -> gh::MergeMethod {
    match method {
        CodePrMergeMethod::Squash => gh::MergeMethod::Squash,
        CodePrMergeMethod::Merge => gh::MergeMethod::Merge,
        CodePrMergeMethod::Rebase => gh::MergeMethod::Rebase,
    }
}

fn aggregate_cache_key(
    owner: &OwnerId,
    kind: &str,
    repositories: &[CodeGitHubRepositoryTarget],
) -> String {
    let mut keys = repositories.iter().map(repository_key).collect::<Vec<_>>();
    keys.sort();
    format!("{owner}:{kind}:{}", keys.join(","))
}

fn repository_key(target: &CodeGitHubRepositoryTarget) -> String {
    format!(
        "{}/{}/{}",
        target.host.to_ascii_lowercase(),
        target.owner.to_ascii_lowercase(),
        target.name.to_ascii_lowercase()
    )
}

fn repository_key_ref(repository: &CodeGitHubRepositoryRef) -> String {
    format!(
        "{}/{}/{}",
        repository.host.to_ascii_lowercase(),
        repository.owner.to_ascii_lowercase(),
        repository.name.to_ascii_lowercase()
    )
}

fn api_endpoint(target: &CodeGitHubRepositoryTarget, tail: &str) -> String {
    format!("repos/{}/{}/{}", target.owner, target.name, tail)
}

async fn run_api_json(binary: &Path, host: &str, endpoint: &str) -> Result<Value, String> {
    let mut args = vec!["api".to_owned(), endpoint.to_owned()];
    if host != "github.com" {
        args.extend(["--hostname".to_owned(), host.to_owned()]);
    }
    let borrowed = args.iter().map(String::as_str).collect::<Vec<_>>();
    let raw = gh::run_gh(Path::new("."), binary, &borrowed, GH_READ_TIMEOUT).await?;
    serde_json::from_str(&raw).map_err(|error| format!("invalid GitHub response: {error}"))
}

async fn git_read(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("GIT_TERMINAL_PROMPT", "0");
    let child = command
        .spawn()
        .map_err(|error| format!("failed to spawn git: {error}"))?;
    let output = timeout(GIT_READ_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("git {} timed out", args.join(" ")))?
        .map_err(|error| format!("git {} failed: {error}", args.join(" ")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

fn normalized_optional(value: &Value, key: &str) -> Option<String> {
    text_field(value, key)
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "null")
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn datetime_field(value: &Value, key: &str) -> Option<DateTime<Utc>> {
    text_field(value, key).and_then(|value| {
        DateTime::parse_from_rfc3339(&value)
            .ok()
            .map(|value| value.with_timezone(&Utc))
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;

    #[test]
    fn repository_inputs_cover_https_ssh_and_short_forms() {
        for (input, expected) in [
            ("openai/codex", "github.com/openai/codex"),
            (
                "https://github.com/openai/codex.git",
                "github.com/openai/codex",
            ),
            ("git@github.com:openai/codex.git", "github.com/openai/codex"),
            (
                "github.example.com/platform/app",
                "github.example.com/platform/app",
            ),
        ] {
            let parsed = parse_repository_input(input).unwrap();
            assert_eq!(repository_key(&parsed), expected);
        }
    }

    #[test]
    fn exact_pull_request_targets_group_repositories_and_numbers() {
        let grouped = dedupe_numbered_targets(vec![
            (
                CodeGitHubRepositoryTarget {
                    host: "GitHub.COM".into(),
                    owner: "brightwave-inc".into(),
                    name: "tidebreak.git".into(),
                },
                vec![41, 40, 41],
            ),
            (
                CodeGitHubRepositoryTarget {
                    host: "github.com".into(),
                    owner: "brightwave-inc".into(),
                    name: "tidebreak".into(),
                },
                vec![42, 0],
            ),
        ])
        .unwrap();

        assert_eq!(grouped.len(), 1);
        assert_eq!(
            repository_key(&grouped[0].0),
            "github.com/brightwave-inc/tidebreak"
        );
        assert_eq!(grouped[0].1, vec![40, 41, 42]);
    }

    #[test]
    fn owner_repository_catalog_stays_cached_until_owner_invalidation() {
        let cache = DeliveryCache::default();
        let owner = OwnerId::local();
        let key = owner.to_string();
        cache.owner_repositories.lock().unwrap().insert(
            key.clone(),
            CachedValue {
                fetched_at: Instant::now()
                    .checked_sub(LIST_CACHE_TTL + Duration::from_secs(1))
                    .unwrap(),
                value: OwnerRepositoryCatalog::default(),
            },
        );

        assert!(cache.owner_repositories(&key).is_some());
        cache.invalidate_owner(&owner);
        assert!(cache.owner_repositories(&key).is_none());
    }

    #[test]
    fn owner_invalidation_rejects_in_flight_catalog_and_workspace_index_writes() {
        let cache = Arc::new(DeliveryCache::default());
        let owner = OwnerId::local();
        let key = owner.to_string();
        let stale_generation = cache.owner_cache_generation(&key);
        let loader_ready = Arc::new(Barrier::new(2));
        let resume_loader = Arc::new(Barrier::new(2));
        let loader = {
            let cache = Arc::clone(&cache);
            let key = key.clone();
            let owner = owner.clone();
            let loader_ready = Arc::clone(&loader_ready);
            let resume_loader = Arc::clone(&resume_loader);
            std::thread::spawn(move || {
                loader_ready.wait();
                resume_loader.wait();
                (
                    cache.put_owner_repositories_if_current(
                        &key,
                        stale_generation,
                        owner_catalog_marker("stale"),
                    ),
                    cache.put_workspace_index_if_current(
                        &key,
                        stale_generation,
                        workspace_index_marker(&owner, "stale"),
                    ),
                )
            })
        };

        loader_ready.wait();
        cache.invalidate_owner(&owner);
        let fresh_generation = cache.owner_cache_generation(&key);
        assert_ne!(fresh_generation, stale_generation);
        assert!(cache.put_owner_repositories_if_current(
            &key,
            fresh_generation,
            owner_catalog_marker("fresh"),
        ));
        assert!(cache.put_workspace_index_if_current(
            &key,
            fresh_generation,
            workspace_index_marker(&owner, "fresh"),
        ));

        resume_loader.wait();
        let (catalog_published, index_published) = loader.join().unwrap();
        assert!(!catalog_published);
        assert!(!index_published);
        assert_eq!(
            cache.owner_repositories(&key).unwrap().value.errors[0].message,
            "fresh"
        );
        assert_eq!(
            cache.workspace_index(&key).unwrap().value[0]
                .head_sha
                .as_deref(),
            Some("fresh")
        );
    }

    fn owner_catalog_marker(message: &str) -> OwnerRepositoryCatalog {
        OwnerRepositoryCatalog {
            entries: Vec::new(),
            errors: vec![CodeDeliverySourceError {
                repository: None,
                kind: "test".into(),
                message: message.into(),
                retry_at: None,
            }],
        }
    }

    fn workspace_index_marker(owner: &OwnerId, marker: &str) -> Vec<WorkspaceIndexEntry> {
        vec![WorkspaceIndexEntry {
            workspace: CodeWorkspace {
                id: tidebreak_core::WorkspaceId::new(),
                owner: owner.clone(),
                repo_id: RepoId::new(),
                title: marker.into(),
                worktree_path: format!("/tmp/{marker}"),
                branch_name: format!("tidebreak/{marker}"),
                base_ref: "main".into(),
                status: CodeWorkspaceStatus::Active,
                pr: None,
                created_at: Utc::now(),
                archived_at: None,
                released_at: None,
                released_tip: None,
                bundle_bytes: None,
            },
            repository_key: format!("github.com/brightwave-inc/{marker}"),
            head_sha: Some(marker.into()),
        }]
    }

    fn repository_ref() -> CodeGitHubRepositoryRef {
        CodeGitHubRepositoryRef {
            host: "github.com".into(),
            owner: "brightwave-inc".into(),
            name: "tidebreak".into(),
            name_with_owner: "brightwave-inc/tidebreak".into(),
            url: "https://github.com/brightwave-inc/tidebreak".into(),
            default_branch: Some("main".into()),
            tidebreak_repo_id: None,
        }
    }

    fn repository_target(name: &str) -> CodeGitHubRepositoryTarget {
        CodeGitHubRepositoryTarget {
            host: "github.com".into(),
            owner: "brightwave-inc".into(),
            name: name.into(),
        }
    }

    fn code_repo(id: RepoId, name: &str) -> CodeRepo {
        CodeRepo {
            id,
            owner: OwnerId::local(),
            root_path: format!("/tmp/{name}"),
            display_name: name.into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
            removed_at: None,
            cloned_from: None,
        }
    }

    fn pull_request_query() -> CodeDeliveryPullRequestQuery {
        CodeDeliveryPullRequestQuery {
            repositories: Vec::new(),
            search: None,
            states: Vec::new(),
            review_states: Vec::new(),
            check_states: Vec::new(),
            authors: Vec::new(),
            attention_only: false,
            ready_only: false,
            tidebreak_linked: None,
            updated_after: None,
            cursor: None,
            limit: None,
            refresh: false,
        }
    }

    fn run_query() -> CodeDeliveryRunQuery {
        CodeDeliveryRunQuery {
            repositories: Vec::new(),
            search: None,
            kinds: Vec::new(),
            statuses: Vec::new(),
            conclusions: Vec::new(),
            workflows: Vec::new(),
            environments: Vec::new(),
            branches: Vec::new(),
            events: Vec::new(),
            actors: Vec::new(),
            attention_only: false,
            tidebreak_linked: None,
            created_after: None,
            cursor: None,
            limit: None,
            refresh: false,
        }
    }

    #[test]
    fn focused_queries_avoid_unrelated_remote_rows() {
        let mut pull_requests = pull_request_query();
        pull_requests.states = vec!["open".into()];
        assert_eq!(pull_request_remote_plan(&pull_requests).state, "open");
        assert!(pull_request_remote_plan(&pull_requests).checks_loaded);
        pull_requests.states.clear();
        pull_requests.attention_only = true;
        assert_eq!(pull_request_remote_plan(&pull_requests).state, "open");
        pull_requests.attention_only = false;
        let settled = pull_request_remote_plan(&pull_requests);
        assert_eq!(settled.state, "all");
        assert!(!settled.checks_loaded);
        assert!(!settled.fields.contains("statusCheckRollup"));

        pull_requests.states = vec!["merged".into()];
        assert_eq!(pull_request_remote_plan(&pull_requests).state, "merged");

        let mut runs = run_query();
        runs.kinds = vec![CodeDeliveryRunKind::WorkflowRun];
        assert_eq!(run_remote_scope(&runs), ("workflows", true, false));
        runs.kinds = vec![CodeDeliveryRunKind::Deployment];
        assert_eq!(run_remote_scope(&runs), ("deployments", false, true));
        runs.kinds.clear();
        assert_eq!(run_remote_scope(&runs), ("all", true, true));
    }

    #[test]
    fn run_sources_keep_rows_and_report_each_failed_source() {
        let target = repository_target("tidebreak");
        let workflows = serde_json::json!({
            "workflow_runs": [{
                "id": 41,
                "run_attempt": 3,
                "status": "completed",
                "conclusion": "success",
                "name": "Desktop CI"
            }]
        });
        let fetched = collect_run_sources(
            &target,
            &repository_ref(),
            &[],
            Ok(Some(workflows)),
            Err("HTTP 503: Service Unavailable".into()),
        );

        assert_eq!(fetched.items.len(), 1);
        assert_eq!(fetched.items[0].kind, CodeDeliveryRunKind::WorkflowRun);
        assert_eq!(fetched.items[0].run_attempt, Some(3));
        assert_eq!(fetched.errors.len(), 1);
        assert!(fetched.errors[0].message.contains("deployments"));

        let deployments = serde_json::json!([{
            "id": 91,
            "environment": "production"
        }]);
        let fetched = collect_run_sources(
            &target,
            &repository_ref(),
            &[],
            Err("HTTP 503: Service Unavailable".into()),
            Ok(Some(deployments)),
        );

        assert_eq!(fetched.items.len(), 1);
        assert_eq!(fetched.items[0].kind, CodeDeliveryRunKind::Deployment);
        assert_eq!(fetched.errors.len(), 1);
        assert!(fetched.errors[0].message.contains("workflow runs"));
    }

    #[test]
    fn member_authorization_drops_removed_repositories_without_rescanning_git() {
        let live_id = RepoId::new();
        let removed_id = RepoId::new();
        let catalog = OwnerRepositoryCatalog {
            entries: vec![
                OwnerRepositoryEntry {
                    repo: code_repo(live_id, "live"),
                    target: repository_target("live"),
                },
                OwnerRepositoryEntry {
                    repo: code_repo(removed_id, "removed"),
                    target: repository_target("removed"),
                },
            ],
            errors: Vec::new(),
        };

        let allowed = live_catalog_target_keys(&catalog, &HashSet::from([live_id]));
        assert!(allowed.contains("github.com/brightwave-inc/live"));
        assert!(!allowed.contains("github.com/brightwave-inc/removed"));
    }

    #[test]
    fn partial_reruns_keep_every_outcome_in_stable_order() {
        let result = rerun_action_result(vec![
            CodeDeliveryRerunOutcome {
                workflow_run_id: 11,
                success: false,
                error: Some("HTTP 503".into()),
            },
            CodeDeliveryRerunOutcome {
                workflow_run_id: 10,
                success: true,
                error: None,
            },
        ]);

        assert!(!result.success);
        assert_eq!(
            result
                .rerun_outcomes
                .iter()
                .map(|outcome| outcome.workflow_run_id)
                .collect::<Vec<_>>(),
            vec![10, 11]
        );
        assert!(result.message.contains("one workflow run failed"));
    }

    #[test]
    fn a_merged_pull_request_carries_its_merge_time() {
        let value: Value = serde_json::from_str(
            r#"{
                "number": 2240,
                "title": "Cache the workspace digest",
                "state": "MERGED",
                "url": "https://github.com/brightwave-inc/tidebreak/pull/2240",
                "isDraft": false,
                "headRefName": "mara/cache",
                "baseRefName": "main",
                "labels": [{"name": "performance"}, {"name": "desktop"}],
                "mergedAt": "2026-08-19T11:41:00Z",
                "closedAt": "2026-08-19T11:41:00Z",
                "createdAt": "2026-08-17T09:05:00Z",
                "updatedAt": "2026-08-19T11:41:00Z"
            }"#,
        )
        .unwrap();
        let parsed = parse_pull_request(&repository_ref(), &value, &[]).unwrap();
        assert_eq!(parsed.state, "merged");
        assert!(parsed.merged_at.is_some());
        assert!(parsed.closed_at.is_some());
        assert_eq!(parsed.labels, vec!["performance", "desktop"]);
        // A settled pull request never asks for attention and is never ready.
        assert!(parsed.attention_reasons.is_empty());
        assert!(!parsed.ready_to_merge);
    }

    #[test]
    fn a_merge_time_outranks_a_closed_state() {
        let value: Value = serde_json::from_str(
            r#"{
                "number": 2233,
                "title": "Split the workspace route",
                "state": "CLOSED",
                "url": "https://github.com/brightwave-inc/tidebreak/pull/2233",
                "headRefName": "ines/split",
                "baseRefName": "main",
                "mergedAt": "2026-08-15T16:02:00Z",
                "closedAt": "2026-08-15T16:02:00Z"
            }"#,
        )
        .unwrap();
        let parsed = parse_pull_request(&repository_ref(), &value, &[]).unwrap();
        assert_eq!(parsed.state, "merged");
    }

    #[test]
    fn an_open_pull_request_has_no_settled_timestamps() {
        let value: Value = serde_json::from_str(
            r#"{
                "number": 2251,
                "title": "Build the delivery center",
                "state": "OPEN",
                "url": "https://github.com/brightwave-inc/tidebreak/pull/2251",
                "headRefName": "thet/delivery-center",
                "baseRefName": "main",
                "mergedAt": null,
                "closedAt": null,
                "labels": []
            }"#,
        )
        .unwrap();
        let parsed = parse_pull_request(&repository_ref(), &value, &[]).unwrap();
        assert_eq!(parsed.state, "open");
        assert!(parsed.merged_at.is_none());
        assert!(parsed.closed_at.is_none());
    }

    #[test]
    fn transient_github_failures_are_the_ones_worth_retrying() {
        for message in [
            "HTTP 504: 504 Gateway Timeout (https://api.github.com/graphql)",
            "HTTP 502: Bad Gateway",
            "gh timed out after 45s",
            "connection reset by peer",
        ] {
            assert!(is_transient_github_error(message), "{message}");
        }
        for message in [
            "HTTP 404: Not Found",
            "GraphQL: Could not resolve to a Repository",
            "gh auth login required",
        ] {
            assert!(!is_transient_github_error(message), "{message}");
        }
    }

    #[test]
    fn pull_request_files_drop_the_shapes_the_panel_cannot_draw() {
        let value: Value = serde_json::from_str(
            r#"[
                {"filename": "a.rs", "status": "modified", "additions": 3, "deletions": 1,
                 "patch": "@@ -1 +1 @@\n-old\n+new"},
                {"filename": "logo.png", "status": "added", "additions": 0, "deletions": 0},
                {"filename": "b.rs", "status": "renamed", "previous_filename": "old.rs",
                 "additions": 0, "deletions": 0},
                {"status": "modified"}
            ]"#,
        )
        .unwrap();
        let files = parse_pull_request_files(&value);
        assert_eq!(files.len(), 3, "the entry without a filename is dropped");
        assert_eq!(files[0].patch.as_deref(), Some("@@ -1 +1 @@\n-old\n+new"));
        assert!(files[1].patch.is_none(), "a binary file has no text diff");
        assert_eq!(files[2].previous_path.as_deref(), Some("old.rs"));
        assert!(pull_request_files_truncated(0, 3));
        assert!(!pull_request_files_truncated(3, 3));
    }

    #[test]
    fn deployment_lists_do_not_claim_an_unknown_status_is_pending() {
        let value: Value = serde_json::from_str(
            r#"{
                "id": 88,
                "ref": "main",
                "sha": "abcdef",
                "environment": "staging",
                "created_at": "2026-08-22T12:00:00Z",
                "updated_at": "2026-08-22T12:01:00Z"
            }"#,
        )
        .unwrap();
        let deployment = parse_deployment(&repository_ref(), &value, None, &[]).unwrap();
        assert_eq!(deployment.status, "unknown");
        assert_eq!(deployment.conclusion, None);
        assert!(deployment.attention_reasons.is_empty());
    }

    #[test]
    fn detail_failures_name_the_missing_section() {
        let target = CodeGitHubRepositoryTarget {
            host: "github.com".into(),
            owner: "brightwave-inc".into(),
            name: "tidebreak".into(),
        };
        let error = detail_source_error(&target, "changed files", "gh api timed out".into());
        assert_eq!(error.kind, "transient");
        assert!(error.message.contains("Could not load changed files"));

        let mut errors = Vec::new();
        record_full_detail_page(
            &mut errors,
            &target,
            "reviews",
            Some(GITHUB_DETAIL_PAGE_SIZE),
        );
        assert_eq!(errors[0].kind, "truncated");
        assert!(errors[0].message.contains("one-page limit"));
    }

    #[test]
    fn pr_attention_is_server_computed() {
        let checks = vec![CodeDeliveryCheck {
            name: "test".into(),
            bucket: PullRequestCheckBucket::Fail,
            detail: None,
            url: None,
            workflow_run_id: None,
        }];
        assert_eq!(
            pull_request_attention(
                "open",
                false,
                Some("changes_requested"),
                Some("conflicting"),
                Some("behind"),
                &checks,
            ),
            vec![
                CodeDeliveryPrAttentionReason::ChangesRequested,
                CodeDeliveryPrAttentionReason::ChecksFailed,
                CodeDeliveryPrAttentionReason::Conflicts,
                CodeDeliveryPrAttentionReason::Behind,
            ]
        );
        assert!(pull_request_attention(
            "open",
            true,
            Some("changes_requested"),
            Some("conflicting"),
            Some("behind"),
            &checks,
        )
        .is_empty());
    }

    #[test]
    fn run_attention_ignores_cancelled_but_keeps_actionable_failures() {
        assert!(run_attention(Some("cancelled")).is_empty());
        assert_eq!(
            run_attention(Some("timed_out")),
            vec![CodeDeliveryRunAttentionReason::TimedOut]
        );
    }

    #[test]
    fn cursors_are_bounded_offsets() {
        let (page, next) = paginate(vec![1, 2, 3], None, Some(2)).unwrap();
        assert_eq!(page, vec![1, 2]);
        assert_eq!(next.as_deref(), Some("2"));
        let (page, next) = paginate(vec![1, 2, 3], Some("2"), Some(2)).unwrap();
        assert_eq!(page, vec![3]);
        assert_eq!(next, None);
    }
}
