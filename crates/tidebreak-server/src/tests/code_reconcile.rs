//! Reconcile sweep against a shimmed `gh` (decision 62).
//!
//! The sweep reads tracked repositories through the delivery path; these
//! tests assert what that read persists — fact snapshots, exact-tier
//! attribution, refreshed repo origin identity — and what it must never
//! mint: attribution from the branch-name guess.

use super::*;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{
    get_pull_request_fact, get_repo, insert_repo, insert_workspace,
    list_attributed_facts_for_workspace,
};
use tidebreak_core::{
    CodePullRequestRelation, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, OwnerId,
    PullRequestDigest, RepoId, WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

const LIST_JSON: &str = r#"[{"number":412,"url":"https://github.com/acme/tools/pull/412","state":"OPEN","title":"Tracked work","isDraft":false,"author":{"login":"octocat"},"reviewDecision":null,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","autoMergeRequest":null,"headRefName":"tidebreak/tracked","headRefOid":"aaa111","baseRefName":"main","updatedAt":"2026-08-22T12:00:00Z","createdAt":"2026-08-22T10:00:00Z","mergedAt":null,"closedAt":null,"labels":[]},{"number":500,"url":"https://github.com/acme/tools/pull/500","state":"OPEN","title":"Somebody else","isDraft":false,"author":{"login":"stranger"},"reviewDecision":null,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","autoMergeRequest":null,"headRefName":"tidebreak/fuzzy","headRefOid":"bbb222","baseRefName":"main","updatedAt":"2026-08-22T12:00:00Z","createdAt":"2026-08-22T11:00:00Z","mergedAt":null,"closedAt":null,"labels":[]}]"#;

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn run(cwd: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .status()
        .unwrap();
    assert!(status.success(), "{args:?} failed in {}", cwd.display());
}

fn init_github_shaped_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["git", "init", "-b", "main"]);
    run(&work, &["git", "config", "user.email", "dev@example.com"]);
    run(&work, &["git", "config", "user.name", "Dev"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    run(&work, &["git", "add", "README.md"]);
    run(&work, &["git", "commit", "-m", "init"]);
    run(&work, &["git", "branch", "tidebreak/tracked"]);
    run(&work, &["git", "branch", "tidebreak/fuzzy"]);
    run(
        &work,
        &[
            "git",
            "remote",
            "add",
            "origin",
            "https://github.com/acme/tools.git",
        ],
    );
    work
}

fn write_gh_shim(dir: &std::path::Path) {
    let body = "#!/bin/sh\n\
         if [ \"$1\" = auth ]; then\n\
           echo '{\"hosts\":{\"github.com\":[{\"active\":true,\"state\":\"success\",\"login\":\"tester\"}]}}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = api ]; then echo '{}'; exit 0; fi\n\
         if [ \"$1\" = pr ] && [ \"$2\" = list ]; then\n"
        .to_owned()
        + &format!("           echo '{LIST_JSON}'\n")
        + "           exit 0\n\
         fi\n\
         echo unexpected \"$@\" >&2\n\
         exit 3\n";
    write_executable(&dir.join("gh"), &body);
}

fn open_pr_digest(number: u64) -> PullRequestDigest {
    PullRequestDigest {
        number,
        url: Some(format!("https://github.com/acme/tools/pull/{number}")),
        state: "open".into(),
        title: None,
        checks_summary: None,
        checks: None,
        draft: None,
        merged: None,
        review_decision: None,
        mergeable: None,
        merge_state_status: None,
        head_branch: None,
        base_branch: None,
        head_sha: None,
        auto_merge_enabled: None,
        in_merge_queue: None,
    }
}

async fn seeded_runtime() -> (
    tempfile::TempDir,
    Arc<CodeRuntime>,
    Arc<tidebreak_core::DbStore>,
    RepoId,
    WorkspaceId,
    WorkspaceId,
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

    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim);
    runtime.set_gh_search_path(Some(shim.display().to_string()));

    let owner = OwnerId::local();
    let repo_id = RepoId::new();
    insert_repo(
        &db,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: work.display().to_string(),
            display_name: "tools".into(),
            default_base_ref: "main".into(),
            branch_prefix: "tidebreak/".into(),
            setup_script: None,
            archive_script: None,
            quick_actions: Vec::new(),
            created_at: chrono::Utc::now(),
            removed_at: None,
            cloned_from: None,
            origin_host: None,
            origin_owner: None,
            origin_name: None,
        },
    )
    .await
    .unwrap();
    // Tracked: the persisted digest number matches PR 412 (the exact tier).
    let tracked = WorkspaceId::new();
    insert_workspace(
        &db,
        &CodeWorkspace {
            id: tracked,
            owner: owner.clone(),
            repo_id,
            title: "tracked".into(),
            worktree_path: work.display().to_string(),
            branch_name: "tidebreak/tracked".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: Some(open_pr_digest(412)),
            created_at: chrono::Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        },
    )
    .await
    .unwrap();
    // Fuzzy: shares PR 500's head branch name but has no digest and, with a
    // never-pushed branch, no matching head SHA — the branch-name tier only.
    let fuzzy = WorkspaceId::new();
    insert_workspace(
        &db,
        &CodeWorkspace {
            id: fuzzy,
            owner: owner.clone(),
            repo_id,
            title: "fuzzy".into(),
            worktree_path: dir.path().join("fuzzy").display().to_string(),
            branch_name: "tidebreak/fuzzy".into(),
            base_ref: "main".into(),
            status: CodeWorkspaceStatus::Active,
            pr: None,
            created_at: chrono::Utc::now(),
            archived_at: None,
            released_at: None,
            released_tip: None,
            bundle_bytes: None,
        },
    )
    .await
    .unwrap();
    (dir, runtime, db, repo_id, tracked, fuzzy)
}

#[tokio::test]
async fn the_sweep_persists_facts_and_mints_exact_attribution_only() {
    let (_dir, runtime, db, repo_id, tracked, fuzzy) = seeded_runtime().await;
    let owner = OwnerId::local();

    crate::code::reconcile::sweep_reconcile(&runtime).await;

    // Both listed pull requests were read, but only the tracked one — the
    // exact number tier — persisted and minted.
    let fact = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .expect("the exact-linked pull request persists");
    assert_eq!(fact.head_branch, "tidebreak/tracked");
    let first_seen = fact.first_seen_at;
    assert!(
        get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 500)
            .await
            .unwrap()
            .is_none(),
        "a branch-name guess must not persist a fact"
    );

    let attributed = list_attributed_facts_for_workspace(&db, &owner, tracked)
        .await
        .unwrap();
    assert_eq!(attributed.len(), 1);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Contributed);
    assert!(
        list_attributed_facts_for_workspace(&db, &owner, fuzzy)
            .await
            .unwrap()
            .is_empty(),
        "the branch-name tier never mints attribution"
    );

    // The sweep recorded the origin identity it resolved.
    let repo = get_repo(&db, &owner, repo_id).await.unwrap().unwrap();
    assert_eq!(repo.origin_host.as_deref(), Some("github.com"));
    assert_eq!(repo.origin_owner.as_deref(), Some("acme"));
    assert_eq!(repo.origin_name.as_deref(), Some("tools"));

    // A second pass is idempotent: same single attribution, first_seen holds.
    runtime.delivery_cache.invalidate_owner(&owner);
    crate::code::reconcile::sweep_reconcile(&runtime).await;
    let attributed = list_attributed_facts_for_workspace(&db, &owner, tracked)
        .await
        .unwrap();
    assert_eq!(attributed.len(), 1);
    let fact = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fact.first_seen_at, first_seen);
}

#[tokio::test]
async fn a_delivery_read_serves_durable_links_with_the_relation() {
    let (_dir, runtime, db, _repo_id, tracked, _fuzzy) = seeded_runtime().await;
    let owner = OwnerId::local();

    crate::code::reconcile::sweep_reconcile(&runtime).await;
    // A fresh read (past the sweep's cache entry) folds the stored
    // attribution into the links.
    runtime.delivery_cache.invalidate_owner(&owner);
    let page = crate::code::delivery::query_pull_requests(
        &runtime,
        &owner,
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
    .unwrap();
    let item = page
        .items
        .iter()
        .find(|item| item.number == 412)
        .expect("the tracked pull request is on the page");
    let link = item
        .workspace_links
        .iter()
        .find(|link| link.workspace_id == tracked)
        .expect("the tracked workspace stays linked");
    assert!(link.exact);
    assert_eq!(link.relation, Some(CodePullRequestRelation::Contributed));

    // The workspace list route's backing read returns the fact.
    let listed = runtime
        .workspace_pull_requests(&owner, tracked)
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].0.number, 412);
    let _ = db;
}
