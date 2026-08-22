//! Post-turn pull-request fact detection against a shimmed `gh` (decision 62).
//!
//! These tests drive `pr_facts::sweep_turn_for_pull_request_acts` directly
//! over a seeded journal: the acts are journaled shell commands, the host is
//! a `#!/bin/sh` shim on the gh search path, and the assertions are fact and
//! attribution rows. No HTTP surface is involved.

use super::*;

use std::os::unix::fs::PermissionsExt;

use tidebreak_core::db::code::{
    append_event, get_pull_request_fact, insert_repo, insert_session, insert_turn,
    insert_workspace, list_attributed_facts_for_workspace,
};
use tidebreak_core::{
    Attention, AttentionSource, CodeEvent, CodePullRequestRelation, CodePullRequestState, CodeRepo,
    CodeSession, CodeSessionId, CodeSessionKind, CodeSessionLifecycle, CodeTurn, CodeTurnId,
    CodeTurnStatus, CodeWorkspace, CodeWorkspaceStatus, HarnessKind, OwnerId, PermissionMode,
    RepoId, ToolDetail, ToolOutcome, WorkspaceId,
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
    let body = format!(
        "#!/bin/sh\n\
         echo \"$@\" >> {log}\n\
         if [ \"$1\" = auth ]; then echo logged in; exit 0; fi\n\
         if [ \"$1\" = pr ] && [ \"$2\" = view ]; then\n\
           echo '{VIEW_JSON}'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = pr ] && [ \"$2\" = list ]; then\n\
           echo '[{VIEW_JSON}]'\n\
           exit 0\n\
         fi\n\
         echo unexpected \"$@\" >&2\n\
         exit 3\n",
        log = log.display(),
    );
    write_executable(&dir.join("gh"), &body);
}

async fn seeded(
    store: &tidebreak_core::DbStore,
    worktree: &std::path::Path,
) -> (CodeSession, CodeTurnId) {
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
    let session = CodeSession {
        id: CodeSessionId::new(),
        owner: owner.clone(),
        workspace_id,
        kind: CodeSessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: Some("2.1.233".into()),
        harness_resume_ref: None,
        permission_mode: PermissionMode::Ask,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: CodeSessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        spawn_epoch: 0,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at: chrono::Utc::now(),
    };
    insert_session(store, &session).await.unwrap();
    let turn_id = CodeTurnId::new();
    insert_turn(
        store,
        &owner,
        &CodeTurn {
            id: turn_id,
            session_id: session.id,
            ordinal: 1,
            status: CodeTurnStatus::Completed,
            user_input: "ship it".into(),
            user_input_blob_id: None,
            attachments: Vec::new(),
            checkpoint_ref: None,
            diffstat: None,
            usage: None,
            narrative: None,
            started_at: chrono::Utc::now(),
            ended_at: Some(chrono::Utc::now()),
        },
    )
    .await
    .unwrap();
    append_event(
        store,
        &owner,
        session.id,
        0,
        &CodeEvent::TurnStarted { turn_id },
    )
    .await
    .unwrap();
    (session, turn_id)
}

async fn journal_command(
    store: &tidebreak_core::DbStore,
    session: &CodeSession,
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
        &CodeEvent::ToolStarted {
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
        &CodeEvent::ToolCompleted {
            call_id: call_id.into(),
            outcome,
            preview: preview.into(),
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
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path)).await;

    let owner = OwnerId::local();
    let fact = get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
        .await
        .unwrap()
        .expect("the confirmed create mints a fact");
    assert_eq!(fact.state, CodePullRequestState::Open);
    assert_eq!(fact.head_branch, "feat/x");
    let first_seen = fact.first_seen_at;

    let attributed = list_attributed_facts_for_workspace(&store, &owner, session.workspace_id)
        .await
        .unwrap();
    assert_eq!(attributed.len(), 1);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Authored);

    // Re-running the sweep is idempotent: one row, first_seen_at holds.
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path)).await;
    let attributed = list_attributed_facts_for_workspace(&store, &owner, session.workspace_id)
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
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path)).await;

    let owner = OwnerId::local();
    let attributed = list_attributed_facts_for_workspace(&store, &owner, session.workspace_id)
        .await
        .unwrap();
    assert_eq!(attributed.len(), 1);
    assert_eq!(attributed[0].1, CodePullRequestRelation::Contributed);
    assert_eq!(attributed[0].0.number, 412);
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
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path)).await;

    let owner = OwnerId::local();
    assert!(
        list_attributed_facts_for_workspace(&store, &owner, session.workspace_id)
            .await
            .unwrap()
            .is_empty()
    );
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
    pr_facts::sweep_turn_for_pull_request_acts(&store, &session, turn_id, Some(&search_path)).await;

    let owner = OwnerId::local();
    assert!(
        get_pull_request_fact(&store, &owner, "github.com", "acme", "tools", 412)
            .await
            .unwrap()
            .is_none()
    );
}
