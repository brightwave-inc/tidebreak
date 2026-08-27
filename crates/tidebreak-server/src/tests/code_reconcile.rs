//! Delivery aggregate and reconcile coverage for pull-request facts (issue 2800).

use super::*;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{
    get_pull_request_fact, get_pull_request_fetch_state, mark_pull_request_fact_stale,
    save_pull_request_fact, set_pull_request_fetch_state, set_pull_request_live_state,
    PullRequestFetchCondition,
};
use tidebreak_core::{
    CodePullRequestFact, CodePullRequestId, CodePullRequestLiveState, CodePullRequestState,
    OwnerId, PullRequestCheck, PullRequestCheckBucket,
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
  *"pr list"*)
    echo '[{"number":12,"url":"https://github.com/acme/tools/pull/12","state":"OPEN","title":"Listed instead of stored","isDraft":false,"author":{"login":"tester"},"headRefName":"feature-12","headRefOid":"sha-12","baseRefName":"main","updatedAt":"2026-08-27T12:00:00Z","createdAt":"2026-08-27T10:00:00Z","labels":[]}]'
    exit 0;;
  *"repos/acme/tools/stacks?per_page=100"*|*"repos/acme/tools/issues?"*|*"repos/acme/tools/issues/12/timeline"*)
    echo '[]'
    exit 0;;
  "api repos/acme/tools")
    echo '{"name":"tools","full_name":"acme/tools","html_url":"https://github.com/acme/tools","default_branch":"main","owner":{"login":"acme"}}'
    exit 0;;
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

fn live(observed_at: chrono::DateTime<Utc>) -> CodePullRequestLiveState {
    CodePullRequestLiveState {
        checks_summary: Some("1 passing, 0 pending, 0 failing".into()),
        checks: Some(vec![PullRequestCheck {
            name: "ci".into(),
            bucket: PullRequestCheckBucket::Pass,
            detail: Some("success".into()),
            url: Some("https://github.com/acme/tools/actions/runs/12".into()),
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

async fn aggregate(
    runtime: &CodeRuntime,
    owner: &OwnerId,
) -> crate::routes::code::types::CodeDeliveryPullRequestsPage {
    crate::code::delivery::query_pull_requests(
        runtime,
        owner,
        true,
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
            cursor: None,
            limit: None,
            refresh: false,
        },
    )
    .await
    .unwrap()
}

fn gh_commands(log: &std::path::Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

#[tokio::test]
async fn a_hot_aggregate_reads_owner_scoped_facts_without_a_pull_request_list() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let fresh = live(Utc::now());
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
        &fresh,
    )
    .await;

    let page = aggregate(&runtime, &owner).await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].number, 12);
    assert_eq!(page.items[0].title, "Stored aggregate");
    assert_eq!(page.items[0].checks.len(), 1);
    assert_eq!(page.items[0].checks[0].name, "ci");

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
    runtime.delivery_cache.invalidate_owner(&owner);
    let updated = aggregate(&runtime, &owner).await;
    assert_eq!(updated.items.len(), 1);
    assert_eq!(updated.items[0].title, "Updated fact");

    let commands = gh_commands(&log);
    assert!(!commands.contains("pr list"), "{commands}");
    assert!(!commands.contains("repos/acme/tools/pulls/"), "{commands}");
}

#[tokio::test]
async fn a_reconcile_pass_preserves_the_pull_etag_without_listing() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stored = fact(&owner, "tools", 12, "Stored aggregate");
    seed_fact(&db, &stored, &live(Utc::now())).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;

    let state = get_pull_request_fetch_state(&db, &owner, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.pull_etag.as_deref(), Some("W/\"pull-12\""));
    let commands = gh_commands(&log);
    assert!(
        commands.is_empty(),
        "a fresh row sweep must not probe GitHub: {commands}"
    );
}

#[tokio::test]
async fn reconcile_refreshes_stale_rows_under_their_own_owner() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let stale_at = Utc::now() - ChronoDuration::minutes(10);
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "Local stale row"),
        &live(stale_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&stranger, "tools", 12, "Other owner's stale row"),
        &live(stale_at),
    )
    .await;
    let settled = CodePullRequestFact {
        state: CodePullRequestState::Closed,
        closed_at: Some(stale_at),
        ..fact(&owner, "tools", 14, "Settled stale row")
    };
    seed_fact(&db, &settled, &live(stale_at)).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;

    for row_owner in [&owner, &stranger] {
        let state = get_pull_request_fetch_state(&db, row_owner, "github.com", "acme", "tools", 12)
            .await
            .unwrap()
            .unwrap();
        assert!(state.fact.live.unwrap().observed_at > stale_at);
        assert_eq!(state.pull_etag.as_deref(), Some("W/\"pull-12\""));
    }
    let settled = get_pull_request_fetch_state(&db, &owner, "github.com", "acme", "tools", 14)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(settled.fact.live.unwrap().observed_at, stale_at);
    assert_eq!(settled.pull_etag.as_deref(), Some("W/\"pull-14\""));
    let commands = gh_commands(&log);
    assert!(!commands.contains("pr list"), "{commands}");
    assert!(
        !commands.contains("repos/acme/tools/pulls/14"),
        "settled rows must receive no scheduled reads: {commands}"
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

#[tokio::test]
async fn the_aggregate_refreshes_only_stale_rows_through_the_conditional_fetcher() {
    let (_dir, runtime, db, log) = seeded_runtime().await;
    let owner = OwnerId::local();
    let stranger = OwnerId::new("stranger").unwrap();
    let fresh_at = Utc::now();
    seed_fact(
        &db,
        &fact(&owner, "tools", 12, "Invalidated live tier"),
        &live(fresh_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&owner, "tools", 13, "Fresh live tier"),
        &live(fresh_at),
    )
    .await;
    seed_fact(
        &db,
        &fact(&stranger, "tools", 12, "Other owner's live tier"),
        &live(fresh_at),
    )
    .await;

    assert!(
        mark_pull_request_fact_stale(&db, &owner, "github.com", "acme", "tools", 12,)
            .await
            .unwrap()
    );
    let invalidated = get_pull_request_fetch_state(&db, &owner, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert!(invalidated.fact.live.unwrap().observed_at < fresh_at);
    assert_eq!(invalidated.pull_etag.as_deref(), Some("W/\"pull-12\""));
    let other_owner = get_pull_request_fact(&db, &stranger, "github.com", "acme", "tools", 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(other_owner.live.unwrap().observed_at, fresh_at);

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
