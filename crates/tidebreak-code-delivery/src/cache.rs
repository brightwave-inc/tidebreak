//! Per-owner caches: pull-request and run aggregates, resolved repositories, catalogs, and the workspace index.

use super::*;

#[derive(Debug, Clone)]
pub(super) struct CachedAggregate<T> {
    pub(super) fetched_at: Instant,
    pub(super) items: Vec<T>,
    pub(super) errors: Vec<CodeDeliverySourceError>,
}

#[derive(Debug, Clone)]
pub(super) struct CachedValue<T> {
    pub(super) fetched_at: Instant,
    pub(super) value: T,
}

#[derive(Debug, Clone)]
pub(super) struct OwnerRepositoryEntry {
    pub(super) repo: CodeRepo,
    pub(super) target: CodeGitHubRepositoryTarget,
}

#[derive(Debug, Clone, Default)]
pub(super) struct OwnerRepositoryCatalog {
    pub(super) entries: Vec<OwnerRepositoryEntry>,
    pub(super) errors: Vec<CodeDeliverySourceError>,
}

/// Short-lived owner/query caches. No GitHub response is durable.
#[derive(Debug, Default)]
pub struct DeliveryCache {
    pub(super) pull_requests:
        Mutex<HashMap<String, CachedAggregate<CodeDeliveryPullRequestSummary>>>,
    pub(super) runs: Mutex<HashMap<String, CachedAggregate<CodeDeliveryRunSummary>>>,
    pub(super) pull_request_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) run_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) repositories: Mutex<HashMap<String, CachedValue<CodeGitHubRepositoryRef>>>,
    pub(super) repository_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) owner_repositories: Mutex<HashMap<String, CachedValue<OwnerRepositoryCatalog>>>,
    pub(super) owner_repository_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) workspace_indexes: Mutex<HashMap<String, CachedValue<Vec<WorkspaceIndexEntry>>>>,
    pub(super) workspace_index_reads: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) owner_cache_generations: Mutex<HashMap<String, u64>>,
}

impl DeliveryCache {
    pub(super) fn pull_requests(
        &self,
        key: &str,
    ) -> Option<CachedAggregate<CodeDeliveryPullRequestSummary>> {
        let mut cache = self.pull_requests.lock().expect("delivery PR cache");
        cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
        cache.get(key).cloned()
    }

    pub(super) fn put_pull_requests(
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

    pub(super) fn pull_request_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.pull_request_reads
            .lock()
            .expect("delivery PR read locks")
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub(super) fn runs(&self, key: &str) -> Option<CachedAggregate<CodeDeliveryRunSummary>> {
        let mut cache = self.runs.lock().expect("delivery run cache");
        cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
        cache.get(key).cloned()
    }

    pub(super) fn put_runs(
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

    pub(super) fn run_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.run_reads
            .lock()
            .expect("delivery run read locks")
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub(super) fn repository(&self, key: &str) -> Option<CachedValue<CodeGitHubRepositoryRef>> {
        cached_value(&self.repositories, key, "delivery repository cache")
    }

    pub(super) fn put_repository(&self, key: String, value: CodeGitHubRepositoryRef) {
        put_cached_value(&self.repositories, key, value, "delivery repository cache");
    }

    pub(super) fn repository_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.repository_reads,
            key,
            "delivery repository read locks",
        )
    }

    pub(super) fn owner_repositories(
        &self,
        key: &str,
    ) -> Option<CachedValue<OwnerRepositoryCatalog>> {
        self.owner_repositories
            .lock()
            .expect("delivery owner repository cache")
            .get(key)
            .cloned()
    }

    pub(super) fn owner_cache_generation(&self, key: &str) -> u64 {
        self.owner_cache_generations
            .lock()
            .expect("delivery owner cache generations")
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    pub(super) fn put_owner_repositories_if_current(
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

    pub(super) fn owner_repository_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.owner_repository_reads,
            key,
            "delivery owner repository read locks",
        )
    }

    pub(super) fn workspace_index(
        &self,
        key: &str,
    ) -> Option<CachedValue<Vec<WorkspaceIndexEntry>>> {
        cached_value(
            &self.workspace_indexes,
            key,
            "delivery workspace index cache",
        )
    }

    pub(super) fn put_workspace_index_if_current(
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

    pub(super) fn workspace_index_read(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        cache_read_lock(
            &self.workspace_index_reads,
            key,
            "delivery workspace index read locks",
        )
    }

    pub fn invalidate(&self) {
        self.pull_requests
            .lock()
            .expect("delivery PR cache")
            .clear();
        self.runs.lock().expect("delivery run cache").clear();
    }

    pub fn invalidate_owner(&self, owner: &OwnerId) {
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

pub(super) fn cached_value<T: Clone>(
    cache: &Mutex<HashMap<String, CachedValue<T>>>,
    key: &str,
    label: &str,
) -> Option<CachedValue<T>> {
    let mut cache = cache.lock().expect(label);
    cache.retain(|_, value| value.fetched_at.elapsed() <= LIST_CACHE_TTL);
    cache.get(key).cloned()
}

pub(super) fn put_cached_value<T>(
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

pub(super) fn cache_read_lock(
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
pub(super) struct WorkspaceIndexEntry {
    pub(super) workspace: CodeWorkspace,
    pub(super) repository_key: String,
    pub(super) head_sha: Option<String>,
}

pub(super) async fn workspace_index(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    force_refresh: bool,
) -> Result<Vec<WorkspaceIndexEntry>, ServerError> {
    let key = owner.to_string();
    let request_started = Instant::now();
    if !force_refresh {
        if let Some(cached) = runtime.delivery_cache().workspace_index(&key) {
            return Ok(cached.value);
        }
    }

    let read = runtime.delivery_cache().workspace_index_read(&key);
    let _guard = read.lock().await;
    if let Some(cached) = runtime.delivery_cache().workspace_index(&key) {
        if !force_refresh || cached.fetched_at >= request_started {
            return Ok(cached.value);
        }
    }

    loop {
        let generation = runtime.delivery_cache().owner_cache_generation(&key);
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
            .delivery_cache()
            .put_workspace_index_if_current(&key, generation, index.clone())
        {
            return Ok(index);
        }
    }
}

pub(super) fn workspace_status_rank(status: CodeWorkspaceStatus) -> u8 {
    if status == CodeWorkspaceStatus::Archived {
        1
    } else {
        0
    }
}

pub(super) fn aggregate_cache_key(
    owner: &OwnerId,
    kind: &str,
    repositories: &[CodeGitHubRepositoryTarget],
) -> String {
    let mut keys = repositories.iter().map(repository_key).collect::<Vec<_>>();
    keys.sort();
    format!("{owner}:{kind}:{}", keys.join(","))
}

pub(super) fn repository_key(target: &CodeGitHubRepositoryTarget) -> String {
    format!(
        "{}/{}/{}",
        target.host.to_ascii_lowercase(),
        target.owner.to_ascii_lowercase(),
        target.name.to_ascii_lowercase()
    )
}

pub(super) fn repository_key_ref(repository: &CodeGitHubRepositoryRef) -> String {
    format!(
        "{}/{}/{}",
        repository.host.to_ascii_lowercase(),
        repository.owner.to_ascii_lowercase(),
        repository.name.to_ascii_lowercase()
    )
}
