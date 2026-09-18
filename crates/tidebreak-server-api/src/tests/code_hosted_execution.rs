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
    PermissionMode, QuickAction, RepoId, Session, SessionId, SessionKind, SessionLifecycle,
};
use tidebreak_harness::AdapterRegistry;

async fn hosted_fixture() -> (
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
    let runtime = CodeRuntime::with_registry(db.clone(), dir.path().to_path_buf(), registry);
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

async fn seed_retained_checkpoint(
    runtime: &CodeRuntime,
    workspace: &CodeWorkspace,
    repo_root: &std::path::Path,
) -> SessionId {
    std::fs::write(repo_root.join("sandbox.txt"), "from the checkpoint\n").unwrap();
    git(repo_root, &["add", "sandbox.txt"]);
    git(repo_root, &["commit", "-m", "sandbox wip"]);
    git(
        repo_root,
        &["update-ref", "refs/heads/mg-wip/sb-1-i1", "HEAD"],
    );
    git(repo_root, &["reset", "--hard", "HEAD~1"]);
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
        created_at: chrono::Utc::now(),
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
    activate_incarnation(&runtime.db, &workspace.owner, row.id, "sb-1")
        .await
        .unwrap();
    let notice = Event::HarnessNotice {
        level: HarnessNoticeLevel::Info,
        message: "checkpointed".into(),
    };
    ingest_incarnation_event(
        &runtime.db,
        &workspace.owner,
        session.id,
        1,
        row.id,
        1,
        IncarnationSideEffects {
            journal: std::slice::from_ref(&notice),
            wip_ref: Some("mg-wip/sb-1-i1"),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    stop_incarnation(&runtime.db, &workspace.owner, row.id, Some("completed"))
        .await
        .unwrap();
    session.id
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
}
