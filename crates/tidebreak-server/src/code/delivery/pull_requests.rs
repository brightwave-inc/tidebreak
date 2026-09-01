//! Pull requests: list and detail reads, guarded actions, stacks, comments, and stored facts.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PullRequestRemotePlan {
    pub(super) state: &'static str,
    pub(super) fields: &'static str,
    pub(super) checks_loaded: bool,
    /// The one author to ask GitHub for, when the query names exactly one.
    ///
    /// `gh pr list` caps at 100 rows per repository. Filtering an unscoped
    /// page down to one author afterwards silently loses that author's older
    /// pull requests in a busy repository — which is exactly what the default
    /// "Yours" view asks for. Pushing the login into the remote read keeps the
    /// cap on the rows the reader wanted.
    pub(super) author: Option<String>,
}

impl PullRequestRemotePlan {
    pub(super) fn cache_scope(&self) -> String {
        format!(
            "{}:{}:{}",
            self.state,
            if self.checks_loaded {
                "checks"
            } else {
                "summary"
            },
            self.author.as_deref().unwrap_or("*")
        )
    }
}

/// One host pull-request observation plus identity kept off the public wire.
///
/// `stack_parent_number` stays wire-compatible. The fork-qualified head
/// repository remains available until stack resolution has selected one
/// immutable base-repository-and-number identity.
#[derive(Debug, Clone)]
pub(super) struct PullRequestObservation {
    pub(super) summary: CodeDeliveryPullRequestSummary,
    pub(super) head_repository: Option<StackRepositoryIdentity>,
    /// Host-reported stack membership (GitHub stacked pull requests), when
    /// the list read found one. Carried off the wire until the shared fact
    /// pass applies it — the host edge is the authority over branch
    /// inference there.
    pub(super) host_stack: Option<HostStackMembership>,
    /// True when this row came from a host list or view this request.
    /// Stored facts folded onto the page are not host observations: the
    /// persist pass must not treat them as a fresh confirm.
    pub(super) from_host: bool,
}

impl PullRequestObservation {
    pub(super) fn pull_request_identity(&self) -> StackPullRequestIdentity {
        StackPullRequestIdentity {
            base_repository: stack_repository_identity(&self.summary.repository),
            number: self.summary.number,
        }
    }

    pub(super) fn stack_parent_candidate(&self) -> StackParentCandidate {
        StackParentCandidate {
            pull_request: self.pull_request_identity(),
            open: self.summary.state == "open",
            head_repository: self.head_repository.clone(),
            head_branch: (!self.summary.head_branch.is_empty())
                .then(|| self.summary.head_branch.clone()),
        }
    }
}

/// Host-reported membership in one stack (GitHub stacked pull requests).
#[derive(Debug, Clone)]
pub(super) struct HostStackMembership {
    pub(super) stack_number: u64,
    /// Total layers in the stack, bottom to top, including merged ones.
    pub(super) stack_size: u64,
    /// The nearest open member below this one in stack order; `None` when
    /// this pull request is the bottom layer or everything below merged.
    pub(super) parent_number: Option<u64>,
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
    let access = delivery_access(runtime, owner, false).await;
    let capability = access.capability.clone();
    let repositories = dedupe_numbered_targets(repositories)?;
    let Some(reader) = access.reader.clone() else {
        return Ok(CodeDeliveryPullRequestsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: Vec::new(),
            fetched_at: Utc::now(),
        });
    };

    let workspaces = Arc::new(workspace_index(runtime, owner, false).await?);
    let resolved = stream::iter(repositories)
        .map(|(target, numbers)| {
            let reader = reader.clone();
            async move {
                let api = delivery_api(runtime, owner, &reader, &target)
                    .await
                    .map_err(|message| (target.clone(), message))?;
                let repository = resolve_repository_for_api(runtime, &api, &target, None, false)
                    .await
                    .map_err(|message| (target.clone(), message))?;
                Ok((target, Arc::new(api), repository, numbers))
            }
        })
        .buffer_unordered(DELIVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut reads = Vec::new();
    let mut apis: HashMap<String, Arc<DeliveryApi>> = HashMap::new();
    let mut errors = Vec::new();
    for result in resolved {
        match result {
            Ok((target, api, repository, numbers)) => {
                apis.insert(repository_key(&target), Arc::clone(&api));
                reads.extend(
                    numbers.into_iter().map(|number| {
                        (target.clone(), Arc::clone(&api), repository.clone(), number)
                    }),
                );
            }
            Err((target, message)) => errors.push(source_error(Some(target), message)),
        }
    }

    let results = stream::iter(reads)
        .map(|(target, api, repository, number)| {
            let workspaces = Arc::clone(&workspaces);
            async move {
                with_transient_retry(|| {
                    fetch_pull_request(&api, &target, &repository, number, &workspaces)
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
            .summary
            .updated_at
            .cmp(&left.summary.updated_at)
            .then_with(|| left.summary.id.cmp(&right.summary.id))
    });
    // The trigger sweep reads through here, and its stacked-child suppression
    // keys on `stack_parent_number` (decision 77) — so this path persists and
    // annotates the same way the list read does. Host stacks join first so
    // the shared pass sees the same host edges a list read would.
    attach_host_stacks(&apis, &mut items).await;
    let workspaces_gaining_links =
        persist_and_augment_pull_request_facts(runtime, owner, &workspaces, &mut items).await;
    for workspace_id in workspaces_gaining_links {
        crate::code::attention::emit_workspace_digests(
            &runtime.db,
            &runtime.bus,
            owner,
            workspace_id,
        )
        .await;
    }
    let items = items.into_iter().map(|item| item.summary).collect();
    Ok(CodeDeliveryPullRequestsPage {
        capability,
        items,
        next_cursor: None,
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
    let access = delivery_access(runtime, owner, force_refresh).await;
    let capability = access.capability.clone();
    let Some(reader) = access.reader.clone() else {
        return Ok(CodeDeliveryPullRequestsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: vec![access.source_error()],
            fetched_at: Utc::now(),
        });
    };

    let remote_plan = pull_request_remote_plan(&query);
    let cache_key = aggregate_cache_key(
        owner,
        &format!("prs:{}:{}", reader.cache_scope(), remote_plan.cache_scope()),
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
            let workspace_index = workspace_index(runtime, owner, force_refresh).await?;
            let remote_plan = &remote_plan;
            let results = stream::iter(targets.clone())
                .map(|target| {
                    let reader = reader.clone();
                    let workspace_index = workspace_index.clone();
                    async move {
                        fetch_pull_requests(
                            runtime,
                            owner,
                            &reader,
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
                    .summary
                    .updated_at
                    .cmp(&left.summary.updated_at)
                    .then_with(|| left.summary.id.cmp(&right.summary.id))
            });
            // Attributed facts that fell off the host page (cap, filter, or
            // a later empty list) still belong on the aggregate.
            fold_stored_pull_request_facts(runtime, owner, &targets, &mut items).await;
            items.sort_by(|left, right| {
                right
                    .summary
                    .updated_at
                    .cmp(&left.summary.updated_at)
                    .then_with(|| left.summary.id.cmp(&right.summary.id))
            });
            let workspaces_gaining_links = persist_and_augment_pull_request_facts(
                runtime,
                owner,
                &workspace_index,
                &mut items,
            )
            .await;
            let items = items
                .into_iter()
                .map(|item| item.summary)
                .collect::<Vec<_>>();
            runtime.delivery_cache.put_pull_requests(
                cache_key.clone(),
                items.clone(),
                errors.clone(),
            );
            for workspace_id in workspaces_gaining_links {
                crate::code::attention::emit_workspace_digests(
                    &runtime.db,
                    &runtime.bus,
                    owner,
                    workspace_id,
                )
                .await;
            }
            CachedAggregate {
                fetched_at: Instant::now(),
                items,
                errors,
            }
        }
    };
    pull_request_page(capability, aggregate, &query)
}

pub(super) fn pull_request_page(
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
    let access = delivery_access(runtime, owner, false).await;
    let reader = access.require_reader()?;
    let api = delivery_api(runtime, owner, &reader, &target.repository)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let repository = resolve_repository_for_api(runtime, &api, &target.repository, None, false)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let workspace_index = workspace_index(runtime, owner, false).await?;
    let mut observation = fetch_pull_request(
        &api,
        &target.repository,
        &repository,
        target.number,
        &workspace_index,
    )
    .await
    .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let minted = persist_and_augment_pull_request_facts(
        runtime,
        owner,
        &workspace_index,
        std::slice::from_mut(&mut observation),
    )
    .await;
    for workspace_id in minted {
        crate::code::attention::emit_workspace_digests(
            &runtime.db,
            &runtime.bus,
            owner,
            workspace_id,
        )
        .await;
    }
    let mut summary = observation.summary;

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
    let stacks_endpoint = api_endpoint(
        &target.repository,
        &format!("stacks?pull_request={}&per_page=100", target.number),
    );
    let (pull, issue_comments, reviews, inline_comments, changed, stacks) = tokio::join!(
        api.get(&pull_endpoint),
        api.get(&issue_comments_endpoint),
        api.get(&reviews_endpoint),
        api.get(&inline_endpoint),
        api.get(&files_endpoint),
        api.get(&stacks_endpoint),
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

    // Stacks enrich the drawer but never gate it: a failed read (or a host
    // without stacked pull requests) leaves the chain absent and adds no
    // error entry. The host edge also restates the summary's stack fields,
    // with the same authority the list read gives it.
    if let Err(message) = &stacks {
        tracing::debug!("the stacks read failed for the detail drawer: {message}");
    }
    let stack = parse_stack_detail(stacks.as_ref().map_err(String::as_str), target.number).map(
        |(members, membership)| {
            summary.stack_number = Some(membership.stack_number);
            summary.stack_size = Some(membership.stack_size);
            summary.stack_parent_number = membership.parent_number;
            members
        },
    );

    let open = summary.state == "open";
    Ok(CodeDeliveryPullRequestDetail {
        can_mark_ready: open && summary.draft && api.can_mark_pull_request_ready(),
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
        stack,
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
pub(super) fn parse_pull_request_files(value: &Value) -> Vec<CodeDeliveryPullRequestFile> {
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

pub(super) fn pull_request_files_truncated(returned: usize, changed_files: u64) -> bool {
    returned > MAX_DETAIL_FILES || (returned as u64) < changed_files
}

pub(super) fn delivery_action_result(message: String) -> CodeDeliveryActionResult {
    CodeDeliveryActionResult {
        success: true,
        message,
        rerun_outcomes: Vec::new(),
    }
}

pub(crate) async fn act_on_pull_request(
    runtime: &Arc<CodeRuntime>,
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
    let target = body.target;
    match &body.action {
        CodeDeliveryPullRequestAction::Merge {
            auto: true,
            admin: true,
            ..
        } => {
            return Err(ServerError::bad_request(
                "an admin merge is immediate; it cannot arm auto-merge",
            ));
        }
        CodeDeliveryPullRequestAction::RerunFailed { workflow_run_ids }
            if workflow_run_ids.is_empty() =>
        {
            return Err(ServerError::bad_request(
                "at least one workflow run id is required",
            ));
        }
        CodeDeliveryPullRequestAction::Comment { body } if body.trim().is_empty() => {
            return Err(ServerError::bad_request("a comment needs a body"));
        }
        CodeDeliveryPullRequestAction::Comment { body }
            if body.trim().len() > MAX_COMMENT_BYTES =>
        {
            return Err(ServerError::bad_request(format!(
                "a comment may be at most {MAX_COMMENT_BYTES} bytes"
            )));
        }
        _ => {}
    }
    let access = delivery_access(runtime, owner, false).await;
    let reader = access.require_reader()?;
    if matches!(&reader, DeliveryReader::Forge) {
        match &body.action {
            CodeDeliveryPullRequestAction::MarkReady => {
                return Err(ServerError::conflict_kind(
                    "git_forge_mark_ready_unsupported",
                    "This hosted machine cannot mark a draft pull request ready because GitHub's pinned REST API does not expose that transition. Open the pull request on GitHub to mark it ready.",
                ));
            }
            CodeDeliveryPullRequestAction::Merge { admin: true, .. } => {
                return Err(ServerError::conflict_kind(
                    "git_forge_admin_merge_unsupported",
                    "This hosted machine cannot request an admin branch-protection bypass through GitHub's stable REST API. Open the pull request on GitHub to merge with admin privileges.",
                ));
            }
            _ => {}
        }
    }
    let api = delivery_action_api(runtime, owner, &reader, &target.repository).await?;
    // The canonical URL of the pull request being acted on: the key the
    // workspace-side digest refresh matches on (decision 66).
    let pull_request_url = format!(
        "https://{}/{}/{}/pull/{}",
        target.repository.host, target.repository.owner, target.repository.name, target.number
    );
    match body.action {
        CodeDeliveryPullRequestAction::MarkReady => {
            api.mark_pull_request_ready(&target.repository, target.number)
                .await?;
            runtime.delivery_cache.invalidate();
            runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
            Ok(delivery_action_result(format!(
                "Pull request #{} is ready for review",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Merge {
            method,
            auto,
            admin,
            expected_head_sha,
        } => {
            api.merge_pull_request(
                &target.repository,
                target.number,
                method,
                auto,
                admin,
                &expected_head_sha,
            )
            .await?;
            runtime.delivery_cache.invalidate();
            runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
            Ok(delivery_action_result(if auto {
                format!("Auto-merge enabled for pull request #{}", target.number)
            } else if admin {
                format!(
                    "Pull request #{} merged, bypassing branch protection",
                    target.number
                )
            } else {
                format!("Pull request #{} merged", target.number)
            }))
        }
        CodeDeliveryPullRequestAction::CreateStack { numbers } => {
            let mut unique = HashSet::new();
            if numbers.len() < 2
                || numbers.iter().any(|number| !unique.insert(*number))
                || !numbers.contains(&target.number)
            {
                return Err(ServerError::bad_request(
                    "a stack needs at least two distinct pull requests, including this one",
                ));
            }
            let chain = numbers;
            api.create_stack(&target.repository, &chain).await?;
            runtime.delivery_cache.invalidate();
            runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
            Ok(delivery_action_result(format!(
                "Registered a stack of {} pull requests on GitHub",
                chain.len()
            )))
        }
        CodeDeliveryPullRequestAction::RerunFailed { workflow_run_ids } => {
            let unique = workflow_run_ids.into_iter().collect::<HashSet<_>>();
            let results = stream::iter(unique)
                .map(|run_id| {
                    let api = &api;
                    let repository = target.repository.clone();
                    async move { (run_id, api.rerun_failed_jobs(&repository, run_id).await) }
                })
                .buffer_unordered(DELIVERY_CONCURRENCY)
                .collect::<Vec<_>>()
                .await;
            let any_success = results.iter().any(|(_, result)| result.is_ok());
            if any_success {
                runtime.delivery_cache.invalidate();
                runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
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
                            error: Some(error.message().to_owned()),
                        }
                    }
                })
                .collect();
            Ok(rerun_action_result(outcomes))
        }
        CodeDeliveryPullRequestAction::Close => {
            api.update_pull_request_state(&target.repository, target.number, "closed")
                .await?;
            runtime.delivery_cache.invalidate();
            runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
            Ok(delivery_action_result(format!(
                "Pull request #{} closed",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Reopen => {
            api.update_pull_request_state(&target.repository, target.number, "open")
                .await?;
            runtime.delivery_cache.invalidate();
            runtime.refresh_workspaces_for_pull_request(owner, &pull_request_url);
            Ok(delivery_action_result(format!(
                "Pull request #{} reopened",
                target.number
            )))
        }
        CodeDeliveryPullRequestAction::Comment { body } => {
            let body = body.trim();
            api.comment_on_pull_request(&target.repository, target.number, body)
                .await?;
            runtime.delivery_cache.invalidate();
            Ok(delivery_action_result(format!(
                "Comment posted on pull request #{}",
                target.number
            )))
        }
    }
}

/// Read one repository's host stacks over whichever transport the reader
/// selected.
///
/// Stacks are best-effort enrichment (GitHub stacked pull requests): a host
/// without the feature — GHES, or a repository the rollout has not reached —
/// answers 404, and that must never fail an otherwise good pull-request
/// list. A failure logs at debug and the stack fields stay absent.
pub(super) async fn fetch_stacks(
    api: &DeliveryApi,
    target: &CodeGitHubRepositoryTarget,
) -> Option<Value> {
    let endpoint = api_endpoint(target, "stacks?per_page=100");
    match with_transient_retry(|| api.get(&endpoint)).await {
        Ok(value) => Some(value),
        Err(message) => {
            tracing::debug!(
                repository = %repository_key(target),
                "the stacks read failed; stack fields stay absent: {message}"
            );
            None
        }
    }
}

/// Attach host stack membership to exact-number reads: one stacks read per
/// distinct repository, through the transports that path already borrowed —
/// no new credential borrows.
///
/// Best-effort like the list read — a repository whose stacks cannot be read
/// keeps its items as they came, and branch inference stays the fallback.
pub(super) async fn attach_host_stacks(
    apis: &HashMap<String, Arc<DeliveryApi>>,
    items: &mut [PullRequestObservation],
) {
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        groups
            .entry(repository_key_ref(&item.summary.repository))
            .or_default()
            .push(index);
    }
    for (key, indices) in groups {
        let Some(api) = apis.get(&key) else {
            continue;
        };
        let repository = &items[indices[0]].summary.repository;
        let target = CodeGitHubRepositoryTarget {
            host: repository.host.clone(),
            owner: repository.owner.clone(),
            name: repository.name.clone(),
        };
        let Some(payload) = fetch_stacks(api, &target).await else {
            continue;
        };
        let memberships = parse_stack_memberships(&payload);
        for index in indices {
            let item = &mut items[index];
            item.host_stack = memberships.get(&item.summary.number).cloned();
        }
    }
}

pub(super) async fn fetch_pull_requests(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    reader: &DeliveryReader,
    target: &CodeGitHubRepositoryTarget,
    workspaces: &[WorkspaceIndexEntry],
    plan: &PullRequestRemotePlan,
    force_refresh: bool,
) -> Result<Vec<PullRequestObservation>, String> {
    // One borrowed transport per repository, shared by the pull-request list
    // and the stacks enrichment — the credential lender counts one borrow per
    // repository operation, and a second one here would double it.
    let api = delivery_api(runtime, owner, reader, target).await?;
    let (repository, values, stacks) = match &api {
        DeliveryApi::Gh { observation, .. } => {
            let binary = observation
                .binary
                .as_deref()
                .expect("authenticated gh has a binary");
            let repository =
                resolve_repository_cached(runtime, binary, target, None, force_refresh).await?;
            let cli_repository = gh::cli_repository(&target.host, &target.owner, &target.name);
            let limit = MAX_REMOTE_ITEMS_PER_REPO.to_string();
            let mut args = vec![
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
            if let Some(author) = plan.author.as_deref() {
                args.push("--author");
                args.push(author);
            }
            let (raw, stacks) = tokio::join!(
                with_transient_retry(|| {
                    gh::run_gh(Path::new("."), binary, &args, GH_READ_TIMEOUT)
                }),
                fetch_stacks(&api, target),
            );
            let raw = raw?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|error| format!("could not parse pull requests: {error}"))?;
            (
                repository,
                value.as_array().cloned().unwrap_or_default(),
                stacks,
            )
        }
        DeliveryApi::Rest {
            api_base,
            credential,
        } => {
            let repository =
                resolve_repository_rest_cached(runtime, target, None, force_refresh, credential)
                    .await?;
            let (values, stacks) = tokio::join!(
                with_transient_retry(|| {
                    crate::code::forge_rest::delivery_pull_requests(
                        api_base,
                        target,
                        credential,
                        plan.state,
                        plan.checks_loaded,
                    )
                }),
                fetch_stacks(&api, target),
            );
            let values = values?;
            (repository, values, stacks)
        }
    };
    let mut values = values;
    overlay_issue_comment_counts(&api, target, plan.state, &mut values).await;
    // Host stacks ride along as observations; the shared fact pass applies
    // them so host edges and branch inference meet in one place.
    let memberships = stacks
        .as_ref()
        .map(parse_stack_memberships)
        .unwrap_or_default();
    attach_merge_queue_membership(&api, target, &mut values).await;
    Ok(values
        .iter()
        .filter_map(|value| parse_pull_request(&repository, value, workspaces))
        .map(|mut observation| {
            observation.host_stack = memberships.get(&observation.summary.number).cloned();
            observation
        })
        .collect())
}

/// Fold durable facts onto the live page so the delivery aggregate is a
/// projection of `code_pull_request` plus this request's host rows.
///
/// A fact whose number is already on the page stays the host observation —
/// GitHub is still authoritative for volatile fields. A fact the host did
/// not return becomes a stored row with empty heuristic links; attribution
/// is applied in [`persist_and_augment_pull_request_facts`].
pub(super) async fn fold_stored_pull_request_facts(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    targets: &[CodeGitHubRepositoryTarget],
    items: &mut Vec<PullRequestObservation>,
) {
    for target in targets {
        let facts = match list_pull_request_facts_for_repo(
            &runtime.db,
            owner,
            &target.host,
            &target.owner,
            &target.name,
        )
        .await
        {
            Ok(facts) => facts,
            Err(err) => {
                tracing::debug!("fact fold failed for a delivery page: {err}");
                continue;
            }
        };
        if facts.is_empty() {
            continue;
        }
        let present: HashSet<u64> = items
            .iter()
            .filter(|item| repository_key_ref(&item.summary.repository) == repository_key(target))
            .map(|item| item.summary.number)
            .collect();
        let repository = items
            .iter()
            .find(|item| repository_key_ref(&item.summary.repository) == repository_key(target))
            .map(|item| item.summary.repository.clone())
            .unwrap_or_else(|| repository_ref_from_target(target, None));
        for fact in facts {
            if present.contains(&fact.number) {
                continue;
            }
            items.push(observation_from_fact(&fact, repository.clone()));
        }
    }
}

pub(super) fn observation_from_fact(
    fact: &CodePullRequestFact,
    repository: CodeGitHubRepositoryRef,
) -> PullRequestObservation {
    let live = fact.live.as_ref();
    let checks: Vec<CodeDeliveryCheck> = live
        .and_then(|live| live.checks.as_ref())
        .map(|checks| {
            checks
                .iter()
                .map(|check| CodeDeliveryCheck {
                    name: check.name.clone(),
                    bucket: check.bucket,
                    detail: check.detail.clone(),
                    url: check.url.clone(),
                    workflow_run_id: None,
                })
                .collect()
        })
        .unwrap_or_default();
    let checks_loaded = live.and_then(|live| live.checks.as_ref()).is_some();
    let review_decision = live.and_then(|live| live.review_decision.clone());
    let mergeable = live.and_then(|live| live.mergeable.clone());
    let merge_state_status = live.and_then(|live| live.merge_state_status.clone());
    let auto_merge_enabled = live
        .and_then(|live| live.auto_merge_enabled)
        .unwrap_or(false);
    let in_merge_queue = live.and_then(|live| live.in_merge_queue);
    let state = fact.state.as_str().to_owned();
    let attention_reasons = pull_request_attention(
        &state,
        fact.draft,
        review_decision.as_deref(),
        mergeable.as_deref(),
        merge_state_status.as_deref(),
        &checks,
    );
    let ready_to_merge = state == "open"
        && !fact.draft
        && !auto_merge_enabled
        && in_merge_queue != Some(true)
        && checks_loaded
        && attention_reasons.is_empty()
        && !checks
            .iter()
            .any(|check| check.bucket == PullRequestCheckBucket::Pending)
        && !matches!(review_decision.as_deref(), Some("review_required"));
    PullRequestObservation {
        summary: CodeDeliveryPullRequestSummary {
            id: format!("{}#{}", repository_key_ref(&repository), fact.number),
            repository,
            number: fact.number,
            url: fact.url.clone(),
            title: fact.title.clone(),
            state,
            draft: fact.draft,
            author: fact.author.clone(),
            author_avatar_url: None,
            head_branch: fact.head_branch.clone(),
            base_branch: fact.base_branch.clone(),
            head_sha: fact.head_sha.clone(),
            review_decision,
            mergeable,
            merge_state_status,
            auto_merge_enabled,
            in_merge_queue,
            comment_count: None,
            checks,
            attention_reasons,
            ready_to_merge,
            workspace_links: Vec::new(),
            stack_parent_number: None,
            stack_number: None,
            stack_size: None,
            unregistered_stack_numbers: None,
            labels: Vec::new(),
            created_at: fact.created_at,
            updated_at: fact.updated_at,
            merged_at: fact.merged_at,
            closed_at: fact.closed_at,
        },
        head_repository: None,
        host_stack: None,
        from_host: false,
    }
}

pub(super) fn pull_request_remote_plan(
    query: &CodeDeliveryPullRequestQuery,
) -> PullRequestRemotePlan {
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
    // Only a single author pushes down: `gh pr list` takes one `--author`,
    // while the query's list is a union. Several authors still page the
    // unscoped read and match locally.
    let author = match query.authors.as_slice() {
        [only] if !only.trim().is_empty() => Some(only.trim().to_owned()),
        _ => None,
    };
    PullRequestRemotePlan {
        state,
        fields: if checks_loaded {
            PR_LIST_FIELDS_WITH_CHECKS
        } else {
            PR_LIST_FIELDS
        },
        checks_loaded,
        author,
    }
}

pub(super) async fn fetch_pull_request(
    api: &DeliveryApi,
    target: &CodeGitHubRepositoryTarget,
    repository: &CodeGitHubRepositoryRef,
    number: u64,
    workspaces: &[WorkspaceIndexEntry],
) -> Result<PullRequestObservation, String> {
    let mut value = api.pull_request(target, repository, number).await?;
    attach_merge_queue_membership(api, target, std::slice::from_mut(&mut value)).await;
    parse_pull_request(repository, &value, workspaces)
        .ok_or_else(|| "GitHub returned an incomplete pull request".into())
}

/// REST `mergeable_state` never reports `queued`. Membership comes from the
/// issue timeline both readers already share.
pub(super) async fn attach_merge_queue_membership(
    api: &DeliveryApi,
    target: &CodeGitHubRepositoryTarget,
    values: &mut [Value],
) {
    let jobs = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            Some((
                index,
                u64_field(value, "number")?,
                text_field(value, "state").is_some_and(|state| state.eq_ignore_ascii_case("open")),
            ))
        })
        .collect::<Vec<_>>();
    let memberships = stream::iter(jobs)
        .map(|(index, number, open)| async move {
            let queued = if open {
                api.merge_queue_membership(target, number).await
            } else {
                Some(false)
            };
            (index, queued)
        })
        .buffered(DELIVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for (index, queued) in memberships {
        let Some(queued) = queued else {
            continue;
        };
        if let Some(object) = values[index].as_object_mut() {
            object.insert("inMergeQueue".to_owned(), Value::Bool(queued));
        }
    }
}

pub(super) fn parse_pull_request(
    repository: &CodeGitHubRepositoryRef,
    value: &Value,
    workspaces: &[WorkspaceIndexEntry],
) -> Option<PullRequestObservation> {
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
    let auto_merge_enabled = value
        .get("autoMergeRequest")
        .is_some_and(|request| !request.is_null());
    let in_merge_queue = match bool_field(value, "inMergeQueue") {
        Some(queued) => Some(queued),
        None => (merge_state_status.as_deref() == Some("queued")).then_some(true),
    };
    let comment_count = parse_comment_count(value);
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
        && !auto_merge_enabled
        && in_merge_queue != Some(true)
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
    Some(PullRequestObservation {
        summary: CodeDeliveryPullRequestSummary {
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
            auto_merge_enabled,
            in_merge_queue,
            comment_count,
            checks,
            attention_reasons,
            ready_to_merge,
            workspace_links,
            stack_parent_number: None,
            stack_number: None,
            stack_size: None,
            unregistered_stack_numbers: None,
            labels,
            created_at: datetime_field(value, "createdAt").unwrap_or_else(Utc::now),
            updated_at: datetime_field(value, "updatedAt").unwrap_or_else(Utc::now),
            merged_at,
            closed_at,
        },
        head_repository: parse_head_repository(repository, value),
        host_stack: None,
        from_host: true,
    })
}

pub(super) fn parse_head_repository(
    base_repository: &CodeGitHubRepositoryRef,
    value: &Value,
) -> Option<StackRepositoryIdentity> {
    let repository = value.get("headRepository")?;
    if repository.is_null() {
        return None;
    }
    let name_with_owner = repository.get("nameWithOwner").and_then(Value::as_str);
    let from_name_with_owner = name_with_owner.and_then(|name_with_owner| {
        let (owner, name) = name_with_owner.split_once('/')?;
        if name.contains('/') {
            return None;
        }
        StackRepositoryIdentity::new(&base_repository.host, owner, name)
    });
    if name_with_owner.is_some() && from_name_with_owner.is_none() {
        return None;
    }
    let owner = value
        .get("headRepositoryOwner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str);
    let name = repository.get("name").and_then(Value::as_str);
    let from_parts = owner
        .zip(name)
        .and_then(|(owner, name)| StackRepositoryIdentity::new(&base_repository.host, owner, name));
    let identity = match (from_name_with_owner, from_parts) {
        (Some(name_with_owner), Some(parts)) if name_with_owner == parts => parts,
        (Some(_), Some(_)) => return None,
        (Some(identity), None) | (None, Some(identity)) => identity,
        (None, None) => return None,
    };
    if owner.is_some_and(|owner| {
        StackRepositoryIdentity::new(&base_repository.host, owner, &identity.name)
            .is_none_or(|candidate| candidate.owner != identity.owner)
    }) {
        return None;
    }
    if name.is_some_and(|name| {
        StackRepositoryIdentity::new(&base_repository.host, &identity.owner, name)
            .is_none_or(|candidate| candidate.name != identity.name)
    }) {
        return None;
    }
    Some(identity)
}

/// One layer of a host-reported stack, in the payload's bottom-to-top order.
pub(super) fn parse_stack_member(value: &Value) -> Option<CodeDeliveryStackMember> {
    Some(CodeDeliveryStackMember {
        number: u64_field(value, "number")?,
        state: text_field(value, "state")?.to_ascii_lowercase(),
        draft: bool_field(value, "draft").unwrap_or(false),
        merged_at: text_field(value, "merged_at"),
        head_branch: value
            .pointer("/head/ref")
            .and_then(Value::as_str)
            .map(str::to_owned)?,
        head_sha: value
            .pointer("/head/sha")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// One host stack from `GET /repos/{owner}/{repo}/stacks`: its number, its
/// layer count, and its parseable layers in payload order (bottom to top).
#[derive(Debug)]
pub(super) struct HostStack {
    pub(super) number: u64,
    /// Raw layer count from the payload: a malformed layer still counts
    /// toward the stack size the host reports.
    pub(super) size: u64,
    pub(super) members: Vec<CodeDeliveryStackMember>,
}

pub(super) fn parse_host_stack(stack: &Value) -> Option<HostStack> {
    let number = u64_field(stack, "number")?;
    let layers = stack.get("pull_requests")?.as_array()?;
    Some(HostStack {
        number,
        size: layers.len() as u64,
        members: layers.iter().filter_map(parse_stack_member).collect(),
    })
}

/// The parent within one stack: the nearest open member below `position`.
///
/// Merged layers do not parent anything — a merged base is part of the
/// target branch already, and the child's live dependency is the nearest
/// layer still waiting to merge.
pub(super) fn stack_parent_below(
    members: &[CodeDeliveryStackMember],
    position: usize,
) -> Option<u64> {
    members[..position]
        .iter()
        .rev()
        .find(|member| member.state == "open")
        .map(|member| member.number)
}

/// Host stack memberships keyed by pull-request number.
///
/// A payload that is not an array of the expected shape yields an empty
/// map, which every caller treats as "no host stacks" — never as an error.
pub(super) fn parse_stack_memberships(payload: &Value) -> HashMap<u64, HostStackMembership> {
    let mut memberships = HashMap::new();
    for stack in payload.as_array().into_iter().flatten() {
        let Some(stack) = parse_host_stack(stack) else {
            continue;
        };
        for (position, member) in stack.members.iter().enumerate() {
            memberships.insert(
                member.number,
                HostStackMembership {
                    stack_number: stack.number,
                    stack_size: stack.size,
                    parent_number: stack_parent_below(&stack.members, position),
                },
            );
        }
    }
    memberships
}

/// The stack chain for one pull request, from `stacks?pull_request={n}`.
///
/// The first stack naming the pull request is the chain, in payload order
/// (bottom to top). `None` when the read failed, returned no stack, or no
/// stack names the pull request — stacks are best-effort enrichment, never
/// a load-bearing section of the drawer.
pub(super) fn parse_stack_detail<'a>(
    payload: Result<&'a Value, &'a str>,
    number: u64,
) -> Option<(Vec<CodeDeliveryStackMember>, HostStackMembership)> {
    let payload = payload.ok()?;
    for stack in payload.as_array().into_iter().flatten() {
        let Some(parsed) = parse_host_stack(stack) else {
            continue;
        };
        let Some(position) = parsed
            .members
            .iter()
            .position(|member| member.number == number)
        else {
            continue;
        };
        return Some((
            parsed.members.clone(),
            HostStackMembership {
                stack_number: parsed.number,
                stack_size: parsed.size,
                parent_number: stack_parent_below(&parsed.members, position),
            },
        ));
    }
    None
}

pub(super) fn parse_check(value: &Value) -> Option<CodeDeliveryCheck> {
    let name = text_field(value, "name")
        .or_else(|| text_field(value, "context"))
        .or_else(|| text_field(value, "workflowName"))?;
    let token = normalized_optional(value, "conclusion")
        .or_else(|| normalized_optional(value, "state"))
        .or_else(|| normalized_optional(value, "status"))
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

pub(super) fn workflow_run_id_from_url(url: &str) -> Option<u64> {
    let (_, tail) = url.split_once("/actions/runs/")?;
    tail.split('/').next()?.parse().ok()
}

/// Why an open pull request belongs in the default Needs attention view.
///
/// Conflicts come first because a conflicted tree blocks every other fix:
/// until the head rebases cleanly, requested changes, failing checks, and
/// a behind base cannot even be evaluated on the final diff.
pub(super) fn pull_request_attention(
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
    if mergeable == Some("conflicting") || merge_state_status == Some("dirty") {
        reasons.push(CodeDeliveryPrAttentionReason::Conflicts);
    }
    if review_decision == Some("changes_requested") {
        reasons.push(CodeDeliveryPrAttentionReason::ChangesRequested);
    }
    if checks
        .iter()
        .any(|check| check.bucket == PullRequestCheckBucket::Fail)
    {
        reasons.push(CodeDeliveryPrAttentionReason::ChecksFailed);
    }
    if merge_state_status == Some("behind") {
        reasons.push(CodeDeliveryPrAttentionReason::Behind);
    }
    // GitHub reports the merge state as blocked while required checks run
    // (decision 66): checks in flight are ordinary progress, not attention.
    if merge_state_status == Some("blocked")
        && !checks
            .iter()
            .any(|check| check.bucket == PullRequestCheckBucket::Pending)
    {
        reasons.push(CodeDeliveryPrAttentionReason::Blocked);
    }
    reasons
}

pub(super) fn links_for_pr(
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

pub(super) fn workspace_links(
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
                    relation: None,
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

/// Project one delivery summary into the digest vocabulary (decision 66):
/// the same shape a workspace read stores, so the live tier and its
/// write-through take one path no matter who observed the pull request.
pub(crate) fn digest_from_summary(item: &CodeDeliveryPullRequestSummary) -> PullRequestDigest {
    PullRequestDigest {
        number: item.number,
        url: Some(item.url.clone()),
        state: item.state.clone(),
        title: Some(item.title.clone()),
        checks_summary: None,
        check_counts: None,
        checks: Some(
            item.checks
                .iter()
                .map(|check| PullRequestCheck {
                    name: check.name.clone(),
                    bucket: check.bucket,
                    detail: check.detail.clone(),
                    url: check.url.clone(),
                })
                .collect(),
        ),
        draft: Some(item.draft),
        // `state` alone cannot separate merged from closed on every host
        // response, which is why the summary carries `merged_at`.
        merged: Some(item.merged_at.is_some()),
        review_decision: item.review_decision.clone(),
        mergeable: item.mergeable.clone(),
        merge_state_status: item.merge_state_status.clone(),
        head_branch: Some(item.head_branch.clone()),
        base_branch: Some(item.base_branch.clone()),
        head_sha: item.head_sha.clone(),
        auto_merge_enabled: Some(item.auto_merge_enabled),
        in_merge_queue: item.in_merge_queue,
    }
}

/// Persist durable facts for the page's tracked pull requests and fold the
/// stored attribution back into every item's workspace links (decision 77).
///
/// Tracked means exact-linked to a workspace (the index's number or head-SHA
/// tiers) or already holding a fact row — a pull request nobody here worked
/// on stays a live-only observation. The branch-name tier never mints.
/// Returns the workspaces that gained an attribution row, so the caller can
/// restate their digests. Best-effort throughout: a store failure degrades
/// to the live heuristic links.
pub(super) async fn persist_and_augment_pull_request_facts(
    runtime: &CodeRuntime,
    owner: &OwnerId,
    workspaces: &[WorkspaceIndexEntry],
    items: &mut [PullRequestObservation],
) -> Vec<WorkspaceId> {
    let db = &runtime.db;
    let mut minted = Vec::new();
    let now = Utc::now();
    let mut stack_candidates: HashMap<StackPullRequestIdentity, StackParentCandidate> = items
        .iter()
        .map(|item| {
            let candidate = item.stack_parent_candidate();
            (candidate.pull_request.clone(), candidate)
        })
        .collect();

    // One fact read per repository identity on the page.
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        groups
            .entry(repository_key_ref(&item.summary.repository))
            .or_default()
            .push(index);
    }
    let mut fact_ids: HashMap<usize, CodePullRequestId> = HashMap::new();
    for indices in groups.values() {
        let repository = &items[indices[0]].summary.repository;
        let mut repo_facts = match list_pull_request_facts_for_repo(
            db,
            owner,
            &repository.host,
            &repository.owner,
            &repository.name,
        )
        .await
        {
            Ok(facts) => facts,
            Err(err) => {
                tracing::debug!("fact read failed for a delivery page: {err}");
                continue;
            }
        };
        let base_repository = stack_repository_identity(repository);
        for fact in &repo_facts {
            let pull_request = StackPullRequestIdentity {
                base_repository: base_repository.clone(),
                number: fact.number,
            };
            stack_candidates
                .entry(pull_request.clone())
                .or_insert_with(|| StackParentCandidate {
                    pull_request,
                    open: fact.state == CodePullRequestState::Open,
                    // Durable facts predate fork-qualified identity. They can
                    // prove that a same-named candidate exists, but they must
                    // not select one fork by branch name alone.
                    head_repository: None,
                    head_branch: (!fact.head_branch.is_empty()).then(|| fact.head_branch.clone()),
                });
        }
        let known: HashMap<u64, CodePullRequestId> = repo_facts
            .iter()
            .map(|fact| (fact.number, fact.id))
            .collect();
        let known_queue: HashMap<u64, bool> = repo_facts
            .iter()
            .filter_map(|fact| {
                fact.live
                    .as_ref()?
                    .in_merge_queue
                    .map(|queued| (fact.number, queued))
            })
            .collect();
        for &index in indices {
            let from_host = items[index].from_host;
            let item = &mut items[index].summary;
            if item.in_merge_queue.is_none() {
                item.in_merge_queue = known_queue.get(&item.number).copied();
            }
            if item.in_merge_queue == Some(true) {
                // Once GitHub owns the next move, stale check failures should
                // not keep the pull request in the reader's attention queue.
                item.attention_reasons.clear();
                item.ready_to_merge = false;
            }
            let exact_workspaces: Vec<WorkspaceId> = item
                .workspace_links
                .iter()
                .filter(|link| link.exact)
                .map(|link| link.workspace_id)
                .collect();
            if exact_workspaces.is_empty() && !known.contains_key(&item.number) {
                continue;
            }
            if !from_host {
                if let Some(id) = known.get(&item.number) {
                    fact_ids.insert(index, *id);
                }
                continue;
            }
            let Some(fact) = crate::code::reconcile::fact_from_summary(owner, item, now) else {
                continue;
            };
            let id = match save_pull_request_fact(db, &fact).await {
                Ok(id) => id,
                Err(err) => {
                    tracing::debug!("fact upsert failed for a delivery page: {err}");
                    continue;
                }
            };
            // The summary is a fresh host observation: write it onto the
            // row's live tier and fan real change out to every workspace
            // holding the pull request (decision 66). One list read per
            // repository is what keeps every surface fresh.
            runtime
                .record_pull_request_live_state(owner, None, &digest_from_summary(item))
                .await;
            // Keep this pass's fact set current for later durable reads.
            match repo_facts
                .iter_mut()
                .find(|known| known.number == fact.number)
            {
                Some(existing) => *existing = fact,
                None => repo_facts.push(fact),
            }
            fact_ids.insert(index, id);
            for workspace_id in exact_workspaces {
                match insert_pull_request_attribution(
                    db,
                    &CodePullRequestAttribution {
                        owner: owner.clone(),
                        pull_request_id: id,
                        workspace_id,
                        relation: CodePullRequestRelation::Contributed,
                        discovered_via: CodePullRequestDiscovery::Reconcile,
                        session_id: None,
                        parent_call_id: None,
                        created_at: now,
                    },
                )
                .await
                {
                    Ok(true) => minted.push(workspace_id),
                    Ok(false) => {}
                    Err(err) => tracing::debug!("attribution claim failed: {err}"),
                }
            }
        }
    }

    let stack_index = StackParentIndex::new(stack_candidates.into_values());
    for item in items.iter_mut() {
        let child = item.pull_request_identity();
        let head_repository = item.head_repository.clone();
        let summary = &mut item.summary;
        summary.stack_parent_number = None;
        if summary.base_branch.is_empty()
            || summary
                .repository
                .default_branch
                .as_deref()
                .is_some_and(|default| default == summary.base_branch)
        {
            continue;
        }
        let Some(edge) = StackParentEdge::new(
            stack_repository_identity(&summary.repository),
            head_repository,
            &summary.base_branch,
        ) else {
            continue;
        };
        match stack_index.resolve(&edge, Some(&child)) {
            StackParentResolution::Resolved(parent) => {
                summary.stack_parent_number = Some(parent.number);
            }
            StackParentResolution::Unresolved { reason, .. } => {
                tracing::debug!(
                    pull_request = summary.number,
                    base_branch = %summary.base_branch,
                    ?reason,
                    "stack edge stayed unresolved"
                );
            }
        }
    }

    for item in items.iter_mut() {
        // A host-reported stack names the parent from the host's own stack
        // order, and that edge is the authority: it wins over branch
        // inference, including "no parent" for a bottom layer, which clears
        // whatever inference would have guessed.
        let Some((stack_number, stack_size, parent_number)) = item
            .host_stack
            .as_ref()
            .map(|stack| (stack.stack_number, stack.stack_size, stack.parent_number))
        else {
            continue;
        };
        item.summary.stack_number = Some(stack_number);
        item.summary.stack_size = Some(stack_size);
        item.summary.stack_parent_number = parent_number;
    }

    // Detect stack-shaped chains the host has no stack for (GitHub stacked
    // pull requests): consecutive inferred edges among this page's open pull
    // requests, gapless from a root to a single top, with no member already
    // host-registered. Every member has to be on the page — inference only
    // resolves edges between page items, and a chain with a hole in it would
    // be refused by the create call anyway. A fork (two pull requests on the
    // same base branch) is not a stack and offers nothing.
    let mut page_items: HashMap<StackPullRequestIdentity, usize> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if item.host_stack.is_none() && item.summary.state == "open" {
            page_items.insert(item.pull_request_identity(), index);
        }
    }
    let mut children: HashMap<StackPullRequestIdentity, Option<StackPullRequestIdentity>> =
        HashMap::new();
    for (identity, &index) in &page_items {
        let Some(parent_number) = items[index].summary.stack_parent_number else {
            continue;
        };
        let parent = StackPullRequestIdentity {
            base_repository: identity.base_repository.clone(),
            number: parent_number,
        };
        if page_items.contains_key(&parent) {
            match children.entry(parent) {
                std::collections::hash_map::Entry::Occupied(mut forked) => {
                    *forked.get_mut() = None;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Some((*identity).clone()));
                }
            }
        }
    }

    // Walk each member to its root, then memoize the chain that hangs below
    // that root: a fork kills it, a chain of one is not a stack, and every
    // member reports the same bottom-to-top array.
    let mut chains: HashMap<StackPullRequestIdentity, Option<Vec<u64>>> = HashMap::new();
    let hop_limit = page_items.len() + 1;
    for (identity, &index) in &page_items {
        let mut root = (*identity).clone();
        let mut hops = 0;
        let mut rooted = true;
        while let Some(parent_number) = items[page_items[&root]].summary.stack_parent_number {
            let parent = StackPullRequestIdentity {
                base_repository: root.base_repository.clone(),
                number: parent_number,
            };
            if !page_items.contains_key(&parent) || hops > hop_limit {
                // A hole above — off-page, closed, or host-registered — or a
                // cycle: the chain cannot be verified end to end.
                rooted = false;
                break;
            }
            root = parent;
            hops += 1;
        }
        if !rooted {
            continue;
        }
        if !chains.contains_key(&root) {
            let mut chain: Vec<u64> = Vec::new();
            let mut node = root.clone();
            let mut hops = 0;
            loop {
                chain.push(node.number);
                match children.get(&node) {
                    None => break,
                    // Two pull requests on the same base branch: a fork, not
                    // a stack.
                    Some(None) => {
                        chain.clear();
                        break;
                    }
                    Some(Some(child)) => {
                        node = (*child).clone();
                        hops += 1;
                        if hops > hop_limit {
                            chain.clear();
                            break;
                        }
                    }
                }
            }
            chains.insert(root.clone(), (chain.len() >= 2).then_some(chain));
        }
        if let Some(Some(chain)) = chains.get(&root) {
            items[index].summary.unregistered_stack_numbers = Some(chain.to_vec());
        }
    }

    if fact_ids.is_empty() {
        return minted;
    }
    let ids: Vec<CodePullRequestId> = fact_ids.values().copied().collect();
    let attributions = match list_attributions_for_pull_requests(db, owner, &ids).await {
        Ok(attributions) => attributions,
        Err(err) => {
            tracing::debug!("attribution read failed for a delivery page: {err}");
            return minted;
        }
    };
    let mut by_fact: HashMap<CodePullRequestId, Vec<&CodePullRequestAttribution>> = HashMap::new();
    for attribution in &attributions {
        by_fact
            .entry(attribution.pull_request_id)
            .or_default()
            .push(attribution);
    }

    // Workspace metadata for links the live index did not produce — an
    // archived or foreign-branch workspace whose attribution outlived the
    // heuristic match.
    let mut workspace_meta: HashMap<WorkspaceId, CodeWorkspace> = workspaces
        .iter()
        .map(|entry| (entry.workspace.id, entry.workspace.clone()))
        .collect();
    for attribution in &attributions {
        if workspace_meta.contains_key(&attribution.workspace_id) {
            continue;
        }
        if let Ok(Some(workspace)) = get_workspace(db, owner, attribution.workspace_id).await {
            workspace_meta.insert(workspace.id, workspace);
        }
    }

    for (index, fact_id) in fact_ids {
        let Some(attributions) = by_fact.get(&fact_id) else {
            continue;
        };
        let item = &mut items[index].summary;
        for attribution in attributions {
            if let Some(link) = item
                .workspace_links
                .iter_mut()
                .find(|link| link.workspace_id == attribution.workspace_id)
            {
                link.exact = true;
                link.relation = Some(attribution.relation);
                continue;
            }
            let Some(workspace) = workspace_meta.get(&attribution.workspace_id) else {
                continue;
            };
            item.workspace_links.push(CodeDeliveryWorkspaceLink {
                workspace_id: workspace.id,
                repo_id: workspace.repo_id,
                title: workspace.title.clone(),
                branch_name: workspace.branch_name.clone(),
                status: workspace.status,
                exact: true,
                relation: Some(attribution.relation),
            });
        }
        // Restore the established order — status rank, exact first, newest —
        // because the notifications store routes to the first link.
        item.workspace_links.sort_by(|left, right| {
            let left_time = workspace_meta
                .get(&left.workspace_id)
                .map(|workspace| workspace.created_at);
            let right_time = workspace_meta
                .get(&right.workspace_id)
                .map(|workspace| workspace.created_at);
            workspace_status_rank(left.status)
                .cmp(&workspace_status_rank(right.status))
                .then_with(|| right.exact.cmp(&left.exact))
                .then_with(|| right_time.cmp(&left_time))
        });
    }
    minted
}

pub(super) fn pull_request_matches(
    item: &CodeDeliveryPullRequestSummary,
    query: &CodeDeliveryPullRequestQuery,
) -> bool {
    if let Some(after) = query.updated_after {
        if item.updated_at < after {
            return false;
        }
    }
    if query.attention_only
        && (item.attention_reasons.is_empty() || item.in_merge_queue == Some(true))
    {
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

pub(super) fn parse_issue_comments(value: &Value) -> Vec<PullRequestComment> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comment| parse_comment(comment, PullRequestCommentKind::Issue, "created_at"))
        .collect()
}

pub(super) fn parse_reviews(value: &Value) -> Vec<PullRequestComment> {
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

pub(super) fn parse_inline_comments(value: &Value) -> Vec<PullRequestComment> {
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

pub(super) fn parse_comment(
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

/// Issue-comment count from a list payload.
///
/// GitHub answers this three ways: a number on a REST issue, an array of
/// comment objects from `gh pr list --json comments`, or a connection with
/// `totalCount`. Null and unknown shapes stay absent so the UI does not
/// pretend the count is zero.
pub(super) fn parse_comment_count(value: &Value) -> Option<u64> {
    let comments = value.get("comments")?;
    if comments.is_null() {
        return None;
    }
    if let Some(count) = comments.as_u64() {
        return Some(count);
    }
    if let Some(items) = comments.as_array() {
        return u64::try_from(items.len()).ok();
    }
    comments
        .get("totalCount")
        .or_else(|| comments.get("total_count"))
        .and_then(Value::as_u64)
}

/// GitHub's pull-request list REST payload leaves `comments` null. The issues
/// list uses the same numbers and carries an integer count. One page mixes
/// ordinary issues in, so keep paging while listed PR numbers are still
/// missing. Failures and leftover misses stay absent.
pub(super) const ISSUE_COMMENT_PAGE_SIZE: usize = 100;

pub(super) const ISSUE_COMMENT_MAX_PAGES: u32 = 10;

pub(super) fn absorb_issue_comment_counts(
    issues: &[Value],
    needed: &mut HashSet<u64>,
    counts: &mut HashMap<u64, u64>,
) {
    for issue in issues {
        let Some(number) = issue.get("number").and_then(Value::as_u64) else {
            continue;
        };
        if !needed.contains(&number) {
            continue;
        }
        let Some(comments) = issue.get("comments").and_then(Value::as_u64) else {
            continue;
        };
        needed.remove(&number);
        counts.insert(number, comments);
    }
}

pub(super) async fn overlay_issue_comment_counts(
    api: &DeliveryApi,
    target: &CodeGitHubRepositoryTarget,
    state: &str,
    values: &mut [Value],
) {
    let mut needed = HashSet::new();
    for value in values.iter() {
        if parse_comment_count(value).is_some() {
            continue;
        }
        if let Some(number) = value.get("number").and_then(Value::as_u64) {
            needed.insert(number);
        }
    }
    if needed.is_empty() {
        return;
    }
    let state = if state == "merged" { "closed" } else { state };
    let mut counts = HashMap::new();
    for page in 1..=ISSUE_COMMENT_MAX_PAGES {
        let endpoint = format!(
            "{}?state={state}&per_page={ISSUE_COMMENT_PAGE_SIZE}&page={page}",
            api_endpoint(target, "issues")
        );
        let Ok(payload) = api.get(&endpoint).await else {
            break;
        };
        let Some(issues) = payload.as_array() else {
            break;
        };
        absorb_issue_comment_counts(issues, &mut needed, &mut counts);
        if needed.is_empty() || issues.len() < ISSUE_COMMENT_PAGE_SIZE {
            break;
        }
    }
    for value in values {
        if parse_comment_count(value).is_some() {
            continue;
        }
        let Some(number) = value.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let Some(count) = counts.get(&number) else {
            continue;
        };
        if let Some(object) = value.as_object_mut() {
            object.insert("comments".to_owned(), Value::from(*count));
        }
    }
}

pub(super) fn stack_repository_identity(
    repository: &CodeGitHubRepositoryRef,
) -> StackRepositoryIdentity {
    StackRepositoryIdentity::new(&repository.host, &repository.owner, &repository.name)
        .expect("a resolved GitHub repository has a complete identity")
}
