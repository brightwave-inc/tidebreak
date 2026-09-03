//! Owner scoping of code rows, routes, clones, and credentials.

use super::code::*;
use super::*;

use std::sync::Arc;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CodeRepo, CodeWorkspace, CodeWorkspaceStatus, RepoId, Store, WorkspaceId};
use tidebreak_harness::AdapterRegistry;

/// A self-host code app with two principals: alice is an admin, bob a member.
async fn two_user_code_app() -> (Router, tempfile::TempDir, std::path::PathBuf) {
    let (dir, store) = temp_db_store("code-two-user.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\n"),
    )
    .unwrap();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    let mut config = Config::desktop(dir.path());
    config.profile = tidebreak_core::Profile::SelfHost;
    config.auth_tokens_file = Some(tokens_file);
    let mut state = AppState::new(
        config,
        store_trait,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(runtime);
    let repo = init_git_repo(dir.path());
    (app(state), dir, repo)
}

/// Deployment-plane code routes are admin-gated by where they are registered
/// (decision 6). A member is refused; an admin is not.
#[tokio::test(flavor = "multi_thread")]
async fn code_deployment_plane_routes_refuse_a_member() {
    let (router, _dir, _repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let member = client
        .get(format!("http://{addr}/code/repos/clone-defaults"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member.status(), reqwest::StatusCode::FORBIDDEN);
    let admin = client
        .get(format!("http://{addr}/code/repos/clone-defaults"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(admin.status(), reqwest::StatusCode::OK);

    let member_root = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member_root.status(), reqwest::StatusCode::FORBIDDEN);
    let admin_root = client
        .get(format!("http://{addr}/code/worktree-root"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_root.status(), reqwest::StatusCode::OK);

    let member_refresh = client
        .post(format!("http://{addr}/code/harnesses/refresh"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(member_refresh.status(), reqwest::StatusCode::FORBIDDEN);

    // The member plane is untouched: the doctor read still answers a member.
    let doctor = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(doctor.status(), reqwest::StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_self_host_member_cannot_use_ambient_github_credentials_for_unowned_targets() {
    let (router, _dir, _repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repository = serde_json::json!({
        "host": "github.com",
        "owner": "private-org",
        "name": "deployment-only",
    });
    let requests = [
        (
            "/code/delivery/repositories/resolve",
            serde_json::json!({"repositories": ["private-org/deployment-only"]}),
        ),
        (
            "/code/delivery/pull-requests/query",
            serde_json::json!({"repositories": [repository.clone()]}),
        ),
        (
            "/code/delivery/pull-requests/detail",
            serde_json::json!({"repository": repository.clone(), "number": 1}),
        ),
        (
            "/code/delivery/pull-requests/action",
            serde_json::json!({
                "target": {"repository": repository.clone(), "number": 1},
                "action": {"type": "close"},
            }),
        ),
        (
            "/code/delivery/runs/query",
            serde_json::json!({"repositories": [repository.clone()]}),
        ),
        (
            "/code/delivery/runs/detail",
            serde_json::json!({
                "repository": repository.clone(),
                "kind": "workflow_run",
                "id": 1,
            }),
        ),
        (
            "/code/delivery/runs/action",
            serde_json::json!({
                "target": {
                    "repository": repository.clone(),
                    "kind": "workflow_run",
                    "id": 1,
                },
                "action": {"type": "rerun_failed"},
            }),
        ),
    ];

    for (path, body) in requests {
        let response = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} let a member target a repository outside their registered catalog"
        );
    }
}

/// Two users cloning the same remote must not collide on disk. The clone
/// parent directory is one shared setting, so the owner segment is what keeps
/// the second clone from landing on the first one's checkout.
#[test]
fn clone_targets_are_keyed_by_owner() {
    use crate::code::clone::{legacy_owner_dir, owner_dir};
    let parent = std::path::Path::new("/srv/checkouts");
    // The local profile is single-user and keeps the paths people already have.
    assert_eq!(
        owner_dir(parent, &tidebreak_core::OwnerId::local()),
        parent.to_path_buf()
    );
    let alice = owner_dir(parent, &tidebreak_core::OwnerId::new("alice").unwrap());
    let bob = owner_dir(parent, &tidebreak_core::OwnerId::new("bob").unwrap());
    assert_ne!(alice, bob);
    assert_eq!(alice.parent(), Some(parent));
    assert!(alice.starts_with(parent));
    // The compatibility path remains available for identifying existing rows,
    // but new paths never use it.
    assert_eq!(
        legacy_owner_dir(parent, &tidebreak_core::OwnerId::new("alice").unwrap()),
        parent.join("alice")
    );
    let hostile = owner_dir(
        parent,
        &tidebreak_core::OwnerId::new("../../etc/passwd").unwrap(),
    );
    assert_eq!(hostile.parent(), Some(parent));
    assert!(hostile.starts_with(parent));
    let dots = owner_dir(parent, &tidebreak_core::OwnerId::new("..").unwrap());
    assert_eq!(dots.parent(), Some(parent));
    assert_ne!(dots, parent);
}

/// Rows created with the legacy owner directory keep their exact absolute
/// paths. The new namespace applies only to later clones and worktrees.
#[tokio::test]
async fn legacy_managed_repo_and_worktree_paths_remain_accessible() {
    use crate::code::clone::{legacy_owner_dir, owner_dir, registered_legacy_clone_target};
    use crate::code::worktree::create_worktree;

    let (dir, store) = temp_db_store("code-owner-path-compat.db").await;
    let db = Arc::new(store);
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = CodeRuntime::with_registry(db.clone(), dir.path().to_path_buf(), registry);
    let owner = tidebreak_core::OwnerId::new("user:alice@example").unwrap();

    let clone_parent = dir.path().join("clones");
    let legacy_repo_root = legacy_owner_dir(&clone_parent, &owner).join("demo");
    let repo_root = init_git_repo_named(legacy_repo_root.parent().unwrap(), "demo");
    let repo = CodeRepo {
        id: RepoId::new(),
        owner: owner.clone(),
        root_path: repo_root.canonicalize().unwrap().display().to_string(),
        display_name: "demo".to_owned(),
        default_base_ref: "main".to_owned(),
        branch_prefix: "thet/".to_owned(),
        setup_script: None,
        archive_script: None,
        quick_actions: Vec::new(),
        created_at: chrono::Utc::now(),
        removed_at: None,
        cloned_from: Some("https://example.com/acme/demo.git".to_owned()),
        origin_host: None,
        origin_owner: None,
        origin_name: None,
    };
    tidebreak_core::db::code::insert_repo(&db, &repo)
        .await
        .unwrap();
    assert!(registered_legacy_clone_target(&db, &owner, &repo_root)
        .await
        .unwrap());
    let colliding_owner = tidebreak_core::OwnerId::new("user:alice_example").unwrap();
    assert_eq!(
        legacy_owner_dir(&clone_parent, &colliding_owner),
        legacy_repo_root.parent().unwrap()
    );
    assert!(
        !registered_legacy_clone_target(&db, &colliding_owner, &repo_root)
            .await
            .unwrap()
    );

    let legacy_worktree_root = legacy_owner_dir(&runtime.default_worktree_root(), &owner);
    let workspace_id = WorkspaceId::new();
    let worktree_path =
        legacy_worktree_root.join(format!("demo-{}", &workspace_id.to_string()[..8]));
    create_worktree(&repo_root, &worktree_path, "thet/legacy-owner-path", "main")
        .await
        .unwrap()
        .complete()
        .await;
    let workspace = CodeWorkspace {
        id: workspace_id,
        owner: owner.clone(),
        repo_id: repo.id,
        title: "Legacy owner path".to_owned(),
        worktree_path: worktree_path.display().to_string(),
        branch_name: "thet/legacy-owner-path".to_owned(),
        base_ref: "main".to_owned(),
        status: CodeWorkspaceStatus::Active,
        pr: None,
        created_at: chrono::Utc::now(),
        archived_at: None,
        released_at: None,
        released_tip: None,
        bundle_bytes: None,
    };
    tidebreak_core::db::code::insert_workspace(&db, &workspace)
        .await
        .unwrap();

    assert_ne!(
        owner_dir(&clone_parent, &owner),
        legacy_repo_root.parent().unwrap()
    );
    let new_worktree_root = runtime.owner_worktree_root(&owner).await.unwrap();
    assert_ne!(new_worktree_root, legacy_worktree_root);
    let reread_repo = runtime.get_repo(&owner, repo.id).await.unwrap();
    assert_eq!(reread_repo.root_path, repo.root_path);
    let next_workspace = runtime
        .create_workspace(
            &owner,
            repo.id,
            Some("New owner namespace".to_owned()),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(std::path::Path::new(&next_workspace.worktree_path).starts_with(new_worktree_root));
    let reread_workspace = runtime.get_workspace(&owner, workspace_id).await.unwrap();
    assert_eq!(reread_workspace.worktree_path, workspace.worktree_path);
    let (paths, truncated) = runtime
        .workspace_tree(&owner, workspace_id, "README", Some(10))
        .await
        .unwrap();
    assert_eq!(paths, vec!["README.md"]);
    assert!(!truncated);
}

/// The `/code/*` routes reach the store only through the owner-scoped view.
///
/// Decision 6 puts enforcement in the router rather than in handler habits,
/// and decision 48 step 1 applies that to data scoping: a new code route is
/// owner-scoped because of what it extracts, not because its author
/// remembered to filter. This check is the tripwire for the two ways back
/// out — an unscoped runtime gate, or a system-path store function called
/// from a request path.
#[test]
fn code_routes_go_through_the_owner_scoped_view() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes/code");
    let mut findings = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("code routes directory") {
        let path = entry.expect("code routes entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let text = std::fs::read_to_string(&path).expect("code route file");
        // A system-path store function is never a request path.
        if text.contains("_all_owners") {
            findings.push(format!(
                "{name} calls an `_all_owners` store function; those are system \
                 paths (boot recovery, the stall sweep), not request paths"
            ));
        }
        // The pre-scoping gate answered "is code mode configured", never
        // "whose row is this". `ScopedCode` answers both.
        if text.contains("require_code(") {
            findings.push(format!(
                "{name} uses an unscoped code gate; extract `ScopedCode` instead"
            ));
        }
        // Files that serve code data extract the scoped view. `mod.rs` and
        // `types.rs` declare and shape, and serve nothing.
        let serves_data = text.contains("pub async fn") && name != "mod.rs";
        if serves_data && !text.contains("ScopedCode") {
            // Browser, harness inference, and external adapters are
            // capability-bearer routes. Each derives the owner from a
            // narrower credential instead of accepting the app-token
            // `ScopedCode` extractor. Require each route's own authorization
            // path here.
            if name == "browser.rs" {
                if !text.contains("fn authorize(")
                    || !text.contains("BrowserSubject")
                    || !text.contains("bearer_token")
                {
                    findings.push(format!(
                        "{name} is the capability-bearer browser route but is \
                         missing its `authorize` / `BrowserSubject` / \
                        `bearer_token` authorization path"
                    ));
                }
            } else if name == "llm.rs" {
                if !text.contains("HarnessLlmRelay")
                    || !text.contains("HeaderMap")
                    || !text.contains("relay.forward(")
                {
                    findings.push(format!(
                        "{name} is the capability-bearer harness inference \
                         route but is missing its `HarnessLlmRelay` / \
                        `HeaderMap` / `relay.forward` authorization path"
                    ));
                }
            } else if name == "external.rs" {
                if !text.contains("ExternalGrantAuth")
                    || !text.contains("authenticate_adapter_token")
                    || !text.contains("session_bound_to_grant")
                {
                    findings.push(format!(
                        "{name} is the adapter grant route but is missing its \
                         `ExternalGrantAuth` / `authenticate_adapter_token` / \
                         `session_bound_to_grant` authorization path"
                    ));
                }
            } else {
                findings.push(format!(
                    "{name} defines route handlers but never extracts `ScopedCode`"
                ));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "code routes must query through the owner-scoped view: {findings:?}"
    );
}
