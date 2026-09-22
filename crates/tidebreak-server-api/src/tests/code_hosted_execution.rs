//! Remote workspaces inspect retained checkpoints and restore without a host checkout.

use super::code::{init_git_repo, serve};
use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{
    activate_incarnation, create_incarnation_intent, ingest_incarnation_event, insert_repo,
    insert_session, save_repo, save_workspace, stop_incarnation, IncarnationSideEffects,
};
use tidebreak_core::{
    Attention, AttentionSource, CodeRepo, CodeWorkspace, CodeWorkspaceStatus, Event,
    ExecutionLocation, HarnessKind, HarnessNoticeLevel, IncarnationAdmission, OwnerId,
    PermissionMode, QuickAction, RepoId, Session, SessionId, SessionKind, SessionLifecycle, TurnId,
};
use tidebreak_harness::AdapterRegistry;

async fn hosted_fixture() -> (
    AppState,
    Arc<CodeRuntime>,
    CodeRepo,
    CodeWorkspace,
    tempfile::TempDir,
) {
    hosted_fixture_lending(None).await
}

async fn hosted_fixture_lending(
    lender: Option<Arc<dyn crate::obo_gateway::GitCredentialLender>>,
) -> (
    AppState,
    Arc<CodeRuntime>,
    CodeRepo,
    CodeWorkspace,
    tempfile::TempDir,
) {
    let (dir, db) = temp_db_store("hosted-execution.db").await;
    let db = Arc::new(db);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let mut runtime = CodeRuntime::with_registry(db.clone(), dir.path().to_path_buf(), registry);
    if let Some(lender) = lender {
        runtime = runtime.with_git_credentials(lender);
    }
    let owner = OwnerId::local();
    let mut repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: init_git_repo(dir.path()).display().to_string(),
        display_name: "hosted".into(),
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
    };
    insert_repo(&db, &repo).await.unwrap();
    let workspace = runtime
        .create_workspace(&owner, repo.id, Some("existing".into()), None, None)
        .await
        .unwrap();
    repo.setup_script = Some("echo setup > hosted-script-ran".into());
    repo.archive_script = Some("echo archive > hosted-script-ran".into());
    repo.quick_actions = vec![QuickAction {
        name: "probe".into(),
        command: "echo action > hosted-script-ran".into(),
        auto_run_on_create: true,
    }];
    save_repo(&db, &repo).await.unwrap();
    let runtime = Arc::new(runtime);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        db,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(runtime.clone());
    (state, runtime, repo, workspace, dir)
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_only_remote_archive_refuses_restore_without_a_checkpoint() {
    let (state, runtime, _repo, mut workspace, _dir) = hosted_fixture().await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();
    let token = state.token.clone();
    let addr = serve(app(state)).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/code/workspaces/{}", workspace.id);
    let archived = client
        .post(format!("{base}/archive"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(archived.status(), reqwest::StatusCode::OK);
    assert_eq!(
        runtime
            .get_workspace(&workspace.owner, workspace.id)
            .await
            .unwrap()
            .status,
        CodeWorkspaceStatus::Archived
    );
    let restore = client
        .post(format!("{base}/restore"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(restore.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = restore.json().await.unwrap();
    assert_eq!(body["kind"], "sandbox_checkpoint_missing");
    let retry = client
        .post(format!("{base}/retry-setup"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(retry.status(), reqwest::StatusCode::CONFLICT);
    let retry_body: serde_json::Value = retry.json().await.unwrap();
    assert_eq!(retry_body["kind"], "workspace_remote");
    assert!(!std::path::Path::new(&workspace.worktree_path).exists());
    assert_eq!(
        runtime
            .get_workspace(&workspace.owner, workspace.id)
            .await
            .unwrap()
            .status,
        CodeWorkspaceStatus::Archived
    );
}

fn git(repo: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap()
            .success(),
        "{args:?}"
    );
}

fn write_wip_ref(repo_root: &std::path::Path, file: &str, contents: &str, git_ref: &str) {
    std::fs::write(repo_root.join(file), contents).unwrap();
    git(repo_root, &["add", file]);
    git(repo_root, &["commit", "-m", git_ref]);
    git(
        repo_root,
        &["update-ref", &format!("refs/heads/{git_ref}"), "HEAD"],
    );
    git(repo_root, &["reset", "--hard", "HEAD~1"]);
}

async fn seed_retained_checkpoint(
    runtime: &CodeRuntime,
    workspace: &CodeWorkspace,
    repo_root: &std::path::Path,
) -> SessionId {
    write_wip_ref(
        repo_root,
        "sandbox.txt",
        "from the checkpoint\n",
        "mg-wip/sb-1-i1",
    );
    seed_session_checkpoint(
        runtime,
        workspace,
        "mg-wip/sb-1-i1",
        "sb-1",
        chrono::Utc::now(),
    )
    .await
}

async fn seed_session_checkpoint(
    runtime: &CodeRuntime,
    workspace: &CodeWorkspace,
    wip_ref: &str,
    sandbox_id: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> SessionId {
    let (session_id, row) = seed_running_checkpoint(
        runtime,
        workspace,
        Some(wip_ref),
        None,
        sandbox_id,
        created_at,
    )
    .await;
    stop_incarnation(&runtime.db, &workspace.owner, row, Some("completed"))
        .await
        .unwrap();
    session_id
}

/// A session whose sandbox is still running, optionally after one checkpoint.
async fn seed_running_checkpoint(
    runtime: &CodeRuntime,
    workspace: &CodeWorkspace,
    wip_ref: Option<&str>,
    wip_at: Option<chrono::DateTime<chrono::Utc>>,
    sandbox_id: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> (SessionId, tidebreak_core::CodeIncarnationId) {
    let session = Session {
        visibility: tidebreak_core::SessionVisibility::Private,
        id: SessionId::new(),
        owner: workspace.owner.clone(),
        owner_kind: None,
        workspace_id: Some(workspace.id),
        kind: SessionKind::Interactive,
        harness_kind: HarnessKind::ClaudeCode,
        harness_version: None,
        harness_resume_ref: None,
        permission_mode: PermissionMode::Allow,
        model: None,
        reasoning_effort: None,
        fast_mode: false,
        lifecycle: SessionLifecycle::Idle,
        fence_reason: None,
        child_pid: None,
        child_process_identity: None,
        spawn_epoch: 1,
        attention: Attention::working(AttentionSource::Lifecycle),
        unrecognized_event_count: 0,
        subagents: Vec::new(),
        created_at,
        execution_location: ExecutionLocation::Sandbox,
        acts_as: None,
    };
    insert_session(&runtime.db, &session).await.unwrap();
    let IncarnationAdmission::Admitted(row) =
        create_incarnation_intent(&runtime.db, &workspace.owner, session.id, 1, 4)
            .await
            .unwrap()
    else {
        panic!("expected incarnation admission");
    };
    activate_incarnation(&runtime.db, &workspace.owner, row.id, sandbox_id)
        .await
        .unwrap();
    if let Some(wip_ref) = wip_ref {
        push_checkpoint(runtime, workspace, session.id, row.id, 1, wip_ref, wip_at).await;
    }
    (session.id, row.id)
}

async fn push_checkpoint(
    runtime: &CodeRuntime,
    workspace: &CodeWorkspace,
    session_id: SessionId,
    incarnation: tidebreak_core::CodeIncarnationId,
    seq: i64,
    wip_ref: &str,
    wip_at: Option<chrono::DateTime<chrono::Utc>>,
) {
    let notice = Event::HarnessNotice {
        level: HarnessNoticeLevel::Info,
        message: "checkpointed".into(),
    };
    ingest_incarnation_event(
        &runtime.db,
        &workspace.owner,
        session_id,
        1,
        incarnation,
        seq,
        IncarnationSideEffects {
            journal: std::slice::from_ref(&notice),
            wip_ref: Some(wip_ref),
            wip_at,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn remote_workspace_inspects_and_restores_from_a_retained_checkpoint() {
    let (_state, runtime, repo, mut workspace, _dir) = hosted_fixture().await;
    let repo_root = std::path::PathBuf::from(&repo.root_path);
    let session_id = seed_retained_checkpoint(&runtime, &workspace, &repo_root).await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();
    assert!(!std::path::Path::new(&workspace.worktree_path).exists());

    let (paths, _, source) = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap();
    assert!(paths.iter().any(|path| path == "sandbox.txt"));
    let source = source.expect("retained revision");
    assert_eq!(
        source.revision,
        crate::code::types::WorkspaceContentRevision::Retained
    );
    assert_eq!(source.revision_ref.as_deref(), Some("mg-wip/sb-1-i1"));

    let (blob, _) = runtime
        .workspace_blob(&workspace.owner, workspace.id, "sandbox.txt")
        .await
        .unwrap();
    assert_eq!(blob.content, "from the checkpoint\n");

    let escaped = runtime
        .workspace_blob(&workspace.owner, workspace.id, "../README.md")
        .await
        .unwrap_err();
    assert_eq!(escaped.kind(), "path");

    let (files, _, _, _, _) = runtime
        .workspace_files(&workspace.owner, workspace.id, None)
        .await
        .unwrap();
    assert!(files
        .iter()
        .any(|file| file.path.to_wire() == "sandbox.txt"));

    let (diff, _, _, _, _) = runtime
        .workspace_diff(&workspace.owner, workspace.id, None, Some("sandbox.txt"))
        .await
        .unwrap();
    assert!(diff.contains("from the checkpoint"));

    let mut session = runtime
        .get_session(&workspace.owner, session_id)
        .await
        .unwrap();
    session.lifecycle = SessionLifecycle::Ended;
    tidebreak_core::db::code::save_session(&runtime.db, &session)
        .await
        .unwrap();
    workspace.status = CodeWorkspaceStatus::Archived;
    workspace.archived_at = Some(chrono::Utc::now());
    save_workspace(&runtime.db, &workspace).await.unwrap();

    let restored = runtime
        .restore_workspace(&workspace.owner, workspace.id)
        .await
        .unwrap();
    assert_eq!(restored.status, CodeWorkspaceStatus::Active);
    assert!(restored.worktree_path.starts_with("remote:"));
    assert!(!std::path::Path::new(&restored.worktree_path).exists());
    let session = runtime
        .get_session(&workspace.owner, session_id)
        .await
        .unwrap();
    assert_eq!(session.lifecycle, SessionLifecycle::Idle);
    assert_eq!(session.id, session_id);
    assert_eq!(
        tidebreak_core::db::code::latest_pushed_wip_ref(&runtime.db, &workspace.owner, session_id)
            .await
            .unwrap()
            .as_deref(),
        Some("mg-wip/sb-1-i1")
    );
    let sessions = tidebreak_core::db::code::list_sessions_for_workspace(
        &runtime.db,
        &workspace.owner,
        workspace.id,
    )
    .await
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    // Restore is idempotent and does not mint a second session.
    let restored_again = runtime
        .restore_workspace(&workspace.owner, workspace.id)
        .await
        .unwrap();
    assert_eq!(restored_again.id, workspace.id);
    let sessions = tidebreak_core::db::code::list_sessions_for_workspace(
        &runtime.db,
        &workspace.owner,
        workspace.id,
    )
    .await
    .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, session_id);
    assert_eq!(sessions[0].lifecycle, SessionLifecycle::Idle);
}

#[tokio::test]
async fn remote_inspect_uses_the_newest_session_checkpoint() {
    let (_state, runtime, repo, mut workspace, _dir) = hosted_fixture().await;
    let repo_root = std::path::PathBuf::from(&repo.root_path);
    write_wip_ref(
        &repo_root,
        "old.txt",
        "from the older session\n",
        "mg-wip/old-i1",
    );
    write_wip_ref(
        &repo_root,
        "new.txt",
        "from the newer session\n",
        "mg-wip/new-i1",
    );
    let older = chrono::Utc::now() - chrono::Duration::seconds(60);
    seed_session_checkpoint(&runtime, &workspace, "mg-wip/old-i1", "old", older).await;
    seed_session_checkpoint(
        &runtime,
        &workspace,
        "mg-wip/new-i1",
        "new",
        chrono::Utc::now(),
    )
    .await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();

    let (paths, _, source) = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap();
    let source = source.expect("retained revision");
    assert_eq!(source.revision_ref.as_deref(), Some("mg-wip/new-i1"));
    assert!(paths.iter().any(|path| path == "new.txt"));
    let (blob, _) = runtime
        .workspace_blob(&workspace.owner, workspace.id, "new.txt")
        .await
        .unwrap();
    assert_eq!(blob.content, "from the newer session\n");
}

#[tokio::test]
async fn remote_files_and_diff_reject_a_historical_turn() {
    let (_state, runtime, repo, mut workspace, _dir) = hosted_fixture().await;
    let repo_root = std::path::PathBuf::from(&repo.root_path);
    seed_retained_checkpoint(&runtime, &workspace, &repo_root).await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();
    let turn = TurnId::new();
    let files = runtime
        .workspace_files(&workspace.owner, workspace.id, Some(turn))
        .await
        .unwrap_err();
    assert_eq!(files.kind(), "sandbox_historical_turn_unsupported");
    let diff = runtime
        .workspace_diff(
            &workspace.owner,
            workspace.id,
            Some(turn),
            Some("sandbox.txt"),
        )
        .await
        .unwrap_err();
    assert_eq!(diff.kind(), "sandbox_historical_turn_unsupported");
}

#[tokio::test]
async fn restore_refuses_a_checkpoint_string_that_is_not_recoverable() {
    let (_state, runtime, _repo, mut workspace, _dir) = hosted_fixture().await;
    seed_session_checkpoint(
        &runtime,
        &workspace,
        "mg-wip/ghost-i1",
        "ghost",
        chrono::Utc::now(),
    )
    .await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    workspace.status = CodeWorkspaceStatus::Archived;
    workspace.archived_at = Some(chrono::Utc::now());
    save_workspace(&runtime.db, &workspace).await.unwrap();
    let error = runtime
        .restore_workspace(&workspace.owner, workspace.id)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "sandbox_checkpoint_missing");
    assert_eq!(
        runtime
            .get_workspace(&workspace.owner, workspace.id)
            .await
            .unwrap()
            .status,
        CodeWorkspaceStatus::Archived
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_sandbox_checkpoint_reads_as_live_until_the_sandbox_stops() {
    let (state, runtime, repo, mut workspace, _dir) = hosted_fixture().await;
    let repo_root = std::path::PathBuf::from(&repo.root_path);
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();

    // The turn has started and the first periodic push has not landed yet.
    let (session_id, incarnation) = seed_running_checkpoint(
        &runtime,
        &workspace,
        None,
        None,
        "sb-live",
        chrono::Utc::now(),
    )
    .await;
    let waiting = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap_err();
    assert_eq!(waiting.kind(), "workspace_sandbox_unavailable");
    assert!(
        waiting.message().contains("within about a minute"),
        "{}",
        waiting.message()
    );

    let saved_at: chrono::DateTime<chrono::Utc> = "2026-09-22T10:00:00Z".parse().unwrap();
    write_wip_ref(
        &repo_root,
        "live.txt",
        "periodic push\n",
        "mg-wip/sb-live-i1",
    );
    push_checkpoint(
        &runtime,
        &workspace,
        session_id,
        incarnation,
        1,
        "mg-wip/sb-live-i1",
        Some(saved_at),
    )
    .await;

    let token = state.token.clone();
    let addr = serve(app(state)).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/code/workspaces/{}", workspace.id);
    for route in ["tree", "files", "diff?file=live.txt", "blob?path=live.txt"] {
        let response = client
            .get(format!("{base}/{route}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{route}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["revision"], "live", "{route}");
        assert_eq!(body["revision_ref"], "mg-wip/sb-live-i1", "{route}");
        assert_eq!(
            body["revision_saved_at"]
                .as_str()
                .and_then(|value| value.parse::<chrono::DateTime<chrono::Utc>>().ok()),
            Some(saved_at),
            "{route}"
        );
    }

    // The sandbox retires; the same checkpoint is now retained work.
    stop_incarnation(
        &runtime.db,
        &workspace.owner,
        incarnation,
        Some("completed"),
    )
    .await
    .unwrap();
    let (_, _, source) = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap();
    let source = source.expect("retained revision");
    assert_eq!(
        source.revision,
        crate::code::types::WorkspaceContentRevision::Retained
    );
    assert_eq!(source.saved_at, Some(saved_at));

    // A later sandbox for the same session has not pushed yet. Its
    // predecessor's checkpoint is still retained work, not live work.
    let IncarnationAdmission::Admitted(next) =
        create_incarnation_intent(&runtime.db, &workspace.owner, session_id, 2, 4)
            .await
            .unwrap()
    else {
        panic!("expected incarnation admission");
    };
    activate_incarnation(&runtime.db, &workspace.owner, next.id, "sb-live-2")
        .await
        .unwrap();
    let (_, _, source) = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap();
    let source = source.expect("retained revision");
    assert_eq!(
        source.revision,
        crate::code::types::WorkspaceContentRevision::Retained
    );
    assert_eq!(source.revision_ref.as_deref(), Some("mg-wip/sb-live-i1"));
}

/// A private origin the host cannot fetch anonymously borrows one
/// repository credential from the workspace's lender, as the identity its
/// sessions act as, and still refuses when that fetch fails.
#[tokio::test]
async fn a_private_origin_borrows_the_workspace_credential_before_refusing() {
    use crate::obo_gateway::test_support::FakeLender;
    use crate::obo_gateway::{GitForgeAttributionRequest, GitForgeError};

    for (lender, asked) in [
        (
            FakeLender::offering("acme-ship[bot]"),
            vec![GitForgeAttributionRequest::Person],
        ),
        (
            FakeLender::refusing(GitForgeError::NotConnected { connect_url: None }),
            vec![
                GitForgeAttributionRequest::Person,
                GitForgeAttributionRequest::Installation,
            ],
        ),
    ] {
        let lender = Arc::new(lender);
        let (_state, runtime, repo, mut workspace, dir) =
            hosted_fixture_lending(Some(lender.clone())).await;
        tidebreak_core::db::code::set_repo_origin(
            &runtime.db,
            &repo.owner,
            repo.id,
            "github.com",
            "acme",
            "private",
        )
        .await
        .unwrap();
        let repo_root = std::path::PathBuf::from(&repo.root_path);
        seed_retained_checkpoint(&runtime, &workspace, &repo_root).await;
        workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
        save_workspace(&runtime.db, &workspace).await.unwrap();
        let refused = dir.path().join("refuses-anonymous.git");
        git(
            &repo_root,
            &["remote", "add", "origin", refused.to_str().unwrap()],
        );

        // Workspace creation already probed the forge identity.
        let probed = lender.asked().len();
        let error = runtime
            .workspace_tree(&workspace.owner, workspace.id, "", None)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "workspace_sandbox_unavailable");
        assert_eq!(lender.asked()[probed..], asked[..]);
        assert_eq!(
            lender.minted(),
            vec!["acme/private".to_owned(); asked.len()]
        );
    }
}

/// An origin the host can read anonymously never asks the lender.
#[tokio::test]
async fn a_readable_origin_does_not_borrow_a_credential() {
    use crate::obo_gateway::test_support::FakeLender;

    let lender = Arc::new(FakeLender::offering("acme-ship[bot]"));
    let (_state, runtime, repo, mut workspace, _dir) =
        hosted_fixture_lending(Some(lender.clone())).await;
    tidebreak_core::db::code::set_repo_origin(
        &runtime.db,
        &repo.owner,
        repo.id,
        "github.com",
        "acme",
        "public",
    )
    .await
    .unwrap();
    let repo_root = std::path::PathBuf::from(&repo.root_path);
    seed_retained_checkpoint(&runtime, &workspace, &repo_root).await;
    workspace.worktree_path = CodeWorkspace::remote_worktree_marker(workspace.id);
    save_workspace(&runtime.db, &workspace).await.unwrap();
    let readable = repo_root.to_str().unwrap().to_owned();
    git(&repo_root, &["remote", "add", "origin", &readable]);

    let (paths, _, _) = runtime
        .workspace_tree(&workspace.owner, workspace.id, "", None)
        .await
        .unwrap();
    assert!(paths.iter().any(|path| path == "sandbox.txt"));
    assert!(lender.minted().is_empty());
}
