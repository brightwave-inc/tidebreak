//! Hosted deployments refuse host scripts and terminals before side effects.

use super::code::{init_git_repo, serve};
use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{insert_repo, list_workspaces, save_repo, save_workspace};
use tidebreak_core::{CodeRepo, CodeWorkspace, CodeWorkspaceStatus, OwnerId, QuickAction, RepoId};
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
    let runtime = Arc::new(runtime.with_sandbox_only_execution());
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

async fn assert_hosted_refusal(response: reqwest::Response) {
    assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["kind"], "sandbox_session_required");
}

#[tokio::test]
async fn sandbox_only_workspace_lifecycle_refuses_before_scripts_or_checkout_changes() {
    let (state, runtime, repo, mut workspace, _dir) = hosted_fixture().await;
    let token = state.token.clone();
    let addr = serve(app(state)).await;
    let client = reqwest::Client::new();
    let path = std::path::PathBuf::from(&workspace.worktree_path);
    let marker = path.join("hosted-script-ran");
    let rows_before = list_workspaces(&runtime.db, &workspace.owner, Some(repo.id))
        .await
        .unwrap();
    assert_hosted_refusal(
        client
            .post(format!("http://{addr}/code/workspaces"))
            .bearer_auth(&token)
            .json(&serde_json::json!({"repo_id": repo.id, "title": "forbidden"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        list_workspaces(&runtime.db, &workspace.owner, Some(repo.id))
            .await
            .unwrap()
            .len(),
        rows_before.len()
    );
    for action in ["archive", "actions/probe"] {
        assert_hosted_refusal(
            client
                .post(format!(
                    "http://{addr}/code/workspaces/{}/{action}",
                    workspace.id
                ))
                .bearer_auth(&token)
                .json(&serde_json::json!({"force": true}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            runtime
                .get_workspace(&workspace.owner, workspace.id)
                .await
                .unwrap()
                .status,
            CodeWorkspaceStatus::Active
        );
        assert!(path.join("README.md").exists());
        assert!(!marker.exists());
    }
    workspace.status = CodeWorkspaceStatus::SetupFailed;
    save_workspace(&runtime.db, &workspace).await.unwrap();
    assert_hosted_refusal(
        client
            .post(format!(
                "http://{addr}/code/workspaces/{}/retry-setup",
                workspace.id
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        runtime
            .get_workspace(&workspace.owner, workspace.id)
            .await
            .unwrap()
            .status,
        CodeWorkspaceStatus::SetupFailed
    );
    assert!(!marker.exists());

    assert!(std::process::Command::new("git")
        .args(["worktree", "remove", "--force", &workspace.worktree_path])
        .current_dir(&repo.root_path)
        .status()
        .unwrap()
        .success());
    workspace.status = CodeWorkspaceStatus::Archived;
    workspace.archived_at = Some(chrono::Utc::now());
    save_workspace(&runtime.db, &workspace).await.unwrap();
    assert_hosted_refusal(
        client
            .post(format!(
                "http://{addr}/code/workspaces/{}/restore",
                workspace.id
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        runtime
            .get_workspace(&workspace.owner, workspace.id)
            .await
            .unwrap()
            .status,
        CodeWorkspaceStatus::Archived
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn sandbox_only_terminals_refuse_execution_but_allow_read_and_close() {
    let (state, _runtime, _repo, workspace, _dir) = hosted_fixture().await;
    let token = state.token.clone();
    let terminal = state
        .terminals
        .open(
            &workspace.owner,
            workspace.id,
            std::path::Path::new(&workspace.worktree_path),
            Some(80),
            Some(24),
        )
        .unwrap();
    let addr = serve(app(state.clone())).await;
    let client = reqwest::Client::new();
    let base = format!("http://{addr}/code/workspaces/{}/terminals", workspace.id);
    assert_hosted_refusal(
        client
            .post(&base)
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(state.terminals.list(workspace.id).len(), 1);
    use base64::Engine;
    let bytes =
        base64::engine::general_purpose::STANDARD.encode("echo terminal > hosted-terminal-ran\n");
    assert_hosted_refusal(
        client
            .post(format!("{base}/{}/write", terminal.id))
            .bearer_auth(&token)
            .json(&serde_json::json!({"bytes": bytes}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_hosted_refusal(
        client
            .post(format!("{base}/{}/resize", terminal.id))
            .bearer_auth(&token)
            .json(&serde_json::json!({"cols": 100, "rows": 30}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let unchanged = state.terminals.list(workspace.id);
    assert_eq!((unchanged[0].cols, unchanged[0].rows), (80, 24));
    assert_eq!(
        client
            .get(format!("{base}/{}/read?cursor=0", terminal.id))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .delete(format!("{base}/{}", terminal.id))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert!(state.terminals.close_workspace_and_wait(workspace.id).await);
    assert!(!std::path::Path::new(&workspace.worktree_path)
        .join("hosted-terminal-ran")
        .exists());
}

#[tokio::test]
async fn sandbox_only_remote_archive_stays_available_without_host_restore_or_retry() {
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
    for action in ["restore", "retry-setup"] {
        let response = client
            .post(format!("{base}/{action}"))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT);
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["kind"], "workspace_remote");
    }
    assert!(!std::path::Path::new(&workspace.worktree_path).exists());
}
