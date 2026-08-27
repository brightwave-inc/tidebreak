//! Delivery aggregate and reconcile coverage for pull-request facts (issue 2800).

use super::*;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{
    get_pull_request_fact, get_pull_request_fetch_state, insert_pull_request_attribution,
    insert_repo, insert_workspace, mark_pull_request_fact_stale, save_pull_request_fact,
    set_pull_request_fetch_state, set_pull_request_live_state, PullRequestFetchCondition,
};
use tidebreak_core::{
    CodePullRequestAttribution, CodePullRequestDiscovery, CodePullRequestFact, CodePullRequestId,
    CodePullRequestLiveState, CodePullRequestRelation, CodePullRequestState, CodeRepo,
    CodeWorkspace, CodeWorkspaceStatus, OwnerId, PullRequestCheck, PullRequestCheckBucket, RepoId,
    WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn write_gh_shim(dir: &std::path::Path, log: &std::path::Path) {
    let body = r#"#!/bin/sh
printf '%s\n' "$*" >> '__LOG__'
if [ "$1" = auth ]; then
  echo '{"hosts":{"github.com":[{"active":true,"state":"success","login":"tester"}]}}'
  exit 0
fi
case "$*" in
  *"pulls/12/reviews"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"reviews-12"\r\n\r\n'
    exit 1;;
  *"commits/sha-12/check-runs"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"checks-12"\r\n\r\n'
    exit 1;;
  *"rules/branches/main"*)
    printf 'HTTP/2.0 200 OK\r\nEtag: W/"rules"\r\n\r\n'
    echo '[]'
    exit 0;;
  *"pulls/12"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"pull-12"\r\n\r\n'
    exit 1;;
  *"pulls/13/reviews"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"reviews-13"\r\n\r\n'
    exit 1;;
  *"commits/sha-13/check-runs"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"checks-13"\r\n\r\n'
    exit 1;;
  *"pulls/13"*)
    printf 'HTTP/2.0 304 Not Modified\r\nEtag: W/"pull-13"\r\n\r\n'
    exit 1;;
  *"pr list"*)
    echo 'the pull-request list path must not run' >&2
    exit 42;;
esac
echo "unexpected gh command: $*" >&2
exit 3
"#
    .replace("__LOG__", &log.display().to_string());
    write_executable(&dir.join("gh"), &body);
}

async fn seeded_runtime() -> (
    tempfile::TempDir,
    Arc<CodeRuntime>,
    Arc<tidebreak_core::DbStore>,
    std::path::PathBuf,
) {
    let (dir, store) = temp_db_store("code-reconcile.db").await;
    let db = Arc::new(store);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db.clone(),
        dir.path().to_path_buf(),
        registry,
    ));
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = dir.path().join("gh.log");
    write_gh_shim(&bin, &log);
    runtime.set_gh_search_path(Some(bin.display().to_string()));
    (dir, runtime, db, log)
}

fn fact(owner: &OwnerId, repo_name: &str, number: u64, title: &str) -> CodePullRequestFact {
    let observed = Utc::now() - ChronoDuration::hours(1);
    CodePullRequestFact {
        id: CodePullRequestId::new(),
        owner: owner.clone(),
        host: "github.com".into(),
        repo_owner: "acme".into(),
        repo_name: repo_name.into(),
        number,
        url: format!("https://github.com/acme/{repo_name}/pull/{number}"),
        title: title.into(),
        state: CodePullRequestState::Open,
        draft: false,
        author: Some(owner.as_str().into()),
        head_branch: format!("feature-{number}"),
        base_branch: "main".into(),
        head_sha: Some(format!("sha-{number}")),
        created_at: observed,
        updated_at: observed,
        merged_at: None,
        closed_at: None,
        first_seen_at: observed,
        last_seen_at: observed,
        live: None,
    }
}

fn live(number: u64, observed_at: chrono::DateTime<Utc>) -> CodePullRequestLiveState {
    CodePullRequestLiveState {
        checks_summary: Some("1 passing, 0 pending, 0 failing".into()),
        checks: Some(vec![PullRequestCheck {
            name: "ci".into(),
            bucket: PullRequestCheckBucket::Pass,
            detail: Some("success".into()),
            url: Some(format!(
                "https://github.com/acme/tools/actions/runs/{number}"
            )),
        }]),
        review_decision: Some("approved".into()),
        mergeable: Some("mergeable".into()),
        merge_state_status: Some("clean".into()),
        auto_merge_enabled: Some(false),
        in_merge_queue: Some(false),
        observed_at,
    }
}

async fn seed_fact(
    db: &tidebreak_core::DbStore,
    fact: &CodePullRequestFact,
    live_state: &CodePullRequestLiveState,
) {
    save_pull_request_fact(db, fact).await.unwrap();
    let pull_etag = format!("W/\"pull-{}\"", fact.number);
    let checks_etag = format!("W/\"checks-{}\"", fact.number);
    let reviews_etag = format!("W/\"reviews-{}\"", fact.number);
    assert!(set_pull_request_fetch_state(
        db,
        &fact.owner,
        &fact.host,
        &fact.repo_owner,
        &fact.repo_name,
        fact.number,
        Some(fact),
        PullRequestFetchCondition::Unconditional,
        Some(&pull_etag),
        Some(&checks_etag),
        Some(&reviews_etag),
    )
    .await
    .unwrap());
    set_pull_request_live_state(
        db,
        &fact.owner,
        &fact.host,
        &fact.repo_owner,
        &fact.repo_name,
        fact.number,
        live_state,
    )
    .await
    .unwrap()
    .expect("the fact exists");
}

fn query(
    cursor: Option<&str>,
    limit: Option<u16>,
    refresh: bool,
) -> crate::routes::code::types::CodeDeliveryPullRequestQuery {
    crate::routes::code::types::CodeDeliveryPullRequestQuery {
        repositories: vec![crate::routes::code::types::CodeGitHubRepositoryTarget {
            host: "github.com".into(),
            owner: "acme".into(),
            name: "tools".into(),
        }],
        search: None,
        states: Vec::new(),
        review_states: Vec::new(),
        check_states: Vec::new(),
        authors: Vec::new(),
        attention_only: false,
        ready_only: false,
        tidebreak_linked: None,
        updated_after: None,
        cursor: cursor.map(str::to_owned),
        limit,
        refresh,
    }
}

async fn aggregate(
    runtime: &CodeRuntime,
    owner: &OwnerId,
) -> crate::routes::code::types::CodeDeliveryPullRequestsPage {
    crate::code::delivery::query_pull_requests(runtime, owner, true, query(None, None, false))
        .await
        .unwrap()
}

fn gh_commands(log: &std::path::Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

#[tokio::test]
async fn the_aggregate_reads_owner_scoped_facts_without_a_list_or_ttl() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let fresh = live(12, Utc::now());
    let owned = fact(&owner, "tools", 12, "Stored aggregate");
    seed_fact(&db, &owned, &fresh).await;
    seed_fact(
        &db,
        &fact(&stranger, "tools", 12, "Another owner's row"),
        &fresh,
    )
    .await;
    seed_fact(
        &db,
        &fact(&owner, "elsewhere", 13, "Another repository"),
        &live(13, Utc::now()),
    )
    .await;

    let page = aggregate(&runtime, &owner).await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 12);
    assert_eq!(page.items[0].title, "Stored aggregate");
    assert_eq!(page.items[0].checks.len(), 1);

    save_pull_request_fact(
        &db,
        &CodePullRequestFact {
            title: "Updated fact".into(),
            last_seen_at: Utc::now(),
            ..owned
        },
    )
    .await
    .unwrap();
    let updated = aggregate(&runtime, &owner).await;
    assert_eq!(updated.items.len(), 1);
    assert_eq!(updated.items[0].title, "Updated fact");

    let commands = gh_commands(&log);
    assert!(!commands.contains("pr list"), "{commands}");
    assert!(!commands.contains("repos/acme/tools/pulls/"), "{commands}");
}

#[tokio::test]
async fn the_aggregate_preserves_durable_attribution() {
    let (dir, runtime, db, _log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let repo_id = RepoId::new();
    insert_repo(
        &db,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: dir.path().display().to_string(),
            display_name: "tools".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: Some("github.com".into()),
            origin_owner: Some("acme".into()),
            origin_name: Some("tools".into()),
        },
    )
    .await
    .unwrap();
    let workspace_id = WorkspaceId::new();
    insert_workspace(
        &db,
        &CodeWorkspace {
            id: workspace_id,
            owner: owner.clone(),
            repo_id,
            title: "tracked".into(),
            worktree_path: dir.path().display().to_string(),
            branch_name: "review/fix".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        },
    )
    .await
    .unwrap();
    let stored = fact(&owner, "tools", 12, "Attributed");
    seed_fact(&db, &stored, &live(12, Utc::now())).await;
    assert!(insert_pull_request_attribution(
        &db,
        &CodePullRequestAttribution {
            owner: owner.clone(),
            pull_request_id: stored.id,
            workspace_id,
            relation: CodePullRequestRelation::Authored,
            discovered_via: CodePullRequestDiscovery::Command,
            session_id: None,
            parent_call_id: None,
            created_at: Utc::now(),
        },
    )
    .await
    .unwrap());

    let page = aggregate(&runtime, &owner).await;
    let link = page.items[0]
        .workspace_links
        .iter()
        .find(|link| link.workspace_id == workspace_id)
        .expect("the attributed workspace is linked");
    assert!(link.exact);
    assert_eq!(link.relation, Some(CodePullRequestRelation::Authored));
}

#[tokio::test]
async fn the_aggregate_refreshes_only_stale_identities_conditionally() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let fresh_at = Utc::now();
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "Stale live tier"),
        &live(12, fresh_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&owner, "tools", 13, "Fresh live tier"),
        &live(13, fresh_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&stranger, "tools", 12, "Another owner's row"),
        &live(12, fresh_at),
    )
    .await;
    assert!(
        mark_pull_request_fact_stale(&db, &owner, "github.com", "acme", "tools", 12)
            .await
            .unwrap()
    );

    let page = aggregate(&runtime, &owner).await;
    assert_eq!(page.items.len(), 2);
    let refreshed = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert!(refreshed.live.unwrap().observed_at > fresh_at);
    let untouched = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 13)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.live.unwrap().observed_at, fresh_at);
    let other_owner = get_pull_request_fact(&db, &stranger, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_owner.live.unwrap().observed_at, fresh_at);
    let state = get_pull_request_fetch_state(&db, &owner, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.pull_etag.as_deref(), Some("W/\"pull-12\""));

    let commands = gh_commands(&log);
    assert!(!commands.contains("pr list"), "{commands}");
    assert!(commands.contains("repos/acme/tools/pulls/12"), "{commands}");
    assert!(
        commands.contains("If-None-Match: W/\"pull-12\""),
        "{commands}"
    );
    assert!(commands.contains("commits/sha-12/check-runs"), "{commands}");
    assert!(commands.contains("pulls/12/reviews"), "{commands}");
    assert!(
        !commands.contains("repos/acme/tools/pulls/13"),
        "{commands}"
    );
}

#[tokio::test]
async fn a_cursor_read_never_starts_a_refresh() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stale_at = Utc::now() - ChronoDuration::minutes(10);
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "First"),
        &live(12, stale_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&owner, "tools", 13, "Second"),
        &live(13, stale_at),
    )
    .await;

    let page = crate::code::delivery::query_pull_requests(
        &runtime,
        &owner,
        true,
        query(Some("1"), Some(1), true),
    )
    .await
    .unwrap();
    assert_eq!(page.items.len(), 1);
    for number in [12, 13] {
        let row = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", number)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.live.unwrap().observed_at, stale_at);
    }
    let commands = gh_commands(&log);
    assert!(!commands.contains("repos/acme/tools/pulls/"), "{commands}");
    assert!(!commands.contains("pr list"), "{commands}");
}

#[tokio::test]
async fn an_explicit_first_page_refresh_fetches_a_fresh_row() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let fresh_at = Utc::now();
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "Fresh row"),
        &live(12, fresh_at),
    )
    .await;

    crate::code::delivery::query_pull_requests(&runtime, &owner, true, query(None, None, true))
        .await
        .unwrap();
    let refreshed = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert!(refreshed.live.unwrap().observed_at > fresh_at);
    let commands = gh_commands(&log);
    assert!(commands.contains("repos/acme/tools/pulls/12"), "{commands}");
    assert!(!commands.contains("pr list"), "{commands}");
}

#[tokio::test]
async fn reconcile_is_an_owner_scoped_stale_open_row_sweep() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let stale_at = Utc::now() - ChronoDuration::minutes(10);
    let fresh_at = Utc::now();
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "Local stale row"),
        &live(12, stale_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&stranger, "tools", 12, "Other owner's stale row"),
        &live(12, stale_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&owner, "tools", 13, "Fresh row"),
        &live(13, fresh_at),
    )
    .await;
    let settled = CodePullRequestFact {
        state: CodePullRequestState::Closed,
        closed_at: Some(stale_at),
        ..fact(&owner, "tools", 14, "Settled stale row")
    };
    seed_fact(&db, &settled, &live(14, stale_at)).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;

    for row_owner in [&owner, &stranger] {
        let row = get_pull_request_fetch_state(&db, row_owner, "github.com", "acme", "tools", 12)
            .await
            .unwrap()
            .unwrap();
        assert!(row.fact.live.unwrap().observed_at > stale_at);
        assert_eq!(row.pull_etag.as_deref(), Some("W/\"pull-12\""));
    }
    let fresh = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 13)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.live.unwrap().observed_at, fresh_at);
    let settled = get_pull_request_fetch_state(&db, &owner, "github.com", "acme", "tools", 14)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.fact.live.unwrap().observed_at, stale_at);
    assert_eq!(settled.pull_etag.as_deref(), Some("W/\"pull-14\""));

    let commands = gh_commands(&log);
    assert!(!commands.contains("pr list"), "{commands}");
    assert!(
        !commands.contains("repos/acme/tools/pulls/13"),
        "fresh rows must receive no scheduled read: {commands}"
    );
    assert!(
        !commands.contains("repos/acme/tools/pulls/14"),
        "settled rows must receive no scheduled read: {commands}"
    );
    assert_eq!(
        commands
            .lines()
            .filter(|command| command.ends_with("repos/acme/tools/pulls/12"))
            .count(),
        2,
        "each owner's row must take its own scoped refresh: {commands}"
    );
}
