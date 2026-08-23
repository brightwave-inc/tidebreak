//! Fact-edge trigger conditions and stack-aware classification (decision 62).
//!
//! These drive the real sweeps against a shimmed `gh`: the reconcile sweep
//! seeds durable facts, then the trigger sweep fires `pr_opened` and
//! `pr_updated` from the local store — no host read — and holds a stacked
//! child's `behind` fire. Deliveries stay pending throughout because the
//! fixture runs no sessions; the rows are the assertions.

use super::*;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{
    arm_trigger, get_pull_request_fact, insert_repo, insert_workspace, list_fires_for_workspace,
    list_triggers_for_repo, save_pull_request_fact, trigger_fire_heads_for_pr,
};
use tidebreak_core::{
    CodeRepo, CodeTrigger, CodeTriggerAction, CodeTriggerCondition, CodeTriggerFireState,
    CodeTriggerId, CodeWorkspace, CodeWorkspaceStatus, OwnerId, PullRequestDigest, RepoId,
    WorkspaceId,
};
use tidebreak_harness::AdapterRegistry;

const PR_412: &str = r#"{"number":412,"url":"https://github.com/acme/tools/pull/412","state":"OPEN","title":"Tracked work","isDraft":false,"author":{"login":"octocat"},"reviewDecision":null,"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","autoMergeRequest":null,"headRefName":"tidebreak/tracked","headRefOid":"aaa111","baseRefName":"main","updatedAt":"2026-08-22T12:00:00Z","createdAt":"2026-08-22T10:00:00Z","mergedAt":null,"closedAt":null,"labels":[],"statusCheckRollup":[]}"#;
const PR_413: &str = r#"{"number":413,"url":"https://github.com/acme/tools/pull/413","state":"OPEN","title":"Stacked child","isDraft":false,"author":{"login":"octocat"},"reviewDecision":null,"mergeable":"MERGEABLE","mergeStateStatus":"BEHIND","autoMergeRequest":null,"headRefName":"tidebreak/tracked-child","headRefOid":"ccc333","baseRefName":"tidebreak/tracked","updatedAt":"2026-08-22T12:30:00Z","createdAt":"2026-08-22T12:00:00Z","mergedAt":null,"closedAt":null,"labels":[],"statusCheckRollup":[]}"#;

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
    run(&work, &["git", "branch", "tidebreak/tracked-child"]);
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
         if [ \"$1\" = api ]; then echo '{}'; exit 0; fi\n"
        .to_owned()
        + &format!(
            "         if [ \"$1\" = pr ] && [ \"$2\" = list ]; then\n\
                        echo '[{PR_412},{PR_413}]'\n\
                        exit 0\n\
                      fi\n\
                      if [ \"$1\" = pr ] && [ \"$2\" = view ] && [ \"$3\" = 412 ]; then\n\
                        echo '{PR_412}'\n\
                        exit 0\n\
                      fi\n\
                      if [ \"$1\" = pr ] && [ \"$2\" = view ] && [ \"$3\" = 413 ]; then\n\
                        echo '{PR_413}'\n\
                        exit 0\n\
                      fi\n"
        )
        + "         echo unexpected \"$@\" >&2\n\
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

fn workspace_row(
    id: WorkspaceId,
    owner: &OwnerId,
    repo_id: RepoId,
    title: &str,
    worktree: &std::path::Path,
    branch: &str,
    pr: Option<PullRequestDigest>,
) -> CodeWorkspace {
    CodeWorkspace {
        id,
        owner: owner.clone(),
        repo_id,
        title: title.into(),
        worktree_path: worktree.display().to_string(),
        branch_name: branch.into(),
        base_ref: "main".into(),
        status: CodeWorkspaceStatus::Active,
        pr,
        created_at: chrono::Utc::now(),
        archived_at: None,
        released_at: None,
        released_tip: None,
        bundle_bytes: None,
    }
}

/// Repo, tracked workspace on PR 412, child workspace on PR 413, shimmed gh.
async fn seeded_runtime() -> (
    tempfile::TempDir,
    Arc<CodeRuntime>,
    Arc<tidebreak_core::DbStore>,
    RepoId,
    WorkspaceId,
    WorkspaceId,
) {
    let (dir, store) = temp_db_store("code-trigger-facts.db").await;
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
    let tracked = WorkspaceId::new();
    insert_workspace(
        &db,
        &workspace_row(
            tracked,
            &owner,
            repo_id,
            "tracked",
            &work,
            "tidebreak/tracked",
            Some(open_pr_digest(412)),
        ),
    )
    .await
    .unwrap();
    let child = WorkspaceId::new();
    insert_workspace(
        &db,
        &workspace_row(
            child,
            &owner,
            repo_id,
            "child",
            &dir.path().join("child"),
            "tidebreak/tracked-child",
            Some(open_pr_digest(413)),
        ),
    )
    .await
    .unwrap();
    (dir, runtime, db, repo_id, tracked, child)
}

async fn armed(
    db: &tidebreak_core::DbStore,
    owner: &OwnerId,
    repo_id: RepoId,
    condition: CodeTriggerCondition,
) -> CodeTriggerId {
    arm_trigger(
        db,
        owner,
        &CodeTrigger {
            id: CodeTriggerId::new(),
            owner: owner.clone(),
            repo_id,
            condition,
            action: CodeTriggerAction::Notify,
            enabled: true,
            // Armed before the fixture pull requests existed, so the
            // opened-edge watermark admits them.
            created_at: "2026-08-01T00:00:00Z".parse().unwrap(),
            updated_at: "2026-08-01T00:00:00Z".parse().unwrap(),
        },
    )
    .await
    .unwrap();
    list_triggers_for_repo(db, owner, repo_id)
        .await
        .unwrap()
        .into_iter()
        .find(|trigger| trigger.condition == condition)
        .expect("the armed trigger reads back")
        .id
}

#[tokio::test]
async fn pr_opened_fires_once_per_pull_request() {
    let (_dir, runtime, db, repo_id, tracked, _child) = seeded_runtime().await;
    let owner = OwnerId::local();
    let trigger = armed(&db, &owner, repo_id, CodeTriggerCondition::PrOpened).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;
    assert!(
        get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .is_some(),
        "the reconcile sweep seeds the fact the edge fires from"
    );

    crate::code::trigger::sweep_triggers(&runtime).await;
    let heads = trigger_fire_heads_for_pr(&db, &owner, trigger, tracked, 412)
        .await
        .unwrap();
    assert_eq!(heads, vec!["opened".to_owned()]);

    // A second sweep re-reads the same facts and mints nothing new.
    crate::code::trigger::sweep_triggers(&runtime).await;
    let heads = trigger_fire_heads_for_pr(&db, &owner, trigger, tracked, 412)
        .await
        .unwrap();
    assert_eq!(heads.len(), 1);
}

#[tokio::test]
async fn pr_updated_baselines_then_fires_on_a_new_head() {
    let (_dir, runtime, db, repo_id, tracked, _child) = seeded_runtime().await;
    let owner = OwnerId::local();
    let trigger = armed(&db, &owner, repo_id, CodeTriggerCondition::PrUpdated).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;
    crate::code::trigger::sweep_triggers(&runtime).await;

    // First sight is a settled baseline: the row exists and delivers nothing.
    let fires = list_fires_for_workspace(&db, &owner, tracked)
        .await
        .unwrap();
    let baseline = fires
        .iter()
        .find(|fire| fire.identity.trigger_id == trigger && fire.identity.pr_number == 412)
        .expect("the baseline row exists");
    assert_eq!(baseline.identity.head_sha, "aaa111");
    assert_eq!(baseline.state, CodeTriggerFireState::Delivered);
    assert!(baseline.payload.is_none());

    // The head moves; the next sweep fires exactly the new head.
    let mut fact = get_pull_request_fact(&db, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    fact.head_sha = Some("bbb999".into());
    save_pull_request_fact(&db, &fact).await.unwrap();
    crate::code::trigger::sweep_triggers(&runtime).await;

    let fires = list_fires_for_workspace(&db, &owner, tracked)
        .await
        .unwrap();
    let fired = fires
        .iter()
        .find(|fire| fire.identity.trigger_id == trigger && fire.identity.head_sha == "bbb999")
        .expect("the moved head fires");
    assert_eq!(fired.state, CodeTriggerFireState::Pending);
    assert!(fired.payload.is_some());
}

#[tokio::test]
async fn a_stacked_child_holds_behind_fires_and_parks_the_watch_lookup() {
    let (_dir, runtime, db, repo_id, _tracked, child) = seeded_runtime().await;
    let owner = OwnerId::local();
    let trigger = armed(&db, &owner, repo_id, CodeTriggerCondition::Behind).await;

    crate::code::reconcile::sweep_reconcile(&runtime).await;
    crate::code::trigger::sweep_triggers(&runtime).await;

    // PR 413 is BEHIND, but its base is PR 412's head: the fire is held.
    let heads = trigger_fire_heads_for_pr(&db, &owner, trigger, child, 413)
        .await
        .unwrap();
    assert!(heads.is_empty(), "a stacked child must not fire behind");

    // The watch-side lookup resolves the same parent from the fact set.
    let digest = PullRequestDigest {
        base_branch: Some("tidebreak/tracked".into()),
        ..open_pr_digest(413)
    };
    assert_eq!(
        crate::code::watch::stacked_parent_number(&runtime, &owner, child, &digest).await,
        Some(412)
    );
    let rooted = PullRequestDigest {
        base_branch: Some("main".into()),
        ..open_pr_digest(413)
    };
    assert_eq!(
        crate::code::watch::stacked_parent_number(&runtime, &owner, child, &rooted).await,
        None
    );
}
