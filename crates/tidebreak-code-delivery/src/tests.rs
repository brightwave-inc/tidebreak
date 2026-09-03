//! Delivery unit tests.

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
fn a_stored_fact_projects_a_delivery_row_without_heuristic_links() {
    let now = Utc::now();
    let fact = CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: OwnerId::local(),
        host: "github.com".into(),
        repo_owner: "acme".into(),
        repo_name: "tools".into(),
        number: 412,
        url: "https://github.com/acme/tools/pull/412".into(),
        title: "Tracked work".into(),
        state: CodePullRequestState::Open,
        draft: false,
        author: Some("octocat".into()),
        head_branch: "tidebreak/tracked".into(),
        base_branch: "main".into(),
        head_sha: Some("aaa111".into()),
        created_at: now,
        updated_at: now,
        merged_at: None,
        closed_at: None,
        first_seen_at: now,
        last_seen_at: now,
        live: None,
    };
    let observation = observation_from_fact(&fact, repository_ref());
    assert!(!observation.from_host);
    assert_eq!(observation.summary.number, 412);
    assert!(observation.summary.workspace_links.is_empty());
    assert_eq!(observation.summary.head_sha.as_deref(), Some("aaa111"));
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
        origin_host: None,
        origin_owner: None,
        origin_name: None,
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
    assert!(settled.fields.contains("headRepository"));
    assert!(settled.fields.contains("headRepositoryOwner"));

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

/// The default Delivery view is one author's open pull requests. Asking
/// GitHub for everyone's and narrowing afterwards would spend the 100-row
/// per-repository cap on other people's work, so a lone author reaches the
/// remote read — and takes its own cache scope, because the rows it comes
/// back with are not the unscoped aggregate.
#[test]
fn a_single_author_reaches_the_remote_read() {
    let mut query = pull_request_query();
    query.states = vec!["open".into()];
    let everyone = pull_request_remote_plan(&query);
    assert_eq!(everyone.author, None);

    query.authors = vec![" mara ".into()];
    let mine = pull_request_remote_plan(&query);
    assert_eq!(mine.author.as_deref(), Some("mara"));
    assert_ne!(mine.cache_scope(), everyone.cache_scope());

    // A union of authors is not something `gh pr list` can express.
    query.authors = vec!["mara".into(), "devon".into()];
    assert_eq!(pull_request_remote_plan(&query).author, None);
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
fn a_stored_workflow_run_projects_the_same_summary_as_a_host_parse() {
    let repository = repository_ref();
    let value = serde_json::json!({
        "id": 41,
        "run_attempt": 3,
        "status": "completed",
        "conclusion": "failure",
        "name": "Desktop CI",
        "display_title": "fix the build",
        "html_url": "https://github.com/brightwave-inc/tidebreak/actions/runs/41",
        "head_branch": "main",
        "head_sha": "abc123",
        "event": "push",
        "actor": { "login": "octocat" },
        "created_at": "2026-08-27T00:00:00Z",
        "updated_at": "2026-08-27T00:01:00Z",
    });
    let parsed = parse_workflow_run(&repository, &value, &[]).unwrap();
    let now = Utc::now();
    let fact = fact_from_run_summary(&OwnerId::local(), &parsed, now).unwrap();
    assert!(
        fact.snapshot_differs(&CodeWorkflowRunFact {
            status: "in_progress".into(),
            conclusion: None,
            ..fact.clone()
        }),
        "a status move is a real change"
    );
    let projected = summary_from_run_fact(&fact, &repository, &[]);
    assert_eq!(projected.kind, CodeDeliveryRunKind::WorkflowRun);
    assert_eq!(projected.github_id, 41);
    assert_eq!(projected.run_attempt, Some(3));
    assert_eq!(projected.name, parsed.name);
    assert_eq!(projected.status, "completed");
    assert_eq!(projected.conclusion.as_deref(), Some("failure"));
    assert_eq!(projected.attention_reasons, parsed.attention_reasons);
    assert_eq!(projected.sha.as_deref(), Some("abc123"));
    assert_eq!(
        fact_from_run_summary(
            &OwnerId::local(),
            &CodeDeliveryRunSummary {
                kind: CodeDeliveryRunKind::Deployment,
                ..parsed
            },
            now
        ),
        None,
        "deployments stay live observations"
    );
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
fn an_empty_check_conclusion_defers_to_its_live_status() {
    let parsed = parse_check(&serde_json::json!({
        "name": "Build preview image",
        "conclusion": "",
        "status": "IN_PROGRESS",
        "detailsUrl": "https://github.com/example/app/actions/runs/42"
    }))
    .unwrap();

    assert_eq!(parsed.bucket, PullRequestCheckBucket::Pending);
    assert_eq!(parsed.detail.as_deref(), Some("in_progress"));
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
    assert_eq!(parsed.summary.state, "merged");
    assert!(parsed.summary.merged_at.is_some());
    assert!(parsed.summary.closed_at.is_some());
    assert_eq!(parsed.summary.labels, vec!["performance", "desktop"]);
    // A settled pull request never asks for attention and is never ready.
    assert!(parsed.summary.attention_reasons.is_empty());
    assert!(!parsed.summary.ready_to_merge);
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
    assert_eq!(parsed.summary.state, "merged");
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
    assert_eq!(parsed.summary.state, "open");
    assert!(parsed.summary.merged_at.is_none());
    assert!(parsed.summary.closed_at.is_none());
}

#[test]
fn merge_queue_membership_prefers_the_timeline_flag() {
    let queued = serde_json::json!({
        "number": 2740,
        "title": "Queued change",
        "state": "OPEN",
        "url": "https://github.com/brightwave-inc/tidebreak/pull/2740",
        "headRefName": "thet/fix",
        "baseRefName": "main",
        "mergeStateStatus": "BLOCKED",
        "inMergeQueue": true,
        "statusCheckRollup": [{
            "name": "CI",
            "status": "IN_PROGRESS",
            "state": "PENDING",
            "conclusion": null
        }]
    });
    let parsed = parse_pull_request(&repository_ref(), &queued, &[]).unwrap();
    assert_eq!(parsed.summary.in_merge_queue, Some(true));

    let unqueued = serde_json::json!({
        "number": 2740,
        "title": "Open change",
        "state": "OPEN",
        "url": "https://github.com/brightwave-inc/tidebreak/pull/2740",
        "headRefName": "thet/fix",
        "baseRefName": "main",
        "mergeStateStatus": "BLOCKED",
        "inMergeQueue": false
    });
    let parsed = parse_pull_request(&repository_ref(), &unqueued, &[]).unwrap();
    assert_eq!(parsed.summary.in_merge_queue, Some(false));

    let host_queued = serde_json::json!({
        "number": 2740,
        "title": "Host queued",
        "state": "OPEN",
        "url": "https://github.com/brightwave-inc/tidebreak/pull/2740",
        "headRefName": "thet/fix",
        "baseRefName": "main",
        "mergeStateStatus": "queued"
    });
    let parsed = parse_pull_request(&repository_ref(), &host_queued, &[]).unwrap();
    assert_eq!(parsed.summary.in_merge_queue, Some(true));
}

#[test]
fn comment_count_reads_rest_numbers_gh_arrays_and_connections() {
    let rest = serde_json::json!({
        "number": 1,
        "title": "count",
        "state": "OPEN",
        "url": "https://github.com/example/demo/pull/1",
        "headRefName": "f",
        "baseRefName": "main",
        "comments": 4
    });
    assert_eq!(
        parse_pull_request(&repository_ref(), &rest, &[])
            .unwrap()
            .summary
            .comment_count,
        Some(4)
    );

    let gh_list = serde_json::json!({
        "number": 1,
        "title": "count",
        "state": "OPEN",
        "url": "https://github.com/example/demo/pull/1",
        "headRefName": "f",
        "baseRefName": "main",
        "comments": [{"body": "a"}, {"body": "b"}]
    });
    assert_eq!(
        parse_pull_request(&repository_ref(), &gh_list, &[])
            .unwrap()
            .summary
            .comment_count,
        Some(2)
    );

    let connection = serde_json::json!({
        "number": 1,
        "title": "count",
        "state": "OPEN",
        "url": "https://github.com/example/demo/pull/1",
        "headRefName": "f",
        "baseRefName": "main",
        "comments": {"totalCount": 7, "nodes": []}
    });
    assert_eq!(
        parse_pull_request(&repository_ref(), &connection, &[])
            .unwrap()
            .summary
            .comment_count,
        Some(7)
    );

    let missing = serde_json::json!({
        "number": 1,
        "title": "count",
        "state": "OPEN",
        "url": "https://github.com/example/demo/pull/1",
        "headRefName": "f",
        "baseRefName": "main",
        "comments": null
    });
    assert_eq!(
        parse_pull_request(&repository_ref(), &missing, &[])
            .unwrap()
            .summary
            .comment_count,
        None
    );
}

#[test]
fn issue_comment_overlay_skips_crowding_issues() {
    let mut needed = HashSet::from([17, 19]);
    let mut counts = HashMap::new();
    absorb_issue_comment_counts(
        &[
            serde_json::json!({"number": 1, "comments": 9}),
            serde_json::json!({"number": 17, "comments": 2}),
        ],
        &mut needed,
        &mut counts,
    );
    assert_eq!(counts.get(&17), Some(&2));
    assert!(!needed.contains(&17));
    absorb_issue_comment_counts(
        &[serde_json::json!({"number": 19, "comments": 4})],
        &mut needed,
        &mut counts,
    );
    assert!(needed.is_empty());
    assert_eq!(counts.get(&19), Some(&4));
}

#[test]
fn pull_request_head_repository_requires_consistent_host_identity() {
    let value = serde_json::json!({
        "number": 2252,
        "title": "Qualify stack identity",
        "state": "OPEN",
        "url": "https://github.com/brightwave-inc/tidebreak/pull/2252",
        "headRepository": {
            "name": "tidebreak",
            "nameWithOwner": "Thet/Tidebreak"
        },
        "headRepositoryOwner": {"login": "thet"},
        "headRefName": "thet/stack-child",
        "baseRefName": "thet/stack-parent"
    });
    let parsed = parse_pull_request(&repository_ref(), &value, &[]).unwrap();
    assert_eq!(
        parsed.head_repository,
        StackRepositoryIdentity::new("github.com", "thet", "tidebreak")
    );

    let conflicting = serde_json::json!({
        "number": 2253,
        "title": "Reject conflicting identity",
        "state": "OPEN",
        "url": "https://github.com/brightwave-inc/tidebreak/pull/2253",
        "headRepository": {
            "name": "tidebreak",
            "nameWithOwner": "alice/tidebreak"
        },
        "headRepositoryOwner": {"login": "bob"},
        "headRefName": "stack-child",
        "baseRefName": "stack-parent"
    });
    assert!(parse_pull_request(&repository_ref(), &conflicting, &[])
        .unwrap()
        .head_repository
        .is_none());
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
            // Conflicts outrank everything: a conflicted tree blocks
            // the fixes every other reason would ask for.
            CodeDeliveryPrAttentionReason::Conflicts,
            CodeDeliveryPrAttentionReason::ChangesRequested,
            CodeDeliveryPrAttentionReason::ChecksFailed,
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
fn host_stacks_parse_in_payload_order_with_open_parents_only() {
    let payload: Value = serde_json::from_str(
        r#"[
            {
                "id": 901,
                "number": 7,
                "node_id": "S_7",
                "url": "https://github.com/brightwave-inc/tidebreak/stacks/7",
                "base": {"ref": "main"},
                "open": true,
                "created_at": "2026-08-20T10:00:00Z",
                "pull_requests": [
                    {"number": 410, "state": "closed", "draft": false,
                     "merged_at": "2026-08-21T09:00:00Z",
                     "head": {"ref": "tidebreak/base", "sha": "aaa000"}},
                    {"number": 411, "state": "open", "draft": true,
                     "merged_at": null,
                     "head": {"ref": "tidebreak/middle", "sha": "bbb111"}},
                    {"number": 412, "state": "open", "draft": false,
                     "merged_at": null,
                     "head": {"ref": "tidebreak/top", "sha": "ccc222"}}
                ]
            },
            {"number": 8, "pull_requests": []},
            "not a stack"
        ]"#,
    )
    .unwrap();
    let memberships = parse_stack_memberships(&payload);
    assert_eq!(memberships.len(), 3, "malformed stacks parse around");
    // The merged bottom layer parents nothing: the nearest open member
    // below decides, and 411 has none.
    let bottom = &memberships[&411];
    assert_eq!(bottom.stack_number, 7);
    assert_eq!(bottom.stack_size, 3);
    assert_eq!(bottom.parent_number, None);
    let top = &memberships[&412];
    assert_eq!(top.stack_number, 7);
    assert_eq!(top.stack_size, 3);
    assert_eq!(top.parent_number, Some(411));
    assert_eq!(memberships[&410].parent_number, None);

    let stack = payload
        .as_array()
        .and_then(|stacks| stacks.first())
        .and_then(parse_host_stack)
        .expect("the first stack parses");
    assert_eq!(
        stack
            .members
            .iter()
            .map(|member| member.number)
            .collect::<Vec<_>>(),
        vec![410, 411, 412],
        "members keep the payload's bottom-to-top order"
    );
    assert_eq!(stack.members[0].state, "closed");
    assert_eq!(
        stack.members[0].merged_at.as_deref(),
        Some("2026-08-21T09:00:00Z")
    );
    assert!(stack.members[1].draft);
    assert_eq!(stack.members[2].head_branch, "tidebreak/top");
}

#[test]
fn stack_detail_keeps_payload_order_and_stays_absent_on_failure() {
    let payload: Value = serde_json::from_str(
        r#"[
            {"number": 9, "pull_requests": [
                {"number": 500, "state": "open", "draft": false, "merged_at": null,
                 "head": {"ref": "tidebreak/far", "sha": "ddd333"}},
                {"number": 501, "state": "open", "draft": false, "merged_at": null,
                 "head": {"ref": "tidebreak/near", "sha": "eee444"}}
            ]},
            {"number": 10, "pull_requests": [
                {"number": 501, "state": "open", "draft": false, "merged_at": null,
                 "head": {"ref": "tidebreak/other", "sha": "fff555"}}
            ]}
        ]"#,
    )
    .unwrap();
    let (members, membership) = parse_stack_detail(Ok(&payload), 501)
        .expect("the first stack naming the pull request is the chain");
    assert_eq!(
        members
            .iter()
            .map(|member| member.number)
            .collect::<Vec<_>>(),
        vec![500, 501],
        "the chain keeps the payload's bottom-to-top order"
    );
    assert_eq!(membership.stack_number, 9);
    assert_eq!(membership.stack_size, 2);
    assert_eq!(membership.parent_number, Some(500));

    // A read that failed, or a payload without this pull request, is
    // simply no chain — never an error entry on the drawer.
    assert!(parse_stack_detail(Err("gh api timed out"), 501).is_none());
    let empty = serde_json::json!([]);
    assert!(parse_stack_detail(Ok(&empty), 501).is_none());
    assert!(parse_stack_detail(Ok(&payload), 499).is_none());
}

#[test]
fn a_blocked_merge_state_is_not_attention_while_checks_run() {
    // GitHub says blocked whenever required checks are still running
    // (decision 66); only a blocked state with no checks in flight is
    // something the reader can act on.
    let running = vec![CodeDeliveryCheck {
        name: "test".into(),
        bucket: PullRequestCheckBucket::Pending,
        detail: None,
        url: None,
        workflow_run_id: None,
    }];
    assert!(
        pull_request_attention("open", false, None, None, Some("blocked"), &running).is_empty()
    );
    assert_eq!(
        pull_request_attention("open", false, None, None, Some("blocked"), &[]),
        vec![CodeDeliveryPrAttentionReason::Blocked]
    );
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
