//! Commit, push, pull-request, and quick-action routes against temp repos.

use super::*;

use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::CodeRuntime;
use crate::obo_gateway::test_support::FakeLender;
use crate::obo_gateway::{GitCredentialLender, GitForgeError};
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{QuickAction, RepoId};
use tidebreak_harness::AdapterRegistry;

async fn code_app() -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with(None).await
}

/// The git app, optionally on a "hosted machine" that lends gateway git
/// credentials (decision 63).
async fn code_app_with(
    lender: Option<Arc<dyn GitCredentialLender>>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code-git.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let mut runtime = CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry);
    if let Some(lender) = lender {
        runtime = runtime.with_git_credentials(lender);
    }
    let runtime = Arc::new(runtime);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
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
    let token = state.token.clone();
    (app(state), token, runtime, dir)
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

fn init_paired_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let bare = dir.join("origin.git");
    let work = dir.join("work");
    run(dir, &["git", "init", "--bare", bare.to_str().unwrap()]);
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["git", "init", "-b", "main"]);
    run(&work, &["git", "config", "user.email", "dev@example.com"]);
    run(&work, &["git", "config", "user.name", "Dev"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    run(&work, &["git", "add", "README.md"]);
    run(&work, &["git", "commit", "-m", "init"]);
    run(
        &work,
        &["git", "remote", "add", "origin", bare.to_str().unwrap()],
    );
    run(&work, &["git", "push", "-u", "origin", "main"]);
    work
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

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn run_stdout(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?} failed in {}",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn json_id(value: &serde_json::Value) -> &str {
    value["id"].as_str().expect("id is a string")
}

async fn register_and_workspace(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    repo: &std::path::Path,
    title: &str,
) -> (serde_json::Value, serde_json::Value) {
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": title,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    (repo_body, created.json().await.unwrap())
}

async fn error_kind(response: reqwest::Response) -> (reqwest::StatusCode, String, String) {
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    (
        status,
        body["kind"].as_str().unwrap_or_default().to_owned(),
        body["message"].as_str().unwrap_or_default().to_owned(),
    )
}

#[tokio::test]
async fn commit_and_push_use_a_bare_origin_and_refuse_a_clean_tree() {
    let (router, token, _runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "first change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    let clean = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(clean).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "nothing_to_commit");

    std::fs::write(path.join("extra.txt"), "line\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = committed.json().await.unwrap();
    assert_eq!(
        body["message"],
        "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)"
    );
    assert_eq!(body["stat"]["files"], 1);

    let again = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "unused" }))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(again).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "nothing_to_commit");

    let pushed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), reqwest::StatusCode::OK);
    let push_body: serde_json::Value = pushed.json().await.unwrap();
    assert_eq!(push_body["remote"], "origin");
    assert_eq!(push_body["branch"], workspace["branch_name"]);

    let digest = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(digest.status(), reqwest::StatusCode::OK);
    let pr: serde_json::Value = digest.json().await.unwrap();
    assert_eq!(pr["dirty"], false);
    assert_eq!(pr["unpushed"], false);
    assert!(pr["ahead"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn pr_create_and_view_use_path_shims_and_persist_the_digest() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "first change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    std::fs::write(path.join("extra.txt"), "line\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    let pushed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), reqwest::StatusCode::OK);

    let empty = tempfile::TempDir::new().unwrap();
    runtime.set_gh_search_path(Some(empty.path().display().to_string()));
    let absent = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/pr"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(absent).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "gh_absent");
    assert!(message.contains("gh pr create"), "{message}");
    assert!(message.contains("git push"), "{message}");

    let signed_out_dir = tempfile::TempDir::new().unwrap();
    write_executable(
        &signed_out_dir.path().join("gh"),
        "#!/bin/sh\nif [ \"$1\" = auth ]; then echo signed out >&2; exit 1; fi\nexit 3\n",
    );
    runtime.set_gh_search_path(Some(signed_out_dir.path().display().to_string()));
    let signed_out = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/pr"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(signed_out).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "gh_signed_out");
    assert!(message.contains("gh auth login"), "{message}");

    let shim_dir = tempfile::TempDir::new().unwrap();
    let log = shim_dir.path().join("log");
    write_executable(
        &shim_dir.path().join("gh"),
        &format!(
            r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = merge ] || [ "$2" = merge ]; then exit 2; fi
if [ "$1" = auth ]; then
  echo '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"tester"}}]}}}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = create ]; then
  echo https://github.com/example/demo/pull/12
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  echo '{{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN","headRefOid":"aaaaaaaa"}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = checks ]; then
  printf 'lint\tpass\t1s\thttps://example.test/lint\n'
  exit 0
fi
exit 3
"#,
            log = log.display()
        ),
    );
    runtime.set_gh_search_path(Some(shim_dir.path().display().to_string()));
    let created = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/pr"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let pr: serde_json::Value = created.json().await.unwrap();
    assert_eq!(pr["pr"]["number"], 12);
    assert_eq!(pr["pr"]["state"], "open");
    assert_eq!(
        pr["pr"]["checks_summary"],
        "1 passing, 0 pending, 0 failing"
    );

    let listed = client
        .get(format!("http://{addr}/code/workspaces/{id}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed["pr"]["number"], 12);

    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("pr create"), "{logged}");
    assert!(
        logged.contains("--base main"),
        "gh pr create must pass the workspace base: {logged}"
    );
    assert!(logged.contains("pr view"), "{logged}");
    assert!(logged.contains("pr checks"), "{logged}");
    // The view field list legitimately names mergeable/autoMergeRequest; a
    // merge invocation or flag may never appear on the create/status path.
    assert!(!logged.contains("pr merge"), "{logged}");
    assert!(!logged.contains("--merge"), "{logged}");
    assert!(!logged.contains("--auto"), "{logged}");
    assert!(!logged.contains("graphql"), "{logged}");
}

#[tokio::test]
async fn quick_actions_return_bounded_output_and_do_not_need_a_session() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (repo_body, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "actions").await;
    let repo_id: RepoId = json_id(&repo_body).parse().unwrap();
    let mut stored = runtime
        .get_repo(&tidebreak_core::OwnerId::local(), repo_id)
        .await
        .unwrap();
    stored.quick_actions = vec![
        QuickAction {
            name: "echo".into(),
            command: "printf 'hello-action\\n'".into(),
            auto_run_on_create: false,
        },
        QuickAction {
            name: "sleep".into(),
            command: "sleep 20".into(),
            auto_run_on_create: false,
        },
    ];
    runtime.save_repo(&stored).await.unwrap();
    let id = json_id(&workspace);

    let echoed = client
        .post(format!("http://{addr}/code/workspaces/{id}/actions/echo"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = echoed.json().await.unwrap();
    assert_eq!(body["name"], "echo");
    assert_eq!(body["success"], true);
    assert!(body["stdout"].as_str().unwrap().contains("hello-action"));
    assert_eq!(body["timed_out"], false);

    let missing = client
        .post(format!("http://{addr}/code/workspaces/{id}/actions/nope"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let timed = client
        .post(format!("http://{addr}/code/workspaces/{id}/actions/sleep"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(timed.status(), reqwest::StatusCode::OK);
    let timed_body: serde_json::Value = timed.json().await.unwrap();
    assert_eq!(timed_body["timed_out"], true);
    assert_eq!(timed_body["success"], false);
}

#[tokio::test]
async fn auto_run_on_create_runs_after_setup() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    let repo_body: serde_json::Value = registered.json().await.unwrap();
    let repo_id: RepoId = json_id(&repo_body).parse().unwrap();
    let mut stored = runtime
        .get_repo(&tidebreak_core::OwnerId::local(), repo_id)
        .await
        .unwrap();
    stored.quick_actions = vec![QuickAction {
        name: "stamp".into(),
        command: "printf 'auto\\n' > stamp.txt".into(),
        auto_run_on_create: true,
    }];
    runtime.save_repo(&stored).await.unwrap();

    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "auto action",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let stamped = std::fs::read_to_string(path.join("stamp.txt")).unwrap();
    assert_eq!(stamped, "auto\n");
}

/// Decision 63: a hosted machine's push borrows nothing for a local origin
/// and keeps working exactly as before; the git card names the acting App
/// identity only once the checkout's origin is a forge repository; and a
/// gateway refusal fails the push with its reason before git runs.
#[tokio::test]
async fn a_hosted_push_borrows_only_for_forge_origins_and_fails_closed() {
    let lender = Arc::new(FakeLender::offering("acme-ship[bot]"));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    // Status reads on a forge checkout now refresh the digest over REST
    // (decision 65); a dead loopback port keeps this test off the network
    // while the borrow accounting below stays observable.
    runtime.set_forge_api_base(Some("http://127.0.0.1:9/".to_owned()));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "hosted change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    // A local bare origin is not a forge repository: nothing is borrowed and
    // the push lands exactly as it always has.
    std::fs::write(path.join("extra.txt"), "line\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    let pushed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), reqwest::StatusCode::OK);
    assert!(
        lender.minted().is_empty(),
        "a local origin must borrow nothing"
    );

    // The identity line follows the origin: absent for the local origin,
    // present once the checkout points at a forge repository.
    let status = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = status.json().await.unwrap();
    assert!(body["pushes_as"].is_null(), "{body}");

    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    let status = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = status.json().await.unwrap();
    assert_eq!(body["pushes_as"], "acme-ship[bot]", "{body}");
    assert!(
        body["pushes_as_self"].is_null(),
        "the App's identity is never claimed as the caller's own: {body}"
    );
    // The forge checkout's status read borrows once for the REST digest
    // (decision 65); the read fails on the dead port and degrades silently.
    let digest_borrows = lender.minted().len();
    assert!(digest_borrows > 0, "the digest read borrows per operation");
    assert!(
        lender
            .minted()
            .iter()
            .all(|repository| repository == "acme/demo"),
        "every borrow names the one forge repository: {:?}",
        lender.minted()
    );

    // A rewritten origin on a foreign host — the exact move an agent in the
    // workspace could make — is outside the lending: no identity is claimed
    // and, through the same gate, no credential would be borrowed.
    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://evil.example/acme/private.git",
        ],
    );
    let status = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = status.json().await.unwrap();
    assert!(body["pushes_as"].is_null(), "{body}");
    assert_eq!(
        lender.minted().len(),
        digest_borrows,
        "a foreign host must never reach the gateway"
    );
    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );

    // A refusal stops the push with the gateway's reason before git runs —
    // nothing reaches the network in this test.
    *lender.mint_refusal.lock().unwrap() = Some(GitForgeError::NoGitForge);
    let refused = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(refused).await;
    assert_eq!(status, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(kind, "git_forge_refused");
    assert!(message.contains("no git forge"), "{message}");
    assert_eq!(
        lender.minted().len(),
        digest_borrows + 1,
        "the refused push asked exactly once more"
    );
    assert!(
        lender
            .minted()
            .iter()
            .all(|repository| repository == "acme/demo"),
        "{:?}",
        lender.minted()
    );
}

/// Decision 65: once the caller's own account acts, the git card names them
/// and states the identity is their own rather than the App's.
#[tokio::test]
async fn a_hosted_git_card_names_the_person_once_connected() {
    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_forge_api_base(Some("http://127.0.0.1:9/".to_owned()));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "person change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    let status = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = status.json().await.unwrap();
    assert_eq!(body["pushes_as"], "mira-chen", "{body}");
    assert_eq!(body["pushes_as_self"], true, "{body}");
}

/// Decision 65: a workspace on a machine whose gateway names the caller
/// commits as that person — author and committer both, scoped to the
/// worktree so the shared clone keeps its own configuration.
#[tokio::test]
async fn a_hosted_workspace_commits_as_the_person() {
    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, _runtime, dir) =
        code_app_with(Some(lender as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "person commit").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    std::fs::write(path.join("named.txt"), "line\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);

    let signature = run_stdout(&path, &["git", "log", "-1", "--format=%an <%ae>|%cn <%ce>"]);
    let expected = "Mira Chen <8675309+mira-chen@users.noreply.github.com>";
    assert_eq!(signature, format!("{expected}|{expected}"));
    // The identity is the worktree's alone: the registered clone still
    // answers with its own configuration.
    assert_eq!(run_stdout(&repo, &["git", "config", "user.name"]), "Dev");
}

/// Machines that lend nothing — and hosted machines whose forge still
/// attributes to the App — leave the checkout's own identity untouched.
#[tokio::test]
async fn a_workspace_without_a_person_identity_keeps_the_checkouts_own() {
    for lender in [
        None,
        Some(Arc::new(FakeLender::offering("acme-ship[bot]")) as Arc<dyn GitCredentialLender>),
    ] {
        let (router, token, _runtime, dir) = code_app_with(lender).await;
        let addr = serve(router).await;
        let client = reqwest::Client::new();
        let repo = init_paired_repo(dir.path());
        let (_repo, workspace) =
            register_and_workspace(&client, addr, &token, &repo, "own identity").await;
        let id = json_id(&workspace);
        let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

        std::fs::write(path.join("plain.txt"), "line\n").unwrap();
        let committed = client
            .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(committed.status(), reqwest::StatusCode::OK);
        let signature = run_stdout(&path, &["git", "log", "-1", "--format=%an <%ae>"]);
        assert_eq!(signature, "Dev <dev@example.com>");
    }
}

/// Decision 65: with no `gh` anywhere, a hosted machine creates the pull
/// request over the forge REST API with a borrowed credential, persists the
/// authored fact from the creation answer, and the card's digest — state,
/// checks, queue — reads over the same surface. A gateway refusal fails the
/// operation with the gateway's reason before anything reaches the forge.
#[tokio::test]
async fn a_hosted_machine_creates_and_reads_the_pull_request_over_rest() {
    type Recorded = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;
    let recorded: Recorded = Arc::default();

    fn rest_pull_request(head_ref: &str) -> serde_json::Value {
        serde_json::json!({
            "number": 7,
            "html_url": "https://github.com/acme/demo/pull/7",
            "title": "Add the hosted change",
            "state": "open",
            "merged": false,
            "draft": false,
            "user": { "login": "mira-chen" },
            "head": { "ref": head_ref, "sha": "feedfeedfeedfeedfeed" },
            "base": { "ref": "main" },
            "mergeable": true,
            "mergeable_state": "clean",
            "auto_merge": null,
            "created_at": "2026-08-24T10:00:00Z",
            "updated_at": "2026-08-24T10:00:00Z",
            "merged_at": null,
            "closed_at": null,
        })
    }

    let create_recorded = Arc::clone(&recorded);
    let create = move |headers: axum::http::HeaderMap,
                       axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&create_recorded);
        async move {
            // The borrowed credential rides as the bearer — asserted
            // server-side so a drifted client fails the test.
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            assert_eq!(bearer, "Bearer ghs_fake_borrowed");
            let head = body["head"].as_str().unwrap_or_default().to_owned();
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(body);
            (
                axum::http::StatusCode::CREATED,
                axum::Json(rest_pull_request(&head)),
            )
        }
    };
    let list = || async { axum::Json(serde_json::json!([rest_pull_request("hosted-branch")])) };
    let detail = || async { axum::Json(rest_pull_request("hosted-branch")) };
    let checks = || async {
        axum::Json(serde_json::json!({
            "check_runs": [
                { "name": "test", "status": "completed", "conclusion": "success",
                  "html_url": "https://github.com/acme/demo/runs/1" },
                { "name": "clippy", "status": "in_progress", "conclusion": null,
                  "html_url": "https://github.com/acme/demo/runs/2" },
            ]
        }))
    };
    let timeline = || async { axum::Json(serde_json::json!([])) };
    let router = axum::Router::new()
        .route("/repos/acme/demo/pulls", axum::routing::post(create))
        .route("/repos/acme/demo/pulls", axum::routing::get(list))
        .route("/repos/acme/demo/pulls/7", axum::routing::get(detail))
        .route(
            "/repos/acme/demo/commits/{sha}/check-runs",
            axum::routing::get(checks),
        )
        .route(
            "/repos/acme/demo/issues/7/timeline",
            axum::routing::get(timeline),
        );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let api = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_forge_api_base(Some(format!("http://{api}")));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "hosted change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    std::fs::write(path.join("feature.txt"), "line\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );

    let created = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/pr"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "title": "Add the hosted change" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = created.json().await.unwrap();
    assert_eq!(body["pr"]["number"], 7, "{body}");
    assert_eq!(
        body["pr"]["url"], "https://github.com/acme/demo/pull/7",
        "{body}"
    );
    assert_eq!(
        body["pr"]["checks_summary"], "1 passing, 1 pending, 0 failing",
        "the digest's checks ride REST too: {body}"
    );

    let sent = recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(sent.len(), 1, "one create request reached the forge");
    assert_eq!(sent[0]["title"], "Add the hosted change");
    assert_eq!(sent[0]["base"], "main");
    assert_eq!(sent[0]["head"], workspace["branch_name"]);
    assert!(
        lender
            .minted()
            .iter()
            .all(|repository| repository == "acme/demo"),
        "every borrow names the one repository: {:?}",
        lender.minted()
    );

    // The authored fact came straight from the creation answer — no `gh`
    // exists here to re-read it (decision 62 meets decision 65).
    let facts: serde_json::Value = client
        .get(format!("http://{addr}/code/workspaces/{id}/pull-requests"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let fact = &facts["items"][0];
    assert_eq!(fact["number"], 7, "{facts}");
    assert_eq!(fact["author"], "mira-chen", "{facts}");
    assert_eq!(fact["relation"], "authored", "{facts}");
}

/// Decision 65: a gateway refusal fails the pull-request creation with the
/// gateway's reason, before anything reaches the forge.
#[tokio::test]
async fn a_hosted_pull_request_create_fails_closed_with_the_gateway_refusal() {
    let lender = Arc::new(FakeLender::refusing(GitForgeError::NotConnected {
        connect_url: Some("https://gateway.example/account/apps".to_owned()),
    }));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_forge_api_base(Some("http://127.0.0.1:9/".to_owned()));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "refused change").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );

    let refused = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/pr"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(refused).await;
    assert_eq!(status, reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(kind, "git_forge_refused");
    assert!(message.contains("connect your GitHub account"), "{message}");
}

/// The moments after a push are when the reader most wants "checks pending
/// on the new head", and the digest cache used to keep serving the pre-push
/// snapshot for its whole TTL (decision 66).
#[tokio::test]
async fn a_push_drops_the_cached_pull_request_digest() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "digest freshness").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    // A gh shim that answers `pr view` from a file, so the test moves
    // GitHub's state without waiting out any cache.
    let shim_dir = tempfile::TempDir::new().unwrap();
    let answer = shim_dir.path().join("view.json");
    std::fs::write(
        &answer,
        r#"{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN","headRefOid":"aaaaaaaa"}"#,
    )
    .unwrap();
    write_executable(
        &shim_dir.path().join("gh"),
        &format!(
            r#"#!/bin/sh
if [ "$1" = auth ]; then
  echo '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"tester"}}]}}}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  cat {answer}
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = checks ]; then
  printf 'lint\tpass\t1s\thttps://example.test/lint\n'
  exit 0
fi
exit 3
"#,
            answer = answer.display()
        ),
    );
    runtime.set_gh_search_path(Some(shim_dir.path().display().to_string()));

    std::fs::write(path.join("extra.txt"), "one\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    let pushed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), reqwest::StatusCode::OK);

    let first = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(first["pr"]["head_sha"], "aaaaaaaa");

    // GitHub's answer moves, as it does when new commits land on the head.
    std::fs::write(
        &answer,
        r#"{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN","headRefOid":"bbbbbbbb"}"#,
    )
    .unwrap();

    // Within the TTL the cache still serves the old head...
    let cached = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(cached["pr"]["head_sha"], "aaaaaaaa");

    // ...and a push drops it, so the next read carries the new head.
    std::fs::write(path.join("extra.txt"), "two\n").unwrap();
    let committed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/commit"))
        .bearer_auth(&token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(committed.status(), reqwest::StatusCode::OK);
    let pushed = client
        .post(format!("http://{addr}/code/workspaces/{id}/git/push"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), reqwest::StatusCode::OK);

    let fresh = client
        .get(format!("http://{addr}/code/workspaces/{id}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(fresh["pr"]["head_sha"], "bbbbbbbb");
}
