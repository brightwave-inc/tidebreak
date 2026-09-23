//! Owner scoping of code rows, routes, clones, and credentials.

use super::code::*;
use super::*;

use std::sync::Arc;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{CodeRepo, CodeWorkspace, CodeWorkspaceStatus, RepoId, Store, WorkspaceId};
use tidebreak_harness::AdapterRegistry;

/// A self-host code app with three principals: alice is an admin, bob and
/// carol are members. Carol holds no row on anything alice makes, which is
/// what a `deployment` session has to be visible to and a `private` one not.
async fn two_user_code_app() -> (Router, tempfile::TempDir, std::path::PathBuf) {
    let (router, dir, repo, _runtime) = two_user_code_app_with_runtime().await;
    (router, dir, repo)
}

async fn two_user_code_app_with_runtime() -> (
    Router,
    tempfile::TempDir,
    std::path::PathBuf,
    Arc<CodeRuntime>,
) {
    let (dir, store) = temp_db_store("code-two-user.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let tokens_file = dir.path().join("tokens");
    std::fs::write(
        &tokens_file,
        format!("alice {ALICE_TOKEN} admin\nbob {BOB_TOKEN}\ncarol {CAROL_TOKEN}\nslack {SLACK_TOKEN} service\n"),
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
    state.code = Some(runtime.clone());
    let repo = init_git_repo(dir.path());
    (app(state), dir, repo, runtime)
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
        setup_error: None,
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
    let (paths, truncated, source) = runtime
        .workspace_tree(&owner, workspace_id, "README", Some(10))
        .await
        .unwrap();
    assert_eq!(paths, vec!["README.md"]);
    assert!(!truncated);
    assert!(source.is_none());
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
            // Browser, native computer use, harness inference, and external
            // adapters are capability-bearer routes. Each derives the owner from a
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
            } else if name == "native.rs" {
                if !text.contains("fn authorize(")
                    || !text.contains("NativeSubject")
                    || !text.contains("bearer_token")
                    || !text.contains("subject_for_token(token)")
                    || !text.contains("get_session(&code.db, &subject.owner, subject.session)")
                    || !text.contains("get_workspace(&code.db, &subject.owner, subject.workspace)")
                {
                    findings.push(format!(
                        "{name} is the capability-bearer native route but is \
                         missing token-derived subject authorization or owner-scoped \
                         session and workspace checks"
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
            } else if name == "conversation.rs" {
                if !text.contains("ExternalGrantAuth") || !text.contains("require_bound(") {
                    findings.push(format!(
                        "{name} is the adapter conversation-tool route but is \
                         missing its `ExternalGrantAuth` / `require_bound` \
                         authorization path"
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

// ---------------------------------------------------------------------------
// Session access (decision 0086)
// ---------------------------------------------------------------------------

/// Register a repository, open a workspace, and start one session under the
/// named principal. Returns the session id.
async fn owned_session(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    repo: &std::path::Path,
) -> String {
    let (_repo_body, workspace) = register_and_workspace(client, addr, token, repo).await;
    create_sibling_sessions(client, addr, token, &workspace, 1)
        .await
        .remove(0)
}

async fn get_status(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    path: &str,
) -> reqwest::StatusCode {
    client
        .get(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
}

/// Publish the one-pixel fixture image to a session as the named principal.
async fn publish_png(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session: &str,
) -> reqwest::Response {
    client
        .post(format!(
            "http://{addr}/sessions/{session}/attachments/images"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "image/png")
        .body(super::code_attachments::one_pixel_png())
        .send()
        .await
        .unwrap()
}

async fn post_status(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> reqwest::StatusCode {
    client
        .post(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
        .status()
}

/// Grant one subject a level on one session, as its owner.
async fn grant_access(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    session: &str,
    subject: &str,
    level: &str,
) {
    let response = client
        .post(format!("http://{addr}/sessions/{session}/access"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "subject": subject, "level": level }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        reqwest::StatusCode::CREATED,
        "the owner may grant access to their own session"
    );
}

async fn owned_delegated_tree(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    runtime: &CodeRuntime,
    repo: &std::path::Path,
) -> (String, String, tidebreak_core::code::CodeGrantId) {
    let (_, workspace) = register_and_workspace(client, addr, ALICE_TOKEN, repo).await;
    let sessions = create_sibling_sessions(client, addr, ALICE_TOKEN, &workspace, 2).await;
    let parent = sessions[0].parse().unwrap();
    let child = sessions[1].parse().unwrap();
    let owner = tidebreak_core::OwnerId::new("user:alice").unwrap();
    let (grant, _) = runtime
        .mint_adapter_grant(&owner, "slack", "U-alice", "T1")
        .await
        .unwrap();
    tidebreak_core::db::code::set_session_context(
        &runtime.db,
        &owner,
        child,
        None,
        Some(parent),
        Some("review"),
    )
    .await
    .unwrap();
    for (session, key) in [
        (parent, "T1/C1/123.45".to_owned()),
        (child, format!("child/{parent}/review")),
    ] {
        tidebreak_core::db::code::bind_external_session(
            &runtime.db,
            &owner,
            grant.id,
            "slack",
            &key,
            session,
        )
        .await
        .unwrap();
    }
    let stored = tidebreak_core::db::code::get_session(&runtime.db, &owner, child)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.visibility,
        tidebreak_core::SessionVisibility::Private
    );
    assert!(
        tidebreak_core::db::code::list_session_access(&runtime.db, &owner, child)
            .await
            .unwrap()
            .is_empty()
    );
    (sessions[0].clone(), sessions[1].clone(), grant.id)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bound_child_inherits_parent_view_and_contribute_without_ownership() {
    let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (parent, child, _) = owned_delegated_tree(&client, addr, &runtime, &repo).await;

    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{child}")).await,
        reqwest::StatusCode::NOT_FOUND,
    );
    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &parent,
        "principal:user:bob",
        "view",
    )
    .await;
    for path in [
        format!("/sessions/{child}"),
        format!("/sessions/{child}/turns"),
        format!("/sessions/{child}/queued"),
    ] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &path).await,
            reqwest::StatusCode::OK,
            "a parent viewer must read {path}",
        );
    }
    assert_eq!(
        post_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{child}/turns"),
            serde_json::json!({ "message": "viewer cannot send" }),
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
    );

    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &parent,
        "principal:user:bob",
        "contribute",
    )
    .await;
    for (path, body) in [
        (format!("/sessions/{child}/reap"), serde_json::json!({})),
        (
            format!("/sessions/{child}/turns"),
            serde_json::json!({ "message": "change model", "model": "other-model" }),
        ),
        (
            format!("/sessions/{child}/mode"),
            serde_json::json!({ "permission_mode": "allow" }),
        ),
        (
            format!("/sessions/{child}/access"),
            serde_json::json!({ "subject": "principal:user:carol", "level": "view" }),
        ),
        (
            format!("/sessions/{child}/visibility"),
            serde_json::json!({ "visibility": "deployment" }),
        ),
    ] {
        assert_eq!(
            post_status(&client, addr, BOB_TOKEN, &path, body).await,
            reqwest::StatusCode::NOT_FOUND,
            "a parent contributor must not manage {path}",
        );
    }
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{child}/access")
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
    );
    let submitted = client
        .post(format!("http://{addr}/sessions/{child}/turns"))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({ "message": "review the child work" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = submitted.json().await.unwrap();
    assert_eq!(turn["actor"]["principal"], "user:bob");

    let stored: serde_json::Value = client
        .get(format!("http://{addr}/sessions/{child}"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stored["visibility"], "private");
    assert_ne!(stored["model"], "other-model");
    let rows: serde_json::Value = client
        .get(format!("http://{addr}/sessions/{child}/access"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        rows,
        serde_json::json!([]),
        "inheritance must not copy rows"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_parent_access_drops_the_bound_child_live_stream() {
    use futures::StreamExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    for revocation in ["parent row", "binding grant", "row without notice"] {
        let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
        let addr = serve(router).await;
        let client = reqwest::Client::new();
        let (parent, child, grant) = owned_delegated_tree(&client, addr, &runtime, &repo).await;
        let owner = tidebreak_core::OwnerId::new("user:alice").unwrap();
        let child_id = child.parse().unwrap();
        grant_access(
            &client,
            addr,
            ALICE_TOKEN,
            &parent,
            "principal:user:bob",
            "view",
        )
        .await;
        let mut request = format!("ws://{addr}/sessions/{child}/events?after=0")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {BOB_TOKEN}").parse().unwrap(),
        );
        let (mut socket, _) = connect_async(request).await.unwrap();
        let marker = tidebreak_core::Event::HarnessNotice {
            level: tidebreak_core::HarnessNoticeLevel::Info,
            message: "child stream ready".into(),
        };
        let epoch = tidebreak_core::db::code::get_session(&runtime.db, &owner, child_id)
            .await
            .unwrap()
            .unwrap()
            .spawn_epoch;
        let seq =
            tidebreak_core::db::code::append_event(&runtime.db, &owner, child_id, epoch, &marker)
                .await
                .unwrap();
        runtime.bus.publish(
            child_id,
            tidebreak_core::code::SequencedEvent { seq, event: marker },
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let frame = next_json(&mut socket).await;
                if frame["event"]["message"] == "child stream ready" {
                    break;
                }
            }
        })
        .await
        .expect("the child stream must deliver a frame before revocation");

        match revocation {
            "parent row" => {
                let revoked = client
                    .delete(format!(
                        "http://{addr}/sessions/{parent}/access/principal:user:bob"
                    ))
                    .bearer_auth(ALICE_TOKEN)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);
            }
            "binding grant" => {
                runtime
                    .revoke_adapter_grant(&owner, grant, "the workspace was unlinked")
                    .await
                    .unwrap();
            }
            "row without notice" => {
                // A delayed access notice must not let another live frame through.
                assert!(tidebreak_core::db::code::revoke_session_access(
                    &runtime.db,
                    &owner,
                    parent.parse().unwrap(),
                    "principal:user:bob",
                )
                .await
                .unwrap());
                runtime.bus.publish_transient(
                    child_id,
                    tidebreak_core::Event::AssistantDelta {
                        text: "private text after revoke".into(),
                    },
                );
            }
            _ => unreachable!(),
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(frame) = socket.next().await {
                match frame {
                    Ok(WsMessage::Close(_)) | Err(_) => return,
                    Ok(WsMessage::Text(text)) => assert!(
                        !text.contains("private text after revoke"),
                        "{revocation} must prevent transient text from reaching the reader",
                    ),
                    Ok(_) => continue,
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{revocation} must close the child's open event stream"));
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{child}")).await,
            reqwest::StatusCode::NOT_FOUND,
            "{revocation} must remove inherited access",
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bound_child_streams_public_digests_until_parent_visibility_is_private() {
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (parent, child, _) = owned_delegated_tree(&client, addr, &runtime, &repo).await;
    assert_eq!(
        post_status(
            &client,
            addr,
            ALICE_TOKEN,
            &format!("/sessions/{parent}/visibility"),
            serde_json::json!({ "visibility": "deployment" }),
        )
        .await,
        reqwest::StatusCode::OK,
    );
    let snapshot: serde_json::Value = client
        .get(format!("http://{addr}/sessions/{parent}"))
        .bearer_auth(CAROL_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        snapshot["children"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["id"] == child),
        "public parent snapshots include the delegated child"
    );
    let mut request = format!("ws://{addr}/updates")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {CAROL_TOKEN}").parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), next_json(&mut socket))
        .await
        .expect("the inherited reader must receive the initial snapshot");
    assert_eq!(first["type"], "snapshot");
    assert!(first["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|digest| digest["session"] == child));
    assert_eq!(
        post_status(
            &client,
            addr,
            ALICE_TOKEN,
            &format!("/sessions/{child}/turns"),
            serde_json::json!({ "message": "publish a child update" }),
        )
        .await,
        reqwest::StatusCode::ACCEPTED,
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notice = next_json(&mut socket).await;
            if notice["type"] == "digest" && notice["session"] == child && notice["turn_count"] == 1
            {
                break;
            }
        }
    })
    .await
    .expect("the inherited public reader must receive the child's live digest");
    assert_eq!(
        post_status(
            &client,
            addr,
            ALICE_TOKEN,
            &format!("/sessions/{parent}/visibility"),
            serde_json::json!({ "visibility": "private" }),
        )
        .await,
        reqwest::StatusCode::OK,
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notice = next_json(&mut socket).await;
            if notice["type"] == "snapshot" {
                let sessions = notice["sessions"].as_array().unwrap();
                assert!(sessions.iter().all(|digest| digest["session"] != child));
                assert!(sessions.iter().all(|digest| digest["session"] != parent));
                break;
            }
        }
    })
    .await
    .expect("narrowing the parent visibility must remove the child from the live snapshot");
}

/// The drill from decision 0086 on the self-host profile, where a machine has
/// many principals: a viewer reads and never writes, a contributor writes but
/// holds no lifecycle authority, and revoking the row puts the session back
/// out of reach.
#[tokio::test(flavor = "multi_thread")]
async fn a_viewer_reads_a_contributor_writes_and_neither_owns() {
    let (router, _dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session = owned_session(&client, addr, ALICE_TOKEN, &repo).await;

    // Before any row exists the behavior is exactly what it was: another
    // principal's session is indistinguishable from one that never existed.
    for path in [
        format!("/sessions/{session}"),
        format!("/sessions/{session}/turns"),
        format!("/sessions/{session}/queued"),
    ] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &path).await,
            reqwest::StatusCode::NOT_FOUND,
            "{path} answered before a row existed"
        );
    }

    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &session,
        "principal:user:bob",
        "view",
    )
    .await;

    // A viewer sees exactly the reads.
    for path in [
        format!("/sessions/{session}"),
        format!("/sessions/{session}/turns"),
        format!("/sessions/{session}/queued"),
        format!("/approvals?session_id={session}"),
    ] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &path).await,
            reqwest::StatusCode::OK,
            "a viewer must read {path}"
        );
    }

    // And none of the writes. Each answers not found rather than forbidden:
    // the caller learns no more than they would about a session that does
    // not exist.
    let writes = [
        (
            format!("/sessions/{session}/turns"),
            serde_json::json!({ "message": "do the thing" }),
        ),
        (
            format!("/sessions/{session}/interrupt"),
            serde_json::json!({}),
        ),
        (
            format!("/sessions/{session}/steer"),
            serde_json::json!({
                "expected_turn_id": uuid::Uuid::new_v4(),
                "guidance": "look at the other file",
            }),
        ),
        (
            format!("/sessions/{session}/queued/send-now"),
            serde_json::json!({}),
        ),
    ];
    for (path, body) in &writes {
        assert_eq!(
            post_status(&client, addr, BOB_TOKEN, path, body.clone()).await,
            reqwest::StatusCode::NOT_FOUND,
            "a viewer must not write {path}"
        );
    }
    // The queue pause is a PUT, and refused the same way.
    assert_eq!(
        client
            .put(format!("http://{addr}/sessions/{session}/queue-paused"))
            .bearer_auth(BOB_TOKEN)
            .json(&serde_json::json!({ "paused": true }))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND,
        "a viewer must not pause the queue"
    );
    // Publishing an image is a write too: it is the authority a later turn
    // attachment is checked against, so a viewer is refused before any
    // bytes are stored.
    assert_eq!(
        publish_png(&client, addr, BOB_TOKEN, &session)
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND,
        "a viewer must not publish an image"
    );

    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &session,
        "principal:user:bob",
        "contribute",
    )
    .await;

    // Submit overrides persist settings, so contribution alone cannot supply them.
    let before_settings: serde_json::Value = client
        .get(format!("http://{addr}/sessions/{session}"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for override_body in [
        serde_json::json!({"message": "change model", "model": "other-model"}),
        serde_json::json!({"message": "change effort", "reasoning_effort": "high"}),
        serde_json::json!({"message": "clear effort", "reasoning_effort": null}),
    ] {
        assert_eq!(
            post_status(
                &client,
                addr,
                BOB_TOKEN,
                &format!("/sessions/{session}/turns"),
                override_body
            )
            .await,
            reqwest::StatusCode::NOT_FOUND,
            "a contributor cannot override session settings in a turn"
        );
    }

    // A contributor submits.
    let submitted = client
        .post(format!("http://{addr}/sessions/{session}/turns"))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({ "message": "trace the access path" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        submitted.status(),
        reqwest::StatusCode::ACCEPTED,
        "a contributor submits a turn"
    );
    // And the turn says who sent it, not whose session it ran under.
    let turn: serde_json::Value = submitted.json().await.unwrap();
    assert_eq!(
        turn["actor"]["principal"].as_str(),
        Some("user:bob"),
        "the submitted turn names its actor"
    );

    let after_settings: serde_json::Value = client
        .get(format!("http://{addr}/sessions/{session}"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_settings["model"], before_settings["model"]);
    assert_eq!(
        after_settings["reasoning_effort"],
        before_settings["reasoning_effort"]
    );

    // A contributor publishes an image and attaches it. The publication row
    // is written under the session's owner, which is the scope the turn's
    // attachment check reads it back through; written under the caller it
    // would be refused here, and the attachment would never resolve.
    let published = publish_png(&client, addr, BOB_TOKEN, &session).await;
    assert_eq!(
        published.status(),
        reqwest::StatusCode::CREATED,
        "a contributor publishes an image"
    );
    let published: serde_json::Value = published.json().await.unwrap();
    let blob_id = published["attachment_id"]
        .as_str()
        .or_else(|| published["blob_id"].as_str())
        .expect("the publication names its blob")
        .to_owned();
    assert_eq!(
        post_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{session}/turns"),
            serde_json::json!({
                "message": "and look at this",
                "attachments": [{ "blob_id": blob_id, "media_type": "image/png" }],
            }),
        )
        .await,
        reqwest::StatusCode::ACCEPTED,
        "a contributor attaches the image they published"
    );

    // Lifecycle and settings stay with the owner, and so does the access
    // list itself.
    let owner_only = [
        (format!("/sessions/{session}/reap"), serde_json::json!({})),
        (
            format!("/sessions/{session}/mode"),
            serde_json::json!({ "permission_mode": "allow" }),
        ),
        (
            format!("/sessions/{session}/effort"),
            serde_json::json!({ "reasoning_effort": "high" }),
        ),
        (
            format!("/sessions/{session}/access"),
            serde_json::json!({ "subject": "principal:user:carol", "level": "view" }),
        ),
        (
            format!("/sessions/{session}/visibility"),
            serde_json::json!({ "visibility": "deployment" }),
        ),
    ];
    for (path, body) in &owner_only {
        assert_eq!(
            post_status(&client, addr, BOB_TOKEN, path, body.clone()).await,
            reqwest::StatusCode::NOT_FOUND,
            "a contributor must not reach {path}"
        );
    }
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{session}/access")
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
        "a contributor must not read the access list"
    );

    // The owner reads the list and sees one row.
    let listed = client
        .get(format!("http://{addr}/sessions/{session}/access"))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let rows: serde_json::Value = listed.json().await.unwrap();
    let rows = rows.as_array().expect("the access list is an array");
    assert_eq!(rows.len(), 1, "granting twice raises one row, not two");
    assert_eq!(rows[0]["subject"].as_str(), Some("principal:user:bob"));
    assert_eq!(rows[0]["level"].as_str(), Some("contribute"));
    assert_eq!(rows[0]["granted_by"].as_str(), Some("user:alice"));

    // Revocation puts the session back out of reach.
    let revoked = client
        .delete(format!(
            "http://{addr}/sessions/{session}/access/principal:user:bob"
        ))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::NOT_FOUND,
        "a revoked row leaves nothing behind"
    );
}

/// `deployment` visibility opens a session to every authenticated principal
/// on the machine, and never to a write. `private` closes it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_deployment_session_shows_to_a_third_principal_and_a_private_one_does_not() {
    let (router, _dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session = owned_session(&client, addr, ALICE_TOKEN, &repo).await;

    assert_eq!(
        get_status(&client, addr, CAROL_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::NOT_FOUND,
        "a private session holds no third principal"
    );

    let opened = client
        .post(format!("http://{addr}/sessions/{session}/visibility"))
        .bearer_auth(ALICE_TOKEN)
        .json(&serde_json::json!({ "visibility": "deployment" }))
        .send()
        .await
        .unwrap();
    assert_eq!(opened.status(), reqwest::StatusCode::OK);
    let snapshot: serde_json::Value = opened.json().await.unwrap();
    assert_eq!(snapshot["visibility"].as_str(), Some("deployment"));

    assert_eq!(
        get_status(&client, addr, CAROL_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::OK,
        "a deployment session shows to a third principal"
    );
    // Visibility never grants a write.
    assert_eq!(
        post_status(
            &client,
            addr,
            CAROL_TOKEN,
            &format!("/sessions/{session}/turns"),
            serde_json::json!({ "message": "not mine to send" }),
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
        "deployment visibility is a read, not a contribution"
    );

    let closed = client
        .post(format!("http://{addr}/sessions/{session}/visibility"))
        .bearer_auth(ALICE_TOKEN)
        .json(&serde_json::json!({ "visibility": "private" }))
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), reqwest::StatusCode::OK);
    assert_eq!(
        get_status(&client, addr, CAROL_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::NOT_FOUND,
        "narrowing back to private closes it again"
    );
}

/// Revoking a row severs the reader's open event socket rather than leaving
/// it streaming a session they no longer hold.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_row_drops_that_readers_live_stream() {
    use futures::StreamExt;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let (router, _dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session = owned_session(&client, addr, ALICE_TOKEN, &repo).await;
    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &session,
        "principal:user:bob",
        "view",
    )
    .await;

    let mut request = format!("ws://{addr}/sessions/{session}/events?after=0")
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {BOB_TOKEN}").parse().unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();

    let revoked = client
        .delete(format!(
            "http://{addr}/sessions/{session}/access/principal:user:bob"
        ))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);

    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(frame) = socket.next().await {
            match frame {
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
        true
    })
    .await;
    assert_eq!(
        closed,
        Ok(true),
        "the revoked reader's stream must close without waiting for a reconnect"
    );
}

/// An external-identity row names someone the machine knows only through an
/// adapter. It resolves for a web caller through their live grant for that
/// identity, and stops resolving when the grant is fenced — the row itself
/// never changes.
#[tokio::test(flavor = "multi_thread")]
async fn an_external_identity_row_resolves_only_through_a_live_grant() {
    let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session = owned_session(&client, addr, ALICE_TOKEN, &repo).await;
    let bob = tidebreak_core::OwnerId::new("user:bob").unwrap();

    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        &session,
        "external:slack:U42",
        "view",
    )
    .await;
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::NOT_FOUND,
        "the row alone binds nobody on this machine"
    );

    let (grant, _pair) = runtime
        .mint_adapter_grant(&bob, "slack", "U42", "T1")
        .await
        .unwrap();
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::OK,
        "a live grant binds the principal to the identity the row names"
    );

    runtime
        .revoke_adapter_grant(&bob, grant.id, "the workspace was unlinked")
        .await
        .unwrap();
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("/sessions/{session}")).await,
        reqwest::StatusCode::NOT_FOUND,
        "a fenced grant makes the row inert without the row changing"
    );
}

/// The desktop profile has one owner, so the drill there is that nothing
/// moved: a session with no rows and `private` visibility answers its owner
/// exactly as before, and the owner still administers its access list.
#[tokio::test(flavor = "multi_thread")]
async fn the_single_owner_profile_keeps_its_default_behavior() {
    let (router, token, _runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let session = owned_session(&client, addr, &token, &repo).await;

    let snapshot = client
        .get(format!("http://{addr}/sessions/{session}"))
        .bearer_auth(&*token)
        .send()
        .await
        .unwrap();
    assert_eq!(snapshot.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = snapshot.json().await.unwrap();
    assert_eq!(
        body["visibility"].as_str(),
        Some("private"),
        "a fresh session is private"
    );

    let listed = client
        .get(format!("http://{addr}/sessions/{session}/access"))
        .bearer_auth(&*token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let rows: serde_json::Value = listed.json().await.unwrap();
    assert!(
        rows.as_array().expect("an array").is_empty(),
        "a session nobody shared has no rows"
    );

    // The owner administers the list on this profile too, and the store
    // refuses a subject that is neither a principal nor a channel identity.
    assert_eq!(
        post_status(
            &client,
            addr,
            &token,
            &format!("/sessions/{session}/access"),
            serde_json::json!({ "subject": "whoever", "level": "view" }),
        )
        .await,
        reqwest::StatusCode::BAD_REQUEST,
        "a malformed subject is a bad request, not a store failure"
    );
    assert_eq!(
        post_status(
            &client,
            addr,
            &token,
            &format!("/sessions/{session}/access"),
            serde_json::json!({ "subject": "principal:someone-else", "level": "view" }),
        )
        .await,
        reqwest::StatusCode::CREATED,
    );

    // The owner's own reads and writes are untouched by any of it.
    assert_eq!(
        get_status(&client, addr, &token, &format!("/sessions/{session}/turns")).await,
        reqwest::StatusCode::OK,
    );
    let submitted = client
        .post(format!("http://{addr}/sessions/{session}/turns"))
        .bearer_auth(&*token)
        .json(&serde_json::json!({ "message": "still mine to send" }))
        .send()
        .await
        .unwrap();
    assert_eq!(submitted.status(), reqwest::StatusCode::ACCEPTED);
    let turn: serde_json::Value = submitted.json().await.unwrap();
    assert_eq!(
        turn["actor"]["principal"].as_str(),
        Some("local"),
        "a desktop turn names the principal that sent it"
    );
}

/// A shared task reveals its workspace, never sibling sessions or repository settings.
#[tokio::test(flavor = "multi_thread")]
async fn shared_session_workspace_reads_preserve_ownership_and_revocation() {
    let (router, dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (repo_body, workspace) = register_and_workspace(&client, addr, ALICE_TOKEN, &repo).await;
    let private_repo = super::code::init_git_repo_named(dir.path(), "private-repo-not-granted");
    let (_, private_workspace) =
        register_and_workspace(&client, addr, ALICE_TOKEN, &private_repo).await;
    let sessions = create_sibling_sessions(&client, addr, ALICE_TOKEN, &workspace, 2).await;
    std::fs::write(
        std::path::Path::new(workspace["worktree_path"].as_str().unwrap())
            .join("private-sibling.txt"),
        "private sibling file",
    )
    .unwrap();
    let workspace_id = workspace["id"].as_str().unwrap();
    let session = &sessions[0];
    let path = format!("/code/workspaces/{workspace_id}");
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("{path}/diff")).await,
        reqwest::StatusCode::NOT_FOUND,
        "a caller without a session grant cannot read the workspace diff"
    );
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &path).await,
        reqwest::StatusCode::NOT_FOUND
    );
    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        session,
        "principal:user:bob",
        "view",
    )
    .await;
    let shared: serde_json::Value = client
        .get(format!("http://{addr}{path}"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shared["read_only"], true);
    assert_eq!(shared["repo_display_name"], repo_body["display_name"]);
    for field in [
        "root_path",
        "setup_script",
        "archive_script",
        "quick_actions",
    ] {
        assert!(
            shared.get(field).is_none(),
            "repository settings must stay private"
        );
    }
    let repos: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/repos"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        repos.is_empty(),
        "workspace metadata must not widen repository access"
    );
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!(
                "/code/workspaces/{}",
                private_workspace["id"].as_str().unwrap()
            )
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
        "ungranted repository labels must stay private"
    );
    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        session,
        "principal:user:bob",
        "contribute",
    )
    .await;
    assert_eq!(
        client
            .patch(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .json(&serde_json::json!({"title":"Not my workspace"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );

    grant_access(
        &client,
        addr,
        ALICE_TOKEN,
        session,
        "principal:user:bob",
        "view",
    )
    .await;
    for list_path in [
        "/code/workspaces".to_owned(),
        format!(
            "/code/workspaces?repo_id={}",
            repo_body["id"].as_str().unwrap()
        ),
    ] {
        let discovered: Vec<serde_json::Value> = client
            .get(format!("http://{addr}{list_path}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0]["id"], workspace_id);
        assert_eq!(discovered[0]["read_only"], true);
        assert_eq!(
            discovered[0]["repo_display_name"],
            repo_body["display_name"]
        );
        assert!(!serde_json::to_string(&discovered)
            .unwrap()
            .contains("private-repo-not-granted"));
    }

    let listed: Vec<serde_json::Value> = client
        .get(format!("http://{addr}{path}/sessions"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], *session);
    assert_eq!(listed[0]["access"], "view");
    assert_eq!(listed[0]["is_owner"], false);
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{}", sessions[1])
        )
        .await,
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{session}/debug")
        )
        .await,
        reqwest::StatusCode::OK,
        "a granted reader may inspect the shared session debug dump"
    );
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/sessions/{}/debug", sessions[1])
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
        "a granted reader must not inspect an ungranted sibling session"
    );
    assert_eq!(
        get_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("/code/repos/{}", repo_body["id"].as_str().unwrap())
        )
        .await,
        reqwest::StatusCode::NOT_FOUND
    );
    for suffix in [
        "/tree",
        "/files",
        "/blob?path=README.md",
        "/file?path=README.md",
    ] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &format!("{path}{suffix}")).await,
            reqwest::StatusCode::OK,
            "a granted reader must read {suffix}"
        );
    }
    let diff: serde_json::Value = client
        .get(format!("http://{addr}{path}/diff"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        diff["diff"]
            .as_str()
            .is_some_and(|body| body.contains("private-sibling.txt")),
        "a granted reader sees the shared workspace diff"
    );
    assert_eq!(
        client
            .patch(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .json(&serde_json::json!({"title": "not yours"}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("{path}/sessions"),
            serde_json::json!({"harness": "codex", "permission_mode": "ask"})
        )
        .await,
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &format!("{path}/terminals")).await,
        reqwest::StatusCode::NOT_FOUND,
        "a granted reader must not read live terminals"
    );
    let revoke = client
        .delete(format!(
            "http://{addr}/sessions/{session}/access/principal:user:bob"
        ))
        .bearer_auth(ALICE_TOKEN)
        .send()
        .await
        .unwrap();
    assert!(revoke.status().is_success(), "{}", revoke.status());
    let discovered: Vec<serde_json::Value> = client
        .get(format!("http://{addr}/code/workspaces"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        discovered.is_empty(),
        "revoked workspaces must disappear from discovery"
    );
    for suffix in ["", "/sessions", "/tree"] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &format!("{path}{suffix}")).await,
            reqwest::StatusCode::NOT_FOUND
        );
    }
}

/// A reader of a shared session sees the workspace's files, and a save from
/// them gets the answer a stranger gets. Saving is a workspace write, gated
/// the way commit and push are.
#[tokio::test(flavor = "multi_thread")]
async fn a_shared_session_reader_cannot_save_a_workspace_file() {
    let (router, _dir, repo) = two_user_code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (_repo, workspace) = register_and_workspace(&client, addr, ALICE_TOKEN, &repo).await;
    let sessions = create_sibling_sessions(&client, addr, ALICE_TOKEN, &workspace, 1).await;
    let worktree = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let path = format!("/code/workspaces/{}", workspace["id"].as_str().unwrap());
    let save = |token: &'static str, base: String| {
        let client = client.clone();
        let url = format!("http://{addr}{path}/file");
        async move {
            client
                .put(url)
                .bearer_auth(token)
                .json(&serde_json::json!({
                    "path": "README.md",
                    "content": format!("saved by {token}\n"),
                    "base_hash": base,
                }))
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    let base = crate::code::worktree::content_hash(b"hello\n");

    assert_eq!(
        save(BOB_TOKEN, base.clone()).await,
        reqwest::StatusCode::NOT_FOUND,
        "a stranger cannot save"
    );
    for level in ["view", "contribute"] {
        grant_access(
            &client,
            addr,
            ALICE_TOKEN,
            &sessions[0],
            "principal:user:bob",
            level,
        )
        .await;
        let blob: serde_json::Value = client
            .get(format!("http://{addr}{path}/blob?path=README.md"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(blob["hash"], base, "a {level} reader still reads the file");
        assert_eq!(
            save(BOB_TOKEN, base.clone()).await,
            reqwest::StatusCode::NOT_FOUND,
            "a {level} reader cannot manage the workspace, so cannot save"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("README.md")).unwrap(),
            "hello\n"
        );
    }

    assert_eq!(save(ALICE_TOKEN, base).await, reqwest::StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        format!("saved by {ALICE_TOKEN}\n")
    );
}

const SLACK_TOKEN: &str = "slack-service-token-for-workspace-management";

#[tokio::test(flavor = "multi_thread")]
async fn shared_parent_snapshots_and_debug_hide_private_children() {
    let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (_, workspace) = register_and_workspace(&client, addr, ALICE_TOKEN, &repo).await;
    let sessions = create_sibling_sessions(&client, addr, ALICE_TOKEN, &workspace, 3).await;
    let owner = tidebreak_core::OwnerId::new("user:alice").unwrap();
    let parent_id = sessions[0].parse().unwrap();
    for child in &sessions[1..] {
        tidebreak_core::db::code::set_session_context(
            &runtime.db,
            &owner,
            child.parse().unwrap(),
            None,
            Some(parent_id),
            Some(child),
        )
        .await
        .unwrap();
    }
    for visible in &sessions[..2] {
        grant_access(
            &client,
            addr,
            ALICE_TOKEN,
            visible,
            "principal:user:bob",
            "view",
        )
        .await;
    }
    crate::code::session_tree::publish_for_parent(&runtime.db, &runtime.bus, &owner, parent_id)
        .await;

    for (token, count) in [(ALICE_TOKEN, 2), (BOB_TOKEN, 1)] {
        for suffix in ["", "/debug"] {
            let body: serde_json::Value = client
                .get(format!("http://{addr}/sessions/{parent_id}{suffix}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let snapshot = if suffix.is_empty() {
                &body
            } else {
                &body["session"]
            };
            let children = snapshot["children"].as_array().unwrap();
            assert_eq!(children.len(), count, "{body}");
            assert!(snapshot["wait"].is_null());
            if token == BOB_TOKEN {
                assert_eq!(children[0]["id"], sessions[1]);
            }
            if suffix == "/debug" {
                let trees: Vec<_> = body["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|frame| frame["event"]["type"] == "session_tree")
                    .collect();
                assert_eq!(trees.len(), 1, "{body}");
                let children = trees[0]["event"]["children"].as_array().unwrap();
                assert_eq!(children.len(), count, "{body}");
                if token == BOB_TOKEN {
                    assert_eq!(children[0]["id"], sessions[1]);
                }
            }
        }
    }
}

/// Gateway sessions can predate service provenance. Only a live workspace grant
/// authorizes management through an inherited contributor row.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_slack_child_management_requires_a_live_workspace_grant() {
    use tidebreak_core::{ActsAs, CodeGrantKind, SessionId};

    for kind in [CodeGrantKind::Person, CodeGrantKind::Workspace] {
        let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
        let addr = serve(router).await;
        let client = reqwest::Client::new();
        let (repo_body, workspace) =
            register_and_workspace(&client, addr, SLACK_TOKEN, &repo).await;
        let owner = tidebreak_core::OwnerId::new("user:slack").unwrap();
        let parent = runtime
            .create_session(
                &owner,
                None,
                workspace["id"].as_str().unwrap().parse().unwrap(),
                tidebreak_core::HarnessKind::ClaudeCode,
                crate::code::runtime::NewSessionSettings {
                    permission_mode: tidebreak_core::PermissionMode::Plan,
                    acts_as: Some(ActsAs::Bot),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(parent.owner_kind.is_none());
        let child_workspace = runtime
            .create_workspace(
                &owner,
                repo_body["id"].as_str().unwrap().parse().unwrap(),
                Some("Delegated work".into()),
                None,
                None,
            )
            .await
            .unwrap();
        let mut child = parent.clone();
        child.id = SessionId::new();
        child.workspace_id = Some(child_workspace.id);
        tidebreak_core::db::code::insert_session(&runtime.db, &child)
            .await
            .unwrap();
        let grant = tidebreak_core::db::code::mint_external_grant(
            &runtime.db,
            &owner,
            tidebreak_core::db::code::MintGrantSubject {
                channel_kind: "slack",
                external_identity: "T1",
                workspace_identity: "T1",
                kind,
            },
            &crate::code::grants::hash_adapter_token("legacy-access"),
            &crate::code::grants::hash_adapter_token("legacy-refresh"),
        )
        .await
        .unwrap();
        for (session, key) in [
            (parent.id, "T1/C1/1.1".to_owned()),
            (
                child.id,
                tidebreak_core::db::code::delegated_child_external_key(parent.id, "child"),
            ),
        ] {
            tidebreak_core::db::code::bind_external_session(
                &runtime.db,
                &owner,
                grant.id,
                "slack",
                &key,
                session,
            )
            .await
            .unwrap();
        }
        tidebreak_core::db::code::set_session_context(
            &runtime.db,
            &owner,
            parent.id,
            Some("C1"),
            None,
            None,
        )
        .await
        .unwrap();
        tidebreak_core::db::code::set_session_context(
            &runtime.db,
            &owner,
            child.id,
            Some("C1"),
            Some(parent.id),
            Some("child"),
        )
        .await
        .unwrap();
        let path = format!("/code/workspaces/{}", child_workspace.id);
        for level in ["view", "contribute"] {
            grant_access(
                &client,
                addr,
                SLACK_TOKEN,
                &parent.id.to_string(),
                "principal:user:bob",
                level,
            )
            .await;
            let can_manage = kind.is_workspace() && level == "contribute";
            for workspace_id in [
                workspace["id"].as_str().unwrap().to_owned(),
                child_workspace.id.to_string(),
            ] {
                let snapshot: serde_json::Value = client
                    .get(format!("http://{addr}/code/workspaces/{workspace_id}"))
                    .bearer_auth(BOB_TOKEN)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                assert_eq!(
                    snapshot["read_only"], !can_manage,
                    "{kind:?} {level}: {snapshot}"
                );
                assert_eq!(snapshot["is_owner"], false);
            }
            let renamed = client
                .patch(format!("http://{addr}{path}"))
                .bearer_auth(BOB_TOKEN)
                .json(&serde_json::json!({"title":"Delegated rename"}))
                .send()
                .await
                .unwrap();
            assert_eq!(
                renamed.status(),
                if can_manage {
                    reqwest::StatusCode::OK
                } else {
                    reqwest::StatusCode::NOT_FOUND
                }
            );
        }
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &format!("{path}/terminals")).await,
            reqwest::StatusCode::NOT_FOUND
        );
        assert_eq!(
            post_status(
                &client,
                addr,
                BOB_TOKEN,
                &format!("{path}/retry-setup"),
                serde_json::json!({})
            )
            .await,
            reqwest::StatusCode::NOT_FOUND
        );
        // A direct contributor row survives disconnect, but management must not.
        grant_access(
            &client,
            addr,
            SLACK_TOKEN,
            &child.id.to_string(),
            "principal:user:bob",
            "contribute",
        )
        .await;
        runtime
            .revoke_adapter_grant(&owner, grant.id, "workspace disconnected")
            .await
            .unwrap();
        let snapshot: serde_json::Value = client
            .get(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(snapshot["read_only"], true);
        assert_eq!(
            post_status(
                &client,
                addr,
                BOB_TOKEN,
                &format!("{path}/archive"),
                serde_json::json!({"force":true})
            )
            .await,
            reqwest::StatusCode::NOT_FOUND
        );
        client
            .delete(format!(
                "http://{addr}/sessions/{}/access/principal:user:bob",
                child.id
            ))
            .bearer_auth(SLACK_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &path).await,
            reqwest::StatusCode::NOT_FOUND
        );
    }
}

/// Management borrows only a Slack service workspace. Ownership, host execution,
/// sibling transcripts, and sharing remain separate permissions.
#[tokio::test(flavor = "multi_thread")]
async fn slack_workspace_management_is_scoped_and_revocable() {
    let (router, _dir, repo, runtime) = two_user_code_app_with_runtime().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let (repo_body, workspace) = register_and_workspace(&client, addr, SLACK_TOKEN, &repo).await;
    let sessions = create_sibling_sessions(&client, addr, SLACK_TOKEN, &workspace, 2).await;
    let id = workspace["id"].as_str().unwrap();
    let session = &sessions[0];
    let path = format!("/code/workspaces/{id}");
    let owner = tidebreak_core::OwnerId::new("user:slack").unwrap();
    let (grant, _) = runtime
        .mint_adapter_grant(&owner, "slack", "U1", "T1")
        .await
        .unwrap();
    tidebreak_core::db::code::bind_external_session(
        &runtime.db,
        &owner,
        grant.id,
        "slack",
        "T1/C1/123.45",
        session.parse().unwrap(),
    )
    .await
    .unwrap();
    let other = runtime
        .create_workspace(
            &owner,
            repo_body["id"].as_str().unwrap().parse().unwrap(),
            Some("Unshared work".into()),
            None,
            None,
        )
        .await
        .unwrap();

    for level in ["view", "contribute"] {
        grant_access(
            &client,
            addr,
            SLACK_TOKEN,
            session,
            "principal:user:bob",
            level,
        )
        .await;
        let snapshot: serde_json::Value = client
            .get(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(snapshot["read_only"], level == "view");
        assert_eq!(snapshot["is_owner"], false);
        let listed: Vec<serde_json::Value> = client
            .get(format!("http://{addr}/code/workspaces"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["read_only"], level == "view");
        assert_eq!(listed[0]["is_owner"], false);
        let renamed = client
            .patch(format!("http://{addr}{path}"))
            .bearer_auth(BOB_TOKEN)
            .json(&serde_json::json!({"title":"Managed from Tidebreak"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            renamed.status(),
            if level == "view" {
                reqwest::StatusCode::NOT_FOUND
            } else {
                reqwest::StatusCode::OK
            }
        );
    }
    // Workspace reads refer to the granted checkout, while repository history
    // and sibling sessions must not borrow the service identity.
    for suffix in ["/tree", "/file?path=README.md", "/files", "/pr"] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &format!("{path}{suffix}")).await,
            reqwest::StatusCode::OK
        );
    }
    for denied in [
        format!("{path}/search?query=secret&history=true"),
        format!("/code/workspaces/{}", other.id),
        format!("/code/workspaces/{}/tree", other.id),
        format!("/sessions/{}", sessions[1]),
        format!("/sessions/{session}/access"),
        format!("{path}/terminals"),
        format!("/code/repos/{}", repo_body["id"].as_str().unwrap()),
    ] {
        assert_eq!(
            get_status(&client, addr, BOB_TOKEN, &denied).await,
            reqwest::StatusCode::NOT_FOUND,
            "{denied}"
        );
    }
    for suffix in ["/terminals", "/retry-setup"] {
        assert_eq!(
            post_status(
                &client,
                addr,
                BOB_TOKEN,
                &format!("{path}{suffix}"),
                serde_json::json!({})
            )
            .await,
            reqwest::StatusCode::NOT_FOUND
        );
    }
    // The same service workspace can execute remotely. Persist the actual
    // remote marker and assert the public API never treats it as a host tree.
    let mut remote_workspace = runtime
        .get_workspace(&owner, id.parse().unwrap())
        .await
        .unwrap();
    remote_workspace.worktree_path = CodeWorkspace::remote_worktree_marker(remote_workspace.id);
    tidebreak_core::db::code::save_workspace(&runtime.db, &remote_workspace)
        .await
        .unwrap();
    // No external delegation gateway is installed in this fixture. A remote
    // PR read reaches the scoped runtime and reports that missing connection.
    let remote_pr = client
        .get(format!("http://{addr}{path}/pr"))
        .bearer_auth(BOB_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(remote_pr.status(), reqwest::StatusCode::CONFLICT);
    let remote_pr: serde_json::Value = remote_pr.json().await.unwrap();
    assert_eq!(remote_pr["kind"], "external_reconnect_required");
    for suffix in [
        "/tree",
        "/files",
        "/diff",
        "/file?path=README.md",
        "/blob?path=README.md",
    ] {
        let response = client
            .get(format!("http://{addr}{path}{suffix}"))
            .bearer_auth(BOB_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT, "{suffix}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            body["kind"], "sandbox_checkpoint_missing",
            "{suffix}: {body}"
        );
        assert!(
            !std::path::Path::new(&remote_workspace.worktree_path).exists(),
            "remote marker must not become a host path"
        );
    }
    let archived = client
        .post(format!("http://{addr}{path}/archive"))
        .bearer_auth(BOB_TOKEN)
        .json(&serde_json::json!({"force":true}))
        .send()
        .await
        .unwrap();
    assert!(archived.status().is_success(), "{}", archived.status());
    let archived: serde_json::Value = archived.json().await.unwrap();
    assert_eq!(archived["read_only"], false);
    assert_eq!(archived["is_owner"], false);
    assert_eq!(
        runtime
            .get_workspace(&owner, id.parse().unwrap())
            .await
            .unwrap()
            .owner,
        owner
    );

    runtime
        .revoke_adapter_grant(&owner, grant.id, "service connection revoked")
        .await
        .unwrap();
    assert_eq!(
        post_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("{path}/restore"),
            serde_json::json!({})
        )
        .await,
        reqwest::StatusCode::NOT_FOUND,
        "service provenance must not bypass a revoked connection"
    );

    client
        .delete(format!(
            "http://{addr}/sessions/{session}/access/principal:user:bob"
        ))
        .bearer_auth(SLACK_TOKEN)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        get_status(&client, addr, BOB_TOKEN, &path).await,
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_status(
            &client,
            addr,
            BOB_TOKEN,
            &format!("{path}/restore"),
            serde_json::json!({})
        )
        .await,
        reqwest::StatusCode::NOT_FOUND
    );
}
