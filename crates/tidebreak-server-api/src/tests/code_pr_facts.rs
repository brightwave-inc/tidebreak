//! Post-turn pull-request fact detection against a shimmed `gh` (decision 77).
//!
//! These tests drive `pr_facts::sweep_turn_for_pull_request_acts` directly
//! over a seeded journal: the acts are journaled shell commands, the host is
//! a `#!/bin/sh` shim on the gh search path, and the assertions are fact and
//! attribution rows. No HTTP surface is involved.

use super::*;

use std::os::unix::fs::PermissionsExt;

use tidebreak_core::db::code::{
    append_event, get_pull_request_fact, get_turn, insert_repo, insert_session, insert_turn,
    insert_workspace, list_attributed_facts_for_workspace, list_pull_request_attributions,
    save_turn,
};
use tidebreak_core::{
    Attention, AttentionSource, CodePullRequestRelation, CodePullRequestState, CodeRepo,
    CodeWorkspace, CodeWorkspaceStatus, Diffstat, Event, HarnessKind, OwnerId, PermissionMode,
    RepoId, Session, SessionId, SessionKind, SessionLifecycle, ToolDetail, ToolOutcome, Turn,
    TurnId, TurnStatus, WorkspaceId,
};

use crate::code::pr_facts;

const VIEW_JSON: &str = r#"{"number":412,"url":"https://github.com/acme/tools/pull/412","title":"Add fact tracking","state":"OPEN","isDraft":false,"author":{"login":"octocat"},"headRefName":"feat/x","headRefOid":"aaa111","baseRefName":"main","createdAt":"2026-08-22T10:00:00Z","updatedAt":"2026-08-22T11:00:00Z","mergedAt":null,"closedAt":null}"#;

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

/// A checkout whose origin names a GitHub repository. Never contacted: git
/// only answers `remote get-url` and `rev-parse` here.
fn init_github_shaped_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["git", "init", "-b", "feat/x"]);
    run(&work, &["git", "config", "user.email", "dev@example.com"]);
    run(&work, &["git", "config", "user.name", "Dev"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    run(&work, &["git", "add", "README.md"]);
    run(&work, &["git", "commit", "-m", "init"]);
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

/// A gh shim that answers auth, one view, and one head-scoped list, and logs
/// every invocation. Anything else fails loudly.
fn write_gh_shim(dir: &std::path::Path, log: &std::path::Path) {
    write_gh_shim_with_view(dir, log, VIEW_JSON);
}

fn write_gh_shim_with_view(dir: &std::path::Path, log: &std::path::Path, view_json: &str) {
    let body = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> {log}\n\
         if [ \"$1\" = auth ]; then\n\
           echo '{{\"hosts\":{{\"github.com\":[{{\"active\":true,\"state\":\"success\",\"login\":\"tester\"}}]}}}}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n\
           echo '{view_json}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = pr ] && [ \"$2\" = list ]; then\n\
           echo '[{view_json}]'\n\
           exit 0\n\
         fi\n\
         echo unexpected \"$@\" >&2\n\
         exit 3\n",
        log = log.display(),
    );
    write_executable(&dir.join("gh"), &body);
}

async fn mark_turn_checkout_changed(
    store: &tidebreak_core::DbStore,
    session: &Session,
    turn_id: TurnId,
) {
    let mut turn = get_turn(store, &session.owner, turn_id)
        .await
        .unwrap()
        .unwrap();
    turn.checkpoint_ref = Some("refs/tidebreak/checkpoints/test".into());
    turn.diffstat = Some(Diffstat {
        files: 1,
        insertions: 1,
        deletions: 0,
        truncated: false,
    });
    save_turn(store, &session.owner, &turn).await.unwrap();
}

async fn seeded(store: &tidebreak_core::DbStore, worktree: &std::path::Path) -> (Session, TurnId) {
    let owner = OwnerId::local();
    let repo_id = RepoId::new();
    insert_repo(
        store,
        &CodeRepo {
            id: repo_id,
            owner: owner.clone(),
            root_path: worktree.display().to_string(),
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
    let workspace_id = WorkspaceId::new();
    insert_workspace(
        store,
        &CodeWorkspace {
            id: workspace_id,
            owner: owner.clone(),
            repo_id,
            title: "facts".into(),
            worktree_path: worktree.display().to_string(),
            branch_name: "feat/x".into(),
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
    let session = Session {
        id: SessionId::new(),
        owner: owner.clone(),
        workspace_id: Some(workspace_id),
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: Some("2.1.233".into()),
        harness_resume_ref: None,
        permission_mode: PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 0,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        visibility: tidebreak_core::SessionVisibility::Private,
        created_at: chrono::Utc::now(),
        execution_location: tidebreak_core::ExecutionLocation::Machine,
    };
    insert_session(store, &session).await.unwrap();
    let turn_id = TurnId::new();
    insert_turn(
        store,
        &owner,
        &Turn {
            actor: None,
            id: turn_id,
            session_id: session.id,
            ordinal: 1,
            status: TurnStatus::Completed,
            model: None,
            fast_mode: false,
            user_input: "ship it".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            rewrite: None,
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
            park_ref: None,
            park_wait: None,
        },
    )
    .await
    .unwrap();
    append_event(
        store,
        &owner,
        session.id,
        0,
        &Event::TurnStarted { turn_id },
    )
    .await
    .unwrap();
    (session, turn_id)
}

#[allow(clippy::too_many_arguments)]
async fn journal_command(
    store: &tidebreak_core::DbStore,
    session: &Session,
    call_id: &str,
    cmd: &str,
    cwd: &std::path::Path,
    outcome: ToolOutcome,
    preview: &str,
    parent_call_id: Option<&str>,
) {
    append_event(
        store,
        &session.owner,
        session.id,
        0,
        &Event::ToolStarted {
            call_id: call_id.into(),
            name: "Bash".into(),
            detail: ToolDetail::Command {
                cmd: cmd.into(),
                cwd: cwd.display().to_string(),
            },
            parent_call_id: parent_call_id.map(str::to_owned),
        },
    )
    .await
    .unwrap();
    append_event(
        store,
        &session.owner,
        session.id,
        0,
        &Event::ToolCompleted {
            call_id: call_id.into(),
            outcome,
            preview: preview.into(),
            output: None,
            action: None,
            result: None,
            detail: None,
            parent_call_id: parent_call_id.map(str::to_owned),
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_journaled_create_mints_an_authored_fact() {
    let (dir, store) = temp_db_store("pr-facts-create.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim, &dir.path().join("gh.log"));
    let (session, turn_id) = seeded(&store, &work).await;

    journal_command(
        &store,
        &session,
        "call-1",
        "gh pr create --fill",
        &work,
        ToolOutcome::Succeeded,
        "https://github.com/acme/tools/pull/412",
        Some("task-9"),
    )
    .await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    let owner = OwnerId::local();
    let fact = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .expect("the confirmed create mints a fact");
    assert_eq!(fact.state, CodePullRequestState::Open);
    assert_eq!(fact.head_branch, "feat/x");
    let first_seen = fact.first_seen_at;

    let attributed = list_attributed_facts_for_workspace(
        &store,
        &owner,
        session.workspace_id.expect("workspace"),
    )
    .await
    .unwrap();
    assert_eq!(attributed.len(), 1);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Authored);

    // Re-running the sweep is idempotent: one row, first_seen_at holds.
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;
    let attributed = list_attributed_facts_for_workspace(
        &store,
        &owner,
        session.workspace_id.expect("workspace"),
    )
    .await
    .unwrap();
    assert_eq!(attributed.len(), 1);
    let fact = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fact.first_seen_at, first_seen);
}

#[tokio::test]
async fn a_journaled_push_mints_a_contributed_fact() {
    let (dir, store) = temp_db_store("pr-facts-push.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim, &dir.path().join("gh.log"));
    let (session, turn_id) = seeded(&store, &work).await;

    journal_command(
        &store,
        &session,
        "call-1",
        "git push -u origin feat/x",
        &work,
        ToolOutcome::Succeeded,
        "branch pushed",
        None,
    )
    .await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    let owner = OwnerId::local();
    let attributed = list_attributed_facts_for_workspace(
        &store,
        &owner,
        session.workspace_id.expect("workspace"),
    )
    .await
    .unwrap();
    assert_eq!(attributed.len(), 1);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Contributed);
    assert_eq!(attributed[0].0.number, 412);
}

#[tokio::test]
async fn a_confirmed_push_marks_the_workspace_hot() {
    // The agent's own push moves the head, and nothing else dirties the row
    // for it: route mutations cover the user's actions, not the engine's.
    // Left unmarked, the watch reads a pre-push head, calls its own fix turn
    // a repeat, and parks (issue 2799).
    let (dir, store) = temp_db_store("pr-facts-hot.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim, &dir.path().join("gh.log"));
    let (session, turn_id) = seeded(&store, &work).await;

    journal_command(
        &store,
        &session,
        "call-1",
        "git push -u origin feat/x",
        &work,
        ToolOutcome::Succeeded,
        "branch pushed",
        None,
    )
    .await;

    let hot = crate::code::pr_refresh::HotPullRequests::default();
    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(
        &store,
        &session,
        turn_id,
        Some(&search_path),
        Some(&hot),
    )
    .await;

    assert_eq!(
        hot.live(),
        vec![(OwnerId::local(), session.workspace_id.expect("workspace"))],
        "the confirmed push puts the workspace on the hot refresh tier"
    );
}

#[tokio::test]
async fn a_turn_that_pushed_nothing_leaves_the_hot_tier_alone() {
    let (dir, store) = temp_db_store("pr-facts-not-hot.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim, &dir.path().join("gh.log"));
    let (session, turn_id) = seeded(&store, &work).await;

    journal_command(
        &store,
        &session,
        "call-1",
        "gh pr view 412",
        &work,
        ToolOutcome::Succeeded,
        "done",
        None,
    )
    .await;

    let hot = crate::code::pr_refresh::HotPullRequests::default();
    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(
        &store,
        &session,
        turn_id,
        Some(&search_path),
        Some(&hot),
    )
    .await;

    assert!(hot.live().is_empty());
}

#[tokio::test]
async fn a_changed_clean_checkout_recovers_an_open_pull_request() {
    let (dir, store) = temp_db_store("pr-facts-checkout.db").await;
    let work = init_github_shaped_repo(dir.path());
    let (session, turn_id) = seeded(&store, &work).await;

    std::fs::write(work.join("changed.txt"), "from the turn\n").unwrap();
    run(&work, &["git", "add", "changed.txt"]);
    run(&work, &["git", "commit", "-m", "change from turn"]);
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap();
    let head = head.trim();

    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    let view = VIEW_JSON.replace("aaa111", head);
    write_gh_shim_with_view(&shim, &dir.path().join("gh.log"), &view);
    mark_turn_checkout_changed(&store, &session, turn_id).await;

    let hot = crate::code::pr_refresh::HotPullRequests::default();
    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(
        &store,
        &session,
        turn_id,
        Some(&search_path),
        Some(&hot),
    )
    .await;

    let owner = OwnerId::local();
    let attributed = list_attributed_facts_for_workspace(
        &store,
        &owner,
        session.workspace_id.expect("workspace"),
    )
    .await
    .unwrap();
    assert_eq!(
        attributed.len(),
        1,
        "gh calls:\n{}",
        std::fs::read_to_string(dir.path().join("gh.log")).unwrap_or_default()
    );
    assert_eq!(attributed[0].0.number, 412);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Contributed);
    let sources = list_pull_request_attributions(&store, &owner)
        .await
        .unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(
        sources[0].discovered_via,
        tidebreak_core::CodePullRequestDiscovery::Command
    );
    assert_eq!(
        hot.live(),
        vec![(owner, session.workspace_id.expect("workspace"))]
    );
}

#[tokio::test]
async fn a_changed_checkout_does_not_claim_a_different_pull_request_head() {
    let (dir, store) = temp_db_store("pr-facts-checkout-head.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_gh_shim(&shim, &dir.path().join("gh.log"));
    let (session, turn_id) = seeded(&store, &work).await;
    mark_turn_checkout_changed(&store, &session, turn_id).await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    assert!(list_attributed_facts_for_workspace(
        &store,
        &session.owner,
        session.workspace_id.expect("workspace")
    )
    .await
    .unwrap()
    .is_empty());
}

#[tokio::test]
async fn a_changed_dirty_checkout_does_not_claim_a_matching_pull_request_head() {
    let (dir, store) = temp_db_store("pr-facts-checkout-dirty.db").await;
    let work = init_github_shaped_repo(dir.path());
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap();
    let view = VIEW_JSON.replace("aaa111", head.trim());

    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    let log = dir.path().join("gh.log");
    write_gh_shim_with_view(&shim, &log, &view);
    let (session, turn_id) = seeded(&store, &work).await;
    mark_turn_checkout_changed(&store, &session, turn_id).await;
    std::fs::write(work.join("unpushed.txt"), "not on the pull request\n").unwrap();

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    assert!(list_attributed_facts_for_workspace(
        &store,
        &session.owner,
        session.workspace_id.expect("workspace")
    )
    .await
    .unwrap()
    .is_empty());
    assert!(!log.exists(), "a dirty checkout should not reach GitHub");
}

#[tokio::test]
async fn a_changed_checkout_on_another_branch_does_not_claim_its_pull_request() {
    let (dir, store) = temp_db_store("pr-facts-checkout-branch.db").await;
    let work = init_github_shaped_repo(dir.path());
    let (session, turn_id) = seeded(&store, &work).await;
    run(&work, &["git", "checkout", "-b", "review-branch"]);
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&work)
        .output()
        .unwrap();
    assert!(head.status.success());
    let head = String::from_utf8(head.stdout).unwrap();
    let view = VIEW_JSON
        .replace("feat/x", "review-branch")
        .replace("aaa111", head.trim());

    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    let log = dir.path().join("gh.log");
    write_gh_shim_with_view(&shim, &log, &view);
    mark_turn_checkout_changed(&store, &session, turn_id).await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    assert!(list_attributed_facts_for_workspace(
        &store,
        &session.owner,
        session.workspace_id.expect("workspace")
    )
    .await
    .unwrap()
    .is_empty());
    assert!(
        !log.exists(),
        "a checkout on another branch should not reach GitHub"
    );
}

#[tokio::test]
async fn reads_comments_and_failures_mint_nothing() {
    let (dir, store) = temp_db_store("pr-facts-quiet.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    let log = dir.path().join("gh.log");
    write_gh_shim(&shim, &log);
    let (session, turn_id) = seeded(&store, &work).await;

    for (call_id, cmd) in [
        ("call-1", "gh pr view 412"),
        ("call-2", "gh pr comment 412 --body looks-good"),
        ("call-3", "gh pr checkout 412"),
        ("call-4", "gh pr close 412"),
    ] {
        journal_command(
            &store,
            &session,
            call_id,
            cmd,
            &work,
            ToolOutcome::Succeeded,
            "done",
            None,
        )
        .await;
    }
    // A create the engine reported as failed is never confirmed, even though
    // the shim would answer for its branch.
    journal_command(
        &store,
        &session,
        "call-5",
        "gh pr create --fill",
        &work,
        ToolOutcome::Failed,
        "error: draft pull requests are not supported",
        None,
    )
    .await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    let owner = OwnerId::local();
    assert!(list_attributed_facts_for_workspace(
        &store,
        &owner,
        session.workspace_id.expect("workspace")
    )
    .await
    .unwrap()
    .is_empty());
    assert!(
        get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .is_none()
    );
    // The detector never reached the host at all.
    assert!(
        !log.exists(),
        "gh was invoked: {:?}",
        std::fs::read_to_string(&log)
    );
}

#[tokio::test]
async fn a_signed_out_gh_mints_nothing() {
    let (dir, store) = temp_db_store("pr-facts-signed-out.db").await;
    let work = init_github_shaped_repo(dir.path());
    let shim = dir.path().join("bin");
    std::fs::create_dir_all(&shim).unwrap();
    write_executable(
        &shim.join("gh"),
        "#!/bin/sh\nif [ \"$1\" = auth ]; then echo signed out >&2; exit 1; fi\nexit 1\n",
    );
    let (session, turn_id) = seeded(&store, &work).await;

    journal_command(
        &store,
        &session,
        "call-1",
        "gh pr create --fill",
        &work,
        ToolOutcome::Succeeded,
        "https://github.com/acme/tools/pull/412",
        None,
    )
    .await;

    let search_path = shim.display().to_string();
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path), None)
        .await;

    let owner = OwnerId::local();
    assert!(
        get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .is_none()
    );
}
