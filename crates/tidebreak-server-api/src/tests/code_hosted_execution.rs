//! Remote workspaces refuse host restore and retry; machine workspaces stay on the host.

use super::code::{init_git_repo, serve};
use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::db::code::{insert_repo, save_repo, save_workspace};
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
