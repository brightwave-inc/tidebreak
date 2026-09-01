//! Repository discovery, resolution, and target parsing.

use super::*;

pub(crate) async fn discover_repositories(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    refresh: bool,
) -> Result<CodeDeliveryRepositoriesSnapshot, ServerError> {
    let access = delivery_access(runtime, owner, refresh).await;
    let capability = access.capability.clone();
    let catalog = owner_repository_catalog(runtime, owner, refresh).await?;
    // Local-only Tidebreak checkouts are not Delivery sources. Keep them off
    // the snapshot so the page does not treat a skipped origin as a refresh
    // failure.
    let mut errors = catalog
        .errors
        .into_iter()
        .filter(|error| error.kind != "not_github")
        .collect::<Vec<_>>();

    let resolved = if let Some(reader) = access.reader.clone() {
        stream::iter(catalog.entries)
            .map(|entry| {
                let reader = reader.clone();
                async move {
                    resolve_repository_for_reader(
                        runtime,
                        owner,
                        &reader,
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

    let access = delivery_access(runtime, owner, false).await;
    let capability = access.capability.clone();

    let mut repositories = Vec::new();
    if let Some(reader) = access.reader.clone() {
        let results = stream::iter(targets)
            .map(|target| {
                let reader = reader.clone();
                async move {
                    resolve_repository_for_reader(runtime, owner, &reader, &target, None, false)
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
        errors.extend(targets.into_iter().map(|target| CodeDeliverySourceError {
            repository: Some(target),
            kind: access.unavailable_kind.into(),
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

pub(crate) async fn repository_target_from_local(
    repo: &CodeRepo,
) -> Result<CodeGitHubRepositoryTarget, String> {
    repository_target_from_path(Path::new(&repo.root_path)).await
}

/// Resolve any checkout's origin remote to a GitHub identity.
///
/// The pull-request fact detector calls this on a command's recorded cwd,
/// which may be a worktree or a clone the agent made outside every
/// registered repository (decision 77).
pub(crate) async fn repository_target_from_path(
    path: &Path,
) -> Result<CodeGitHubRepositoryTarget, String> {
    let remote = git_read(path, &["remote", "get-url", "origin"])
        .await
        .map_err(|message| format!("could not read origin remote: {message}"))?;
    parse_repository_input(&remote)
}

pub(super) async fn owner_repository_catalog(
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

pub(super) async fn ensure_delivery_targets(
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

pub(super) fn live_catalog_target_keys(
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

pub(crate) fn parse_repository_input(input: &str) -> Result<CodeGitHubRepositoryTarget, String> {
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

pub(super) fn valid_repo_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

pub(super) fn dedupe_targets(
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

pub(super) fn dedupe_numbered_targets(
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
    grouped.sort_by_key(|(target, _)| repository_key(target));
    Ok(grouped)
}

pub(super) async fn resolve_repository(
    binary: &Path,
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
) -> Result<CodeGitHubRepositoryRef, String> {
    let endpoint = format!("repos/{}/{}", target.owner, target.name);
    let value = run_api_json(binary, &target.host, &endpoint).await?;
    Ok(repository_ref_from_value(target, tidebreak_repo_id, &value))
}

pub(super) fn repository_ref_from_value(
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
    value: &Value,
) -> CodeGitHubRepositoryRef {
    let owner = value
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .unwrap_or(&target.owner)
        .to_owned();
    let name = text_field(value, "name").unwrap_or_else(|| target.name.clone());
    let name_with_owner =
        text_field(value, "full_name").unwrap_or_else(|| format!("{owner}/{name}"));
    CodeGitHubRepositoryRef {
        host: target.host.clone(),
        owner,
        name,
        name_with_owner,
        url: text_field(value, "html_url")
            .unwrap_or_else(|| format!("https://{}/{}/{}", target.host, target.owner, target.name)),
        default_branch: text_field(value, "default_branch"),
        tidebreak_repo_id,
    }
}

pub(super) async fn resolve_repository_cached(
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

pub(super) async fn resolve_repository_for_reader(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    reader: &DeliveryReader,
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
    force_refresh: bool,
) -> Result<CodeGitHubRepositoryRef, String> {
    match reader {
        DeliveryReader::Gh(observation) => {
            resolve_repository_cached(
                runtime,
                observation
                    .binary
                    .as_deref()
                    .expect("authenticated gh has a binary"),
                target,
                tidebreak_repo_id,
                force_refresh,
            )
            .await
        }
        DeliveryReader::Forge => {
            let credential = borrow_delivery_credential(runtime, owner, target).await?;
            resolve_repository_rest_cached(
                runtime,
                target,
                tidebreak_repo_id,
                force_refresh,
                &credential,
            )
            .await
        }
    }
}

pub(super) async fn resolve_repository_rest_cached(
    runtime: &CodeRuntime,
    target: &CodeGitHubRepositoryTarget,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
    force_refresh: bool,
    credential: &GitCredential,
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

    let api_base = runtime.forge_api_base_for(&target.host);
    let value = crate::code::forge_rest::repository(&api_base, target, credential).await?;
    let repository = repository_ref_from_value(target, None, &value);
    runtime
        .delivery_cache
        .put_repository(key, repository.clone());
    Ok(repository_with_id(repository, tidebreak_repo_id))
}

pub(super) fn repository_with_id(
    mut repository: CodeGitHubRepositoryRef,
    tidebreak_repo_id: Option<tidebreak_core::RepoId>,
) -> CodeGitHubRepositoryRef {
    repository.tidebreak_repo_id = tidebreak_repo_id;
    repository
}

pub(super) fn repository_ref_from_target(
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
