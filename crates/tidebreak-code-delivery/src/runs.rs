//! Workflow runs and deployments: reads, reruns, stored summaries, and parsing.

use super::*;

#[derive(Debug, Default)]
pub(super) struct FetchedRuns {
    pub(super) items: Vec<CodeDeliveryRunSummary>,
    pub(super) errors: Vec<CodeDeliverySourceError>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RunFetchOptions {
    pub(super) fetch_workflows: bool,
    pub(super) fetch_deployments: bool,
    pub(super) force_refresh: bool,
}

pub(super) fn rerun_action_result(
    mut outcomes: Vec<CodeDeliveryRerunOutcome>,
) -> CodeDeliveryActionResult {
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

pub(super) fn workflow_run_count(count: usize) -> String {
    if count == 1 {
        "one workflow run".into()
    } else {
        format!("{count} workflow runs")
    }
}

pub async fn query_runs(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    allow_unscoped_delivery: bool,
    query: CodeDeliveryRunQuery,
) -> Result<CodeDeliveryRunsPage, ServerError> {
    let force_refresh = query.refresh && query.cursor.is_none();
    let targets = dedupe_targets(query.repositories.clone())?;
    ensure_delivery_targets(runtime, owner, allow_unscoped_delivery, &targets).await?;
    let access = runtime.delivery_access(owner, force_refresh).await;
    let capability = access.capability.clone();
    let Some(reader) = access.reader.clone() else {
        return Ok(CodeDeliveryRunsPage {
            capability,
            items: Vec::new(),
            next_cursor: None,
            errors: vec![access.source_error()],
            fetched_at: Utc::now(),
        });
    };

    let (remote_scope, fetch_workflows, fetch_deployments) = run_remote_scope(&query);
    let fetch_options = RunFetchOptions {
        fetch_workflows,
        fetch_deployments,
        force_refresh,
    };
    let cache_key = aggregate_cache_key(
        owner,
        &format!("runs:{}:{remote_scope}", reader.cache_scope()),
        &targets,
    );
    let request_started = Instant::now();
    let cached = if force_refresh {
        None
    } else {
        runtime.delivery_cache().runs(&cache_key)
    };
    let aggregate = match cached {
        Some(cached) => cached,
        None => {
            let read = runtime.delivery_cache().run_read(&cache_key);
            let _guard = read.lock().await;
            if let Some(cached) = runtime.delivery_cache().runs(&cache_key) {
                if !force_refresh || cached.fetched_at >= request_started {
                    return run_page(capability, cached, &query);
                }
            }
            let workspace_index = workspace_index(runtime, owner, force_refresh).await?;
            let results = stream::iter(targets.clone())
                .map(|target| {
                    let reader = reader.clone();
                    let workspace_index = workspace_index.clone();
                    async move {
                        fetch_runs(
                            runtime,
                            owner,
                            &reader,
                            &target,
                            &workspace_index,
                            fetch_options,
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
                .delivery_cache()
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

pub(super) fn run_page(
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

pub async fn run_detail(
    runtime: &dyn DeliveryRuntime,
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
    let access = runtime.delivery_access(owner, false).await;
    let reader = access.require_reader()?;
    let api = reader
        .api(&target.repository)
        .await
        .map_err(|message| ServerError::bad_request_kind("github", message))?;
    let repository =
        resolve_repository_cached(runtime, api.as_ref(), &target.repository, None, false)
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
            let (run, jobs) = tokio::join!(api.get(&run_endpoint), api.get(&jobs_endpoint),);
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
            let (deployment, statuses) =
                tokio::join!(api.get(&deployment_endpoint), api.get(&statuses_endpoint),);
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

pub async fn act_on_run(
    runtime: &dyn DeliveryRuntime,
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
    if body.target.kind != CodeDeliveryRunKind::WorkflowRun {
        return Err(ServerError::bad_request(
            "only GitHub Actions workflow runs can be rerun",
        ));
    }
    let access = runtime.delivery_access(owner, false).await;
    let reader = access.require_reader()?;
    let api = reader.action_api(&body.target.repository).await?;
    match body.action {
        CodeDeliveryRunAction::Rerun => {
            api.rerun_workflow(&body.target.repository, body.target.id)
                .await?;
            runtime.delivery_cache().invalidate();
            Ok(delivery_action_result(format!(
                "Workflow run {} queued again",
                body.target.id
            )))
        }
        CodeDeliveryRunAction::RerunFailed => {
            api.rerun_failed_jobs(&body.target.repository, body.target.id)
                .await?;
            runtime.delivery_cache().invalidate();
            Ok(rerun_action_result(vec![CodeDeliveryRerunOutcome {
                workflow_run_id: body.target.id,
                success: true,
                error: None,
            }]))
        }
    }
}

pub(super) async fn fetch_runs(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    reader: &DeliveryReaderHandle,
    target: &CodeGitHubRepositoryTarget,
    workspaces: &[WorkspaceIndexEntry],
    options: RunFetchOptions,
) -> Result<FetchedRuns, String> {
    let api = reader.api(target).await?;
    let repository =
        resolve_repository_cached(runtime, api.as_ref(), target, None, options.force_refresh)
            .await?;
    let deployments = if options.fetch_deployments {
        api.deployments(target).await.map(Some)
    } else {
        Ok(None)
    };
    let mut fetched = collect_run_sources(target, &repository, workspaces, Ok(None), deployments);
    if options.fetch_workflows {
        match load_or_refresh_workflow_runs(
            runtime,
            owner,
            api.as_ref(),
            target,
            &repository,
            workspaces,
            options.force_refresh,
        )
        .await
        {
            Ok(items) => fetched.items.extend(items),
            Err(message) => {
                fetched
                    .errors
                    .push(detail_source_error(target, "workflow runs", message))
            }
        }
    }
    Ok(fetched)
}

/// Refresh stored workflow runs for every tracked repository (issue 2578).
///
/// The reconcile sweep calls this instead of `query_runs` so deployments
/// stay off the background path and every GitHub read goes through
/// the server's conditional host gate.
pub async fn refresh_workflow_runs(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    targets: &[CodeGitHubRepositoryTarget],
) {
    let access = runtime.delivery_access(owner, false).await;
    let Some(reader) = access.reader.clone() else {
        return;
    };
    let Ok(workspaces) = workspace_index(runtime, owner, false).await else {
        return;
    };
    for target in targets {
        let api = match reader.api(target).await {
            Ok(api) => api,
            Err(message) => {
                tracing::debug!(
                    host = target.host.as_str(),
                    owner = target.owner.as_str(),
                    name = target.name.as_str(),
                    error = message.as_str(),
                    "workflow-run transport failed"
                );
                continue;
            }
        };
        let repository = repository_ref_from_target(target, None);
        if let Err(message) = load_or_refresh_workflow_runs(
            runtime,
            owner,
            api.as_ref(),
            target,
            &repository,
            &workspaces,
            true,
        )
        .await
        {
            tracing::debug!(
                host = target.host.as_str(),
                owner = target.owner.as_str(),
                name = target.name.as_str(),
                error = message.as_str(),
                "workflow-run refresh failed"
            );
        }
    }
}

pub(super) async fn load_or_refresh_workflow_runs(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    api: &dyn DeliveryApi,
    target: &CodeGitHubRepositoryTarget,
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
    force_refresh: bool,
) -> Result<Vec<CodeDeliveryRunSummary>, String> {
    let stored = get_workflow_run_fetch_state(
        runtime.store(),
        owner,
        &target.host,
        &target.owner,
        &target.name,
    )
    .await
    .map_err(|err| err.to_string())?;
    if stored.is_some() && !force_refresh {
        return stored_workflow_run_summaries(runtime, owner, target, repository, workspaces).await;
    }

    let etag = stored.as_ref().and_then(|state| state.list_etag.clone());
    let sent = etag.clone();
    let read = api.workflow_runs(target, etag.as_deref()).await;
    match read {
        Ok(EndpointRead::Fresh { value, etag }) => {
            let summaries = persist_fresh_workflow_runs(
                runtime,
                owner,
                target,
                repository,
                workspaces,
                &value,
                etag.as_deref(),
            )
            .await?;
            Ok(summaries)
        }
        Ok(EndpointRead::NotModified) => {
            let now = Utc::now();
            let _ = set_workflow_run_fetch_state(
                runtime.store(),
                owner,
                &target.host,
                &target.owner,
                &target.name,
                sent.as_deref(),
                now,
                WorkflowRunFetchCondition::ListEtag(sent.as_deref()),
            )
            .await;
            stored_workflow_run_summaries(runtime, owner, target, repository, workspaces).await
        }
        Ok(EndpointRead::Missing) => Ok(Vec::new()),
        Err(HostReadError::Parked(_)) => {
            stored_workflow_run_summaries(runtime, owner, target, repository, workspaces).await
        }
        Err(failure) => Err(failure.to_string()),
    }
}

pub(super) async fn persist_fresh_workflow_runs(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    target: &CodeGitHubRepositoryTarget,
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
    runs: &[Value],
    etag: Option<&str>,
) -> Result<Vec<CodeDeliveryRunSummary>, String> {
    let now = Utc::now();
    let mut summaries = Vec::new();
    let mut changed = false;
    for value in runs.iter().take(MAX_REMOTE_ITEMS_PER_REPO) {
        let Some(summary) = parse_workflow_run(repository, value, workspaces) else {
            continue;
        };
        let Some(fact) = fact_from_run_summary(owner, &summary, now) else {
            continue;
        };
        match save_workflow_run_fact(runtime.store(), &fact).await {
            Ok((_, moved)) => changed |= moved,
            Err(err) => tracing::debug!(error = %err, "workflow-run persist failed"),
        }
        summaries.push(summary);
    }
    let keep: Vec<u64> = summaries.iter().map(|summary| summary.github_id).collect();
    match delete_workflow_run_facts_absent_from(
        runtime.store(),
        owner,
        &target.host,
        &target.owner,
        &target.name,
        &keep,
    )
    .await
    {
        Ok(pruned) => changed |= pruned > 0,
        Err(err) => tracing::debug!(error = %err, "workflow-run prune failed"),
    }
    let _ = set_workflow_run_fetch_state(
        runtime.store(),
        owner,
        &target.host,
        &target.owner,
        &target.name,
        etag,
        now,
        WorkflowRunFetchCondition::Unconditional,
    )
    .await;
    if changed {
        runtime.delivery_cache().invalidate_owner(owner);
        runtime.nudge_delivery_update(owner);
    }
    Ok(summaries)
}

/// Project stored facts for one repository, capped at GitHub's first page.
pub(super) async fn stored_workflow_run_summaries(
    runtime: &dyn DeliveryRuntime,
    owner: &OwnerId,
    target: &CodeGitHubRepositoryTarget,
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
) -> Result<Vec<CodeDeliveryRunSummary>, String> {
    let facts = list_workflow_run_facts_for_repo(
        runtime.store(),
        owner,
        &target.host,
        &target.owner,
        &target.name,
    )
    .await
    .map_err(|err| err.to_string())?;
    Ok(facts
        .into_iter()
        .take(MAX_REMOTE_ITEMS_PER_REPO)
        .map(|fact| summary_from_run_fact(&fact, repository, workspaces))
        .collect())
}

pub(super) fn fact_from_run_summary(
    owner: &OwnerId,
    summary: &CodeDeliveryRunSummary,
    now: DateTime<Utc>,
) -> Option<CodeWorkflowRunFact> {
    if summary.kind != CodeDeliveryRunKind::WorkflowRun {
        return None;
    }
    Some(CodeWorkflowRunFact {
        id: CodeWorkflowRunId::new(),
        owner: owner.clone(),
        host: summary.repository.host.clone(),
        repo_owner: summary.repository.owner.clone(),
        repo_name: summary.repository.name.clone(),
        github_id: summary.github_id,
        run_attempt: summary.run_attempt,
        name: summary.name.clone(),
        url: summary.url.clone(),
        status: summary.status.clone(),
        conclusion: summary.conclusion.clone(),
        workflow: summary.workflow.clone(),
        branch: summary.branch.clone(),
        sha: summary.sha.clone(),
        event: summary.event.clone(),
        actor: summary.actor.clone(),
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        first_seen_at: now,
        last_seen_at: now,
    })
}

pub(super) fn summary_from_run_fact(
    fact: &CodeWorkflowRunFact,
    repository: &CodeGitHubRepositoryRef,
    workspaces: &[WorkspaceIndexEntry],
) -> CodeDeliveryRunSummary {
    CodeDeliveryRunSummary {
        id: format!(
            "{}:workflow:{}",
            repository_key_ref(repository),
            fact.github_id
        ),
        repository: repository.clone(),
        kind: CodeDeliveryRunKind::WorkflowRun,
        github_id: fact.github_id,
        run_attempt: fact.run_attempt,
        name: fact.name.clone(),
        url: fact.url.clone(),
        status: fact.status.clone(),
        conclusion: fact.conclusion.clone(),
        workflow: fact.workflow.clone(),
        environment: None,
        branch: fact.branch.clone(),
        sha: fact.sha.clone(),
        event: fact.event.clone(),
        actor: fact.actor.clone(),
        attention_reasons: run_attention(fact.conclusion.as_deref()),
        workspace_links: links_for_run(
            repository,
            fact.sha.as_deref(),
            fact.branch.as_deref(),
            workspaces,
        ),
        created_at: fact.created_at,
        updated_at: fact.updated_at,
    }
}

pub(super) fn collect_run_sources(
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

pub(super) fn run_remote_scope(query: &CodeDeliveryRunQuery) -> (&'static str, bool, bool) {
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

pub(super) fn parse_workflow_run(
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

pub(super) fn parse_deployment(
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

pub(super) fn run_attention(conclusion: Option<&str>) -> Vec<CodeDeliveryRunAttentionReason> {
    match conclusion {
        Some("failure" | "error") => vec![CodeDeliveryRunAttentionReason::Failure],
        Some("timed_out") => vec![CodeDeliveryRunAttentionReason::TimedOut],
        Some("action_required") => vec![CodeDeliveryRunAttentionReason::ActionRequired],
        Some("startup_failure") => vec![CodeDeliveryRunAttentionReason::StartupFailure],
        _ => Vec::new(),
    }
}

pub(super) fn parse_workflow_job(value: &Value) -> Option<CodeDeliveryWorkflowJob> {
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

pub(super) fn parse_deployment_status(value: &Value) -> Option<CodeDeliveryDeploymentStatus> {
    Some(CodeDeliveryDeploymentStatus {
        id: u64_field(value, "id")?,
        state: text_field(value, "state")?.to_ascii_lowercase(),
        description: text_field(value, "description").unwrap_or_default(),
        environment_url: text_field(value, "environment_url").filter(|value| !value.is_empty()),
        log_url: text_field(value, "log_url").filter(|value| !value.is_empty()),
        created_at: datetime_field(value, "created_at").unwrap_or_else(Utc::now),
    })
}

pub(super) fn links_for_run(
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

pub(super) fn run_matches(item: &CodeDeliveryRunSummary, query: &CodeDeliveryRunQuery) -> bool {
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
