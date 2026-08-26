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
    code_app_with_principal(lender, None).await
}

async fn code_app_with_principal(
    lender: Option<Arc<dyn GitCredentialLender>>,
    member_token: Option<&str>,
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
    let mut config = Config::desktop(dir.path());
    if let Some(member_token) = member_token {
        let tokens_file = dir.path().join("tokens");
        std::fs::write(
            &tokens_file,
            format!("alice {ALICE_TOKEN} admin\nbob {member_token}\n"),
        )
        .unwrap();
        config.profile = tidebreak_core::Profile::SelfHost;
        config.auth_tokens_file = Some(tokens_file);
    }
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
    let token = member_token
        .map(Arc::<str>::from)
        .unwrap_or_else(|| state.token.clone());
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

/// Store a minimal pull-request digest on the workspace row, as the create
/// path does the moment a pull request exists: the URL is the identity the
/// conditional fetcher (decision 66) resolves everything else from.
async fn seed_workspace_pull_request(runtime: &CodeRuntime, id: &str, url: &str, number: u64) {
    let owner = tidebreak_core::OwnerId::local();
    let id: tidebreak_core::WorkspaceId = id.parse().unwrap();
    let mut workspace = runtime.get_workspace(&owner, id).await.unwrap();
    workspace.pr = Some(tidebreak_core::PullRequestDigest {
        number,
        url: Some(url.to_owned()),
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
    });
    runtime.save_workspace(&workspace).await.unwrap();
}

struct MergeFixture {
    client: reqwest::Client,
    addr: std::net::SocketAddr,
    token: Arc<str>,
    runtime: Arc<CodeRuntime>,
    _data: tempfile::TempDir,
    _shim: tempfile::TempDir,
    id: String,
    repo_id: RepoId,
    path: std::path::PathBuf,
    branch: String,
    head: String,
    log: std::path::PathBuf,
}

async fn merge_fixture(
    live_repository: &str,
    live_number: u64,
    live_head: Option<&str>,
) -> MergeFixture {
    let (router, token, runtime, data) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(data.path());
    let (repo_body, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "exact merge").await;
    let repo_id: RepoId = json_id(&repo_body).parse().unwrap();
    let id = json_id(&workspace).to_owned();
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());
    let branch = workspace["branch_name"].as_str().unwrap().to_owned();
    run(&path, &["git", "push", "-u", "origin", &branch]);
    let head = run_stdout(&path, &["git", "rev-parse", "HEAD"]);
    run(
        &path,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/example/demo.git",
        ],
    );
    seed_workspace_pull_request(&runtime, &id, "https://github.com/example/demo/pull/12", 12).await;

    let shim = tempfile::TempDir::new().unwrap();
    let log = shim.path().join("log");
    let live_head = live_head.unwrap_or(&head);
    let (live_owner, live_name) = live_repository.split_once('/').unwrap();
    write_executable(
        &shim.path().join("gh"),
        &format!(
            r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = auth ]; then
  echo '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"tester"}}]}}}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  echo '{{"number":{live_number},"url":"https://github.com/{live_owner}/{live_name}/pull/{live_number}","state":"OPEN","headRefName":"{branch}","headRefOid":"{live_head}"}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = merge ]; then exit 0; fi
exit 3
"#,
            log = log.display(),
        ),
    );
    runtime.set_gh_search_path(Some(shim.path().display().to_string()));
    MergeFixture {
        client,
        addr,
        token,
        runtime,
        _data: data,
        _shim: shim,
        id,
        repo_id,
        path,
        branch,
        head,
        log,
    }
}

fn exact_merge_body(head: &str) -> serde_json::Value {
    serde_json::json!({
        "target": {
            "repository": {
                "host": "github.com",
                "owner": "example",
                "name": "demo"
            },
            "number": 12
        },
        "expected_head_sha": head,
        "method": "squash",
        "auto": false
    })
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
async fn workspace_merge_requires_the_exact_reviewed_target() {
    let (router, token, _runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "exact merge body").await;
    let id = json_id(&workspace);
    let response = client
        .post(format!("http://{addr}/code/workspaces/{id}/pr/merge"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "method": "squash", "auto": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workspace_merge_rechecks_and_uses_the_repository_qualified_head() {
    let fixture = merge_fixture("example/demo", 12, None).await;
    let response = fixture
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            fixture.addr, fixture.id
        ))
        .bearer_auth(&fixture.token)
        .json(&exact_merge_body(&fixture.head))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["target"]["repository"]["host"], "github.com");
    assert_eq!(body["target"]["repository"]["owner"], "example");
    assert_eq!(body["target"]["repository"]["name"], "demo");
    assert_eq!(body["target"]["number"], 12);
    assert_eq!(body["accepted_head_sha"], fixture.head);
    assert!(body["status"].is_object(), "{body}");

    let logged = std::fs::read_to_string(&fixture.log).unwrap();
    assert!(logged.contains("pr view --json"), "{logged}");
    assert!(
        logged.contains(&format!(
            "pr merge 12 --repo example/demo --squash --match-head-commit {}",
            fixture.head
        )),
        "{logged}"
    );
}

#[tokio::test]
async fn workspace_merge_returns_typed_local_and_remote_conflicts() {
    let dirty = merge_fixture("example/demo", 12, None).await;
    std::fs::write(dirty.path.join("dirty.txt"), "changed\n").unwrap();
    let response = dirty
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            dirty.addr, dirty.id
        ))
        .bearer_auth(&dirty.token)
        .json(&exact_merge_body(&dirty.head))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "workspace_dirty");
    assert!(!dirty.log.exists(), "dirty work must fail before gh runs");

    let unpushed = merge_fixture("example/demo", 12, None).await;
    std::fs::write(unpushed.path.join("ahead.txt"), "ahead\n").unwrap();
    run(&unpushed.path, &["git", "add", "ahead.txt"]);
    run(&unpushed.path, &["git", "commit", "-m", "ahead"]);
    let response = unpushed
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            unpushed.addr, unpushed.id
        ))
        .bearer_auth(&unpushed.token)
        .json(&exact_merge_body(&unpushed.head))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "workspace_unpushed");
    assert!(
        !unpushed.log.exists(),
        "unpushed work must fail before gh runs"
    );

    let no_upstream = merge_fixture("example/demo", 12, None).await;
    run(&no_upstream.path, &["git", "branch", "--unset-upstream"]);
    let response = no_upstream
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            no_upstream.addr, no_upstream.id
        ))
        .bearer_auth(&no_upstream.token)
        .json(&exact_merge_body(&no_upstream.head))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "workspace_upstream_missing");

    let changed_branch = merge_fixture("example/demo", 12, None).await;
    run(
        &changed_branch.path,
        &["git", "switch", "-c", "other-branch"],
    );
    let response = changed_branch
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            changed_branch.addr, changed_branch.id
        ))
        .bearer_auth(&changed_branch.token)
        .json(&exact_merge_body(&changed_branch.head))
        .send()
        .await
        .unwrap();
    let (status, kind, _) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "workspace_branch_changed");

    let moved_head = merge_fixture("example/demo", 12, Some("bbbbbbbbbbbbbbbb")).await;
    let response = moved_head
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            moved_head.addr, moved_head.id
        ))
        .bearer_auth(&moved_head.token)
        .json(&exact_merge_body(&moved_head.head))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "pr_head_changed");
    assert!(message.contains(&moved_head.head[..8]), "{message}");
    assert!(message.contains("bbbbbbbb"), "{message}");
    let logged = std::fs::read_to_string(&moved_head.log).unwrap();
    assert!(!logged.contains("pr merge"), "{logged}");

    let moved_target = merge_fixture("example/other", 19, None).await;
    let response = moved_target
        .client
        .post(format!(
            "http://{}/code/workspaces/{}/pr/merge",
            moved_target.addr, moved_target.id
        ))
        .bearer_auth(&moved_target.token)
        .json(&exact_merge_body(&moved_target.head))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "pr_target_changed");
    assert!(message.contains("example/other#19"), "{message}");
    let logged = std::fs::read_to_string(&moved_target.log).unwrap();
    assert!(!logged.contains("pr merge"), "{logged}");
}

#[tokio::test]
async fn workspace_merge_holds_the_turn_lock_through_the_host_action() {
    let fixture = merge_fixture("example/demo", 12, None).await;
    let entered = fixture._shim.path().join("view-entered");
    let release = fixture._shim.path().join("release-view");
    write_executable(
        &fixture._shim.path().join("gh"),
        &format!(
            r#"#!/bin/sh
echo "$@" >> {log}
if [ "$1" = auth ]; then
  echo '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"tester"}}]}}}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  : > {entered}
  while [ ! -f {release} ]; do sleep 0.02; done
  echo '{{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN","headRefName":"{branch}","headRefOid":"{head}"}}'
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = merge ]; then exit 0; fi
exit 3
"#,
            log = fixture.log.display(),
            entered = entered.display(),
            release = release.display(),
            branch = fixture.branch,
            head = fixture.head,
        ),
    );
    let mut repo = fixture
        .runtime
        .get_repo(&tidebreak_core::OwnerId::local(), fixture.repo_id)
        .await
        .unwrap();
    repo.quick_actions = vec![QuickAction {
        name: "mutate".into(),
        command: "printf 'changed\\n' > action.txt".into(),
        auto_run_on_create: false,
    }];
    fixture.runtime.save_repo(&repo).await.unwrap();

    let merge_client = fixture.client.clone();
    let merge_token = fixture.token.clone();
    let merge_url = format!(
        "http://{}/code/workspaces/{}/pr/merge",
        fixture.addr, fixture.id
    );
    let merge_body = exact_merge_body(&fixture.head);
    let merge = tokio::spawn(async move {
        merge_client
            .post(merge_url)
            .bearer_auth(merge_token)
            .json(&merge_body)
            .send()
            .await
            .unwrap()
    });
    for _ in 0..100 {
        if entered.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        entered.exists(),
        "the merge did not reach its locked host read"
    );

    let action_client = fixture.client.clone();
    let action_token = fixture.token.clone();
    let action_url = format!(
        "http://{}/code/workspaces/{}/actions/mutate",
        fixture.addr, fixture.id
    );
    let action = tokio::spawn(async move {
        action_client
            .post(action_url)
            .bearer_auth(action_token)
            .send()
            .await
            .unwrap()
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !fixture.path.join("action.txt").exists(),
        "another workspace action ran inside the merge preflight"
    );

    std::fs::write(&release, "go\n").unwrap();
    assert_eq!(merge.await.unwrap().status(), reqwest::StatusCode::OK);
    assert_eq!(action.await.unwrap().status(), reqwest::StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(fixture.path.join("action.txt")).unwrap(),
        "changed\n"
    );
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
if [ "$1" = api ]; then
  case "$*" in
    *pulls/12/reviews*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *rules/branches/main*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *check-runs*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'
      echo '{{"check_runs":[{{"name":"lint","conclusion":"success","html_url":"https://example.test/lint"}}]}}'
      exit 0;;
    *pulls/12*)
      printf 'HTTP/2.0 200 OK\r\nEtag: W/"pull-1"\r\n\r\n'
      echo '{{"number":12,"html_url":"https://github.com/example/demo/pull/12","state":"open","head":{{"ref":"feature","sha":"aaaaaaaa"}},"base":{{"ref":"main"}}}}'
      exit 0;;
  esac
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
    // The status read is the conditional REST fetcher (decision 66): the
    // pull request, its head's check runs, and its reviews — never a
    // `pr checks` table read or a merge/GraphQL invocation.
    assert!(logged.contains("repos/example/demo/pulls/12"), "{logged}");
    assert!(logged.contains("check-runs"), "{logged}");
    assert!(!logged.contains("pr checks"), "{logged}");
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
/// on the new head" (decision 66): the push refreshes the row before it
/// answers, and a plain read never serves anything but that row.
#[tokio::test]
async fn a_push_refreshes_the_pull_request_row_immediately() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (_repo, workspace) =
        register_and_workspace(&client, addr, &token, &repo, "digest freshness").await;
    let id = json_id(&workspace);
    let path = std::path::PathBuf::from(workspace["worktree_path"].as_str().unwrap());

    // The workspace already knows its pull request by URL, as it does the
    // moment one exists; the fetcher resolves identity from that URL.
    seed_workspace_pull_request(&runtime, id, "https://github.com/example/demo/pull/12", 12).await;

    // A gh shim that answers the REST reads from a file, so the test moves
    // GitHub's state without waiting out any cache.
    let shim_dir = tempfile::TempDir::new().unwrap();
    let answer = shim_dir.path().join("pull.json");
    std::fs::write(
        &answer,
        r#"{"number":12,"html_url":"https://github.com/example/demo/pull/12","state":"open","head":{"ref":"feature","sha":"aaaaaaaa"},"base":{"ref":"main"}}"#,
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
if [ "$1" = api ]; then
  case "$*" in
    *pulls/12/reviews*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *rules/branches/main*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *check-runs*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'
      echo '{{"check_runs":[{{"name":"lint","conclusion":"success","html_url":"https://example.test/lint"}}]}}'
      exit 0;;
    *pulls/12*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'
      cat {answer}
      exit 0;;
  esac
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
        r#"{"number":12,"html_url":"https://github.com/example/demo/pull/12","state":"open","head":{"ref":"feature","sha":"bbbbbbbb"},"base":{"ref":"main"}}"#,
    )
    .unwrap();

    // A plain read serves the stored row without a fetch of its own...
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

    // ...and a push refreshes the row immediately, so the next read carries
    // the new head.
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

/// One host read updates every workspace holding the pull request
/// (decision 66): a digest change lands on the fact row's live tier, and the
/// other holder takes the same snapshot — column and digest cache — with no
/// second GitHub read.
#[tokio::test]
async fn a_digest_change_writes_through_to_every_holder_of_the_pull_request() {
    use tidebreak_core::db::code::{get_pull_request_fact, save_pull_request_fact};
    use tidebreak_core::{CodePullRequestFact, CodePullRequestId, CodePullRequestState};

    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_paired_repo(dir.path());
    let (repo_body, workspace_a) =
        register_and_workspace(&client, addr, &token, &repo, "holder a").await;
    let a = json_id(&workspace_a).to_owned();
    let second = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repo_id": json_id(&repo_body),
            "title": "holder b",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CREATED);
    let workspace_b: serde_json::Value = second.json().await.unwrap();
    let b = json_id(&workspace_b).to_owned();

    // The decision-62 fact row the live tier decorates.
    let owner = tidebreak_core::OwnerId::local();
    let now = chrono::Utc::now();
    save_pull_request_fact(
        &runtime.db,
        &CodePullRequestFact {
            id: CodePullRequestId::new(),
            owner: owner.clone(),
            host: "github.com".into(),
            repo_owner: "example".into(),
            repo_name: "demo".into(),
            number: 12,
            url: "https://github.com/example/demo/pull/12".into(),
            title: "demo".into(),
            state: CodePullRequestState::Open,
            draft: false,
            author: None,
            head_branch: "feature".into(),
            base_branch: "main".into(),
            head_sha: Some("aaaaaaaa".into()),
            created_at: now,
            updated_at: now,
            merged_at: None,
            closed_at: None,
            first_seen_at: now,
            last_seen_at: now,
            live: None,
        },
    )
    .await
    .unwrap();

    // Both workspaces hold the pull request by URL, as they do the moment
    // one exists; the fetcher resolves identity from that URL.
    for id in [&a, &b] {
        seed_workspace_pull_request(&runtime, id, "https://github.com/example/demo/pull/12", 12)
            .await;
    }

    let shim_dir = tempfile::TempDir::new().unwrap();
    let answer = shim_dir.path().join("pull.json");
    std::fs::write(
        &answer,
        r#"{"number":12,"html_url":"https://github.com/example/demo/pull/12","state":"open","head":{"ref":"feature","sha":"aaaaaaaa"},"base":{"ref":"main"}}"#,
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
if [ "$1" = api ]; then
  case "$*" in
    *pulls/12/reviews*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *rules/branches/main*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'; echo '[]'; exit 0;;
    *check-runs*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'
      echo '{{"check_runs":[{{"name":"lint","conclusion":"success","html_url":"https://example.test/lint"}}]}}'
      exit 0;;
    *pulls/12*)
      printf 'HTTP/2.0 200 OK\r\n\r\n'
      cat {answer}
      exit 0;;
  esac
fi
exit 3
"#,
            answer = answer.display()
        ),
    );
    runtime.set_gh_search_path(Some(shim_dir.path().display().to_string()));

    // Workspace A fetches the first snapshot; the write-through hands it to
    // B's column, and B's own read serves that column with no host read of
    // its own (decision 66: the request path reads the stored row).
    let refreshed = client
        .post(format!("http://{addr}/code/workspaces/{a}/pr/refresh"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(refreshed["pr"]["head_sha"], "aaaaaaaa");
    let status_b = client
        .get(format!("http://{addr}/code/workspaces/{b}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(status_b["pr"]["head_sha"], "aaaaaaaa");

    // GitHub moves, and only workspace A reads it.
    std::fs::write(
        &answer,
        r#"{"number":12,"html_url":"https://github.com/example/demo/pull/12","state":"open","mergeable_state":"blocked","head":{"ref":"feature","sha":"bbbbbbbb"},"base":{"ref":"main"}}"#,
    )
    .unwrap();
    let refreshed = client
        .post(format!("http://{addr}/code/workspaces/{a}/pr/refresh"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(refreshed["pr"]["head_sha"], "bbbbbbbb");

    // Workspace B's row took the same snapshot without its own host read...
    let listed = client
        .get(format!("http://{addr}/code/workspaces/{b}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed["pr"]["head_sha"], "bbbbbbbb");

    // ...its next digest read serves that column with no fetch of its
    // own...
    let status_b = client
        .get(format!("http://{addr}/code/workspaces/{b}/pr"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(status_b["pr"]["head_sha"], "bbbbbbbb");

    // ...and the fact row carries the live tier.
    let fact = get_pull_request_fact(&runtime.db, &owner, "github.com", "example", "demo", 12)
        .await
        .unwrap()
        .unwrap();
    let live = fact.live.expect("the digest read writes the live tier");
    assert_eq!(live.merge_state_status.as_deref(), Some("blocked"));
}

fn assert_borrowed_forge_credential(headers: &axum::http::HeaderMap) {
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(bearer, "Bearer ghs_fake_borrowed");
}

async fn register_delivery_repository(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    root: &std::path::Path,
) {
    let response = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "path": root }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
}

/// Issues #2673 and #2700: a gateway-authenticated hosted caller reads and
/// acts through one borrowed credential per repository operation. Every
/// request stays pinned to the registered host and repository.
#[tokio::test]
async fn a_hosted_delivery_page_reads_and_acts_over_forge_rest() {
    type RecordedActions = Arc<std::sync::Mutex<Vec<serde_json::Value>>>;
    let recorded_actions: RecordedActions = Arc::default();

    fn pull_request() -> serde_json::Value {
        serde_json::json!({
            "number": 17,
            "html_url": "https://github.com/acme/demo/pull/17",
            "title": "Repair hosted delivery",
            "body": "Read the delivery detail over REST.",
            "state": "open",
            "draft": false,
            "user": {
                "login": "mira-chen",
                "avatar_url": "https://avatars.example/mira"
            },
            "head": {
                "ref": "hosted-delivery",
                "sha": "feedfeedfeedfeedfeed",
                "repo": {
                    "name": "demo",
                    "full_name": "acme/demo",
                    "owner": { "login": "acme" }
                }
            },
            "base": { "ref": "main" },
            "labels": [{ "name": "code" }],
            "assignees": [{ "login": "mira-chen" }],
            "requested_reviewers": [{ "login": "reviewer" }],
            "comments": 2,
            "changed_files": 1,
            "additions": 12,
            "deletions": 3,
            "commits": 2,
            "created_at": "2026-08-25T10:00:00Z",
            "updated_at": "2026-08-25T11:00:00Z",
            "merged_at": null,
            "closed_at": null
        })
    }

    let repository = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "name": "demo",
            "full_name": "acme/demo",
            "html_url": "https://github.com/acme/demo",
            "default_branch": "main",
            "owner": { "login": "acme" },
        }))
    };
    let pulls = |headers: axum::http::HeaderMap,
                 axum::extract::Query(query): axum::extract::Query<
        std::collections::HashMap<String, String>,
    >| async move {
        assert_borrowed_forge_credential(&headers);
        assert_eq!(query.get("state").map(String::as_str), Some("open"));
        axum::Json(serde_json::json!([pull_request()]))
    };
    let pull = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(pull_request())
    };
    let issue_comments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 100,
            "body": "Please keep the hosted path scoped.",
            "user": { "login": "reviewer" },
            "created_at": "2026-08-25T11:05:00Z"
        }]))
    };
    let reviews = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([]))
    };
    let inline_comments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([]))
    };
    let files = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "filename": "crates/tidebreak-server/src/code/delivery.rs",
            "status": "modified",
            "additions": 12,
            "deletions": 3,
            "patch": "@@ -1 +1 @@"
        }]))
    };
    let checks = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "check_runs": [{
                "name": "desktop test",
                "status": "completed",
                "conclusion": "failure",
                "details_url": "https://github.com/acme/demo/actions/runs/44"
            }, {
                "name": "clippy",
                "status": "in_progress",
                "conclusion": null,
                "html_url": "https://github.com/acme/demo/actions/runs/45"
            }]
        }))
    };
    let workflow_runs = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "workflow_runs": [{
                "id": 44,
                "run_attempt": 2,
                "status": "completed",
                "conclusion": "failure",
                "display_title": "Desktop CI",
                "name": "CI",
                "html_url": "https://github.com/acme/demo/actions/runs/44",
                "head_branch": "hosted-delivery",
                "head_sha": "feedfeedfeedfeedfeed",
                "event": "pull_request",
                "actor": { "login": "mira-chen" },
                "created_at": "2026-08-25T10:00:00Z",
                "updated_at": "2026-08-25T11:00:00Z"
            }]
        }))
    };
    let workflow_run = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "id": 44,
            "run_attempt": 2,
            "status": "completed",
            "conclusion": "failure",
            "display_title": "Desktop CI",
            "name": "CI",
            "html_url": "https://github.com/acme/demo/actions/runs/44",
            "head_branch": "hosted-delivery",
            "head_sha": "feedfeedfeedfeedfeed",
            "event": "pull_request",
            "actor": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:00:00Z",
            "updated_at": "2026-08-25T11:00:00Z"
        }))
    };
    let jobs = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "jobs": [{
                "id": 501,
                "name": "test",
                "status": "completed",
                "conclusion": "failure",
                "html_url": "https://github.com/acme/demo/actions/runs/44/job/501",
                "steps": [{ "name": "Run tests", "conclusion": "failure" }]
            }]
        }))
    };
    let deployments = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 91,
            "environment": "production",
            "ref": "hosted-delivery",
            "sha": "feedfeedfeedfeedfeed",
            "creator": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:30:00Z",
            "updated_at": "2026-08-25T10:30:00Z"
        }]))
    };
    let deployment = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!({
            "id": 91,
            "environment": "production",
            "ref": "hosted-delivery",
            "sha": "feedfeedfeedfeedfeed",
            "creator": { "login": "mira-chen" },
            "created_at": "2026-08-25T10:30:00Z",
            "updated_at": "2026-08-25T10:30:00Z"
        }))
    };
    let deployment_statuses = |headers: axum::http::HeaderMap| async move {
        assert_borrowed_forge_credential(&headers);
        axum::Json(serde_json::json!([{
            "id": 92,
            "state": "success",
            "description": "Deployed",
            "environment_url": "https://demo.example",
            "log_url": "https://github.com/acme/demo/deployments/91",
            "created_at": "2026-08-25T10:35:00Z"
        }]))
    };
    let merge_actions = Arc::clone(&recorded_actions);
    let merge = move |headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&merge_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "merge", "body": body }));
            axum::Json(serde_json::json!({
                "merged": true,
                "message": "Pull Request successfully merged"
            }))
        }
    };
    let state_actions = Arc::clone(&recorded_actions);
    let update_pull = move |headers: axum::http::HeaderMap,
                            axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&state_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "state", "body": body }));
            axum::Json(pull_request())
        }
    };
    let comment_actions = Arc::clone(&recorded_actions);
    let comment = move |headers: axum::http::HeaderMap,
                        axum::Json(body): axum::Json<serde_json::Value>| {
        let recorded = Arc::clone(&comment_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "comment", "body": body }));
            (
                axum::http::StatusCode::CREATED,
                axum::Json(serde_json::json!({ "id": 101 })),
            )
        }
    };
    let rerun_actions = Arc::clone(&recorded_actions);
    let rerun = move |headers: axum::http::HeaderMap,
                      axum::extract::Path(run_id): axum::extract::Path<u64>| {
        let recorded = Arc::clone(&rerun_actions);
        async move {
            assert_borrowed_forge_credential(&headers);
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(serde_json::json!({ "action": "rerun", "run_id": run_id }));
            axum::http::StatusCode::CREATED
        }
    };
    let rerun_failed_actions = Arc::clone(&recorded_actions);
    let rerun_failed =
        move |headers: axum::http::HeaderMap,
              axum::extract::Path(run_id): axum::extract::Path<u64>| {
            let recorded = Arc::clone(&rerun_failed_actions);
            async move {
                assert_borrowed_forge_credential(&headers);
                recorded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(serde_json::json!({
                        "action": "rerun_failed",
                        "run_id": run_id
                    }));
                axum::http::StatusCode::CREATED
            }
        };
    let forge = axum::Router::new()
        .route("/repos/acme/demo", axum::routing::get(repository))
        .route("/repos/acme/demo/pulls", axum::routing::get(pulls))
        .route(
            "/repos/acme/demo/pulls/17",
            axum::routing::get(pull).patch(update_pull),
        )
        .route("/repos/acme/demo/pulls/17/merge", axum::routing::put(merge))
        .route(
            "/repos/acme/demo/issues/17/comments",
            axum::routing::get(issue_comments).post(comment),
        )
        .route(
            "/repos/acme/demo/pulls/17/reviews",
            axum::routing::get(reviews),
        )
        .route(
            "/repos/acme/demo/pulls/17/comments",
            axum::routing::get(inline_comments),
        )
        .route("/repos/acme/demo/pulls/17/files", axum::routing::get(files))
        .route(
            "/repos/acme/demo/commits/{sha}/check-runs",
            axum::routing::get(checks),
        )
        .route(
            "/repos/acme/demo/actions/runs",
            axum::routing::get(workflow_runs),
        )
        .route(
            "/repos/acme/demo/actions/runs/44",
            axum::routing::get(workflow_run),
        )
        .route(
            "/repos/acme/demo/actions/runs/44/jobs",
            axum::routing::get(jobs),
        )
        .route(
            "/repos/acme/demo/actions/runs/{run_id}/rerun",
            axum::routing::post(rerun),
        )
        .route(
            "/repos/acme/demo/actions/runs/{run_id}/rerun-failed-jobs",
            axum::routing::post(rerun_failed),
        )
        .route(
            "/repos/acme/demo/deployments",
            axum::routing::get(deployments),
        )
        .route(
            "/repos/acme/demo/deployments/91",
            axum::routing::get(deployment),
        )
        .route(
            "/repos/acme/demo/deployments/91/statuses",
            axum::routing::get(deployment_statuses),
        );
    let forge_addr = serve(forge).await;

    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_gh_search_path(Some("/path/with/no/gh".into()));
    runtime.set_forge_api_base(Some(format!("http://{forge_addr}")));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let root = init_paired_repo(dir.path());
    run(
        &root,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    register_delivery_repository(&client, addr, &token, &root).await;

    let repositories: serde_json::Value = client
        .get(format!("http://{addr}/code/delivery/repositories"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(repositories["capability"]["authenticated"], true);
    assert_eq!(repositories["capability"]["viewer_login"], "mira-chen");
    assert_eq!(
        repositories["repositories"][0]["name_with_owner"],
        "acme/demo"
    );
    assert_eq!(repositories["repositories"][0]["default_branch"], "main");

    let target = serde_json::json!({
        "host": "github.com",
        "owner": "acme",
        "name": "demo"
    });
    let pull_requests: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/pull-requests/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repositories": [target.clone()],
            "states": ["open"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pull_requests["items"][0]["number"], 17);
    assert_eq!(pull_requests["items"][0]["comment_count"], 2);
    assert_eq!(
        pull_requests["items"][0]["checks"][0]["name"],
        "desktop test"
    );
    assert_eq!(pull_requests["items"][0]["checks"][0]["bucket"], "fail");
    assert_eq!(
        pull_requests["items"][0]["checks"][0]["workflow_run_id"],
        44
    );
    assert_eq!(pull_requests["items"][0]["checks"][1]["bucket"], "pending");
    assert_eq!(
        pull_requests["items"][0]["checks"][1]["workflow_run_id"],
        45
    );
    assert_eq!(
        pull_requests["items"][0]["attention_reasons"][0],
        "checks_failed"
    );

    let pull_request_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/pull-requests/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target.clone(),
            "number": 17
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pull_request_detail["summary"]["number"], 17);
    assert_eq!(
        pull_request_detail["body"],
        "Read the delivery detail over REST."
    );
    assert_eq!(pull_request_detail["comments"][0]["author"], "reviewer");
    assert_eq!(
        pull_request_detail["files"][0]["path"],
        "crates/tidebreak-server/src/code/delivery.rs"
    );

    let runs: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repositories": [target.clone()] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs["items"].as_array().unwrap().len(), 2, "{runs}");
    assert!(runs["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "workflow_run"));
    assert!(runs["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["kind"] == "deployment"));

    let workflow_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target.clone(),
            "kind": "workflow_run",
            "id": 44
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(workflow_detail["summary"]["github_id"], 44);
    assert_eq!(workflow_detail["jobs"][0]["name"], "test");
    assert_eq!(workflow_detail["jobs"][0]["failed_steps"][0], "Run tests");

    let deployment_detail: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/runs/detail"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repository": target,
            "kind": "deployment",
            "id": 91
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(deployment_detail["summary"]["github_id"], 91);
    assert_eq!(
        deployment_detail["deployment_statuses"][0]["state"],
        "success"
    );

    for body in [
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": {
                "type": "merge",
                "method": "squash",
                "auto": false,
                "admin": false,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "close" }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "reopen" }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": { "type": "comment", "body": "  Ship this change.  " }
        }),
        serde_json::json!({
            "target": { "repository": target.clone(), "number": 17 },
            "action": {
                "type": "rerun_failed",
                "workflow_run_ids": [45, 44, 44]
            }
        }),
    ] {
        let response = client
            .post(format!("http://{addr}/code/delivery/pull-requests/action"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    for action in ["rerun", "rerun_failed"] {
        let response = client
            .post(format!("http://{addr}/code/delivery/runs/action"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "target": {
                    "repository": target.clone(),
                    "kind": "workflow_run",
                    "id": 44
                },
                "action": { "type": action }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    let borrows_before_unsupported = lender.minted().len();
    for (action, expected_kind) in [
        (
            serde_json::json!({ "type": "mark_ready" }),
            "git_forge_mark_ready_unsupported",
        ),
        (
            serde_json::json!({
                "type": "merge",
                "method": "squash",
                "auto": true,
                "admin": false,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }),
            "git_forge_auto_merge_unsupported",
        ),
        (
            serde_json::json!({
                "type": "merge",
                "method": "squash",
                "auto": false,
                "admin": true,
                "expected_head_sha": "feedfeedfeedfeedfeed"
            }),
            "git_forge_admin_merge_unsupported",
        ),
    ] {
        let response = client
            .post(format!("http://{addr}/code/delivery/pull-requests/action"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "target": { "repository": target.clone(), "number": 17 },
                "action": action
            }))
            .send()
            .await
            .unwrap();
        let (status, kind, message) = error_kind(response).await;
        assert_eq!(status, reqwest::StatusCode::CONFLICT);
        assert_eq!(kind, expected_kind);
        assert!(
            message.contains("Open the pull request on GitHub"),
            "{message}"
        );
    }
    assert_eq!(
        lender.minted().len(),
        borrows_before_unsupported,
        "unsupported hosted actions do not borrow a credential"
    );

    let actions = recorded_actions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(actions.contains(&serde_json::json!({
        "action": "merge",
        "body": {
            "sha": "feedfeedfeedfeedfeed",
            "merge_method": "squash"
        }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "state",
        "body": { "state": "closed" }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "state",
        "body": { "state": "open" }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "comment",
        "body": { "body": "Ship this change." }
    })));
    assert!(actions.contains(&serde_json::json!({
        "action": "rerun",
        "run_id": 44
    })));
    assert_eq!(
        actions
            .iter()
            .filter(|action| action["action"] == "rerun_failed")
            .count(),
        3,
        "two pull request runs and one run-detail action reach the failed-jobs endpoint"
    );
    assert_eq!(
        lender.minted(),
        vec!["acme/demo".to_owned(); 13],
        "every read and action borrows only for the registered repository"
    );
}

/// Issue #2700: the forge's action failure reaches the caller with its
/// actionable message, and an unregistered host or repository cannot mint a
/// credential or reach the REST origin.
#[tokio::test]
async fn hosted_delivery_actions_propagate_failures_and_pin_the_target() {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let merge_requests = Arc::clone(&requests);
    let merge = move |headers: axum::http::HeaderMap| {
        let requests = Arc::clone(&merge_requests);
        async move {
            assert_borrowed_forge_credential(&headers);
            requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                axum::http::StatusCode::CONFLICT,
                axum::Json(serde_json::json!({
                    "message": "Head branch was modified. Review and try the merge again."
                })),
            )
        }
    };
    let forge =
        axum::Router::new().route("/repos/acme/demo/pulls/17/merge", axum::routing::put(merge));
    let forge_addr = serve(forge).await;

    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, runtime, dir) = code_app_with_principal(
        Some(lender.clone() as Arc<dyn GitCredentialLender>),
        Some(BOB_TOKEN),
    )
    .await;
    runtime.set_gh_search_path(Some("/path/with/no/gh".into()));
    runtime.set_forge_api_base(Some(format!("http://{forge_addr}")));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let root = init_paired_repo(dir.path());
    run(
        &root,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    register_delivery_repository(&client, addr, &token, &root).await;

    let merge_action = |repository: serde_json::Value| {
        serde_json::json!({
            "target": { "repository": repository, "number": 17 },
            "action": {
                "type": "merge",
                "method": "squash",
                "auto": false,
                "admin": false,
                "expected_head_sha": "old-head"
            }
        })
    };
    let response = client
        .post(format!("http://{addr}/code/delivery/pull-requests/action"))
        .bearer_auth(&token)
        .json(&merge_action(serde_json::json!({
            "host": "github.com",
            "owner": "acme",
            "name": "demo"
        })))
        .send()
        .await
        .unwrap();
    let (status, kind, message) = error_kind(response).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(kind, "pr_head_changed");
    assert!(message.contains("Head branch was modified"), "{message}");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(lender.minted(), vec!["acme/demo"]);

    for repository in [
        serde_json::json!({
            "host": "ghe.example",
            "owner": "acme",
            "name": "demo"
        }),
        serde_json::json!({
            "host": "github.com",
            "owner": "acme",
            "name": "other"
        }),
    ] {
        let response = client
            .post(format!("http://{addr}/code/delivery/pull-requests/action"))
            .bearer_auth(&token)
            .json(&merge_action(repository))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    }
    assert_eq!(
        requests.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "unregistered targets never reach the forge"
    );
    assert_eq!(
        lender.minted(),
        vec!["acme/demo"],
        "unregistered targets never mint a credential"
    );
}

/// Issue #2673: a hosted caller who has not connected their forge gets the
/// gateway's connect path. The response never points at a terminal on a
/// machine where the caller cannot run `gh auth login`.
#[tokio::test]
async fn an_unconnected_hosted_delivery_page_never_recommends_gh_login() {
    let connect_url = "https://gateway.example/account/apps";
    let lender = Arc::new(FakeLender::refusing(GitForgeError::NotConnected {
        connect_url: Some(connect_url.to_owned()),
    }));
    let (router, token, runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    runtime.set_gh_search_path(Some("/path/with/no/gh".into()));
    runtime.set_forge_api_base(Some("http://127.0.0.1:9".into()));
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let root = init_paired_repo(dir.path());
    run(
        &root,
        &[
            "git",
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/demo.git",
        ],
    );
    register_delivery_repository(&client, addr, &token, &root).await;

    let repositories: serde_json::Value = client
        .get(format!("http://{addr}/code/delivery/repositories"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let remediation = repositories["capability"]["remediation"].as_str().unwrap();
    assert_eq!(repositories["capability"]["authenticated"], false);
    assert!(remediation.contains(connect_url), "{remediation}");
    assert!(!remediation.contains("gh auth login"), "{remediation}");

    let pull_requests: serde_json::Value = client
        .post(format!("http://{addr}/code/delivery/pull-requests/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "repositories": [{
                "host": "github.com",
                "owner": "acme",
                "name": "demo"
            }],
            "states": ["open"]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pull_requests["items"].as_array().unwrap().len(), 0);
    assert_eq!(pull_requests["errors"][0]["kind"], "git_forge_not_offered");
    assert!(pull_requests["errors"][0]["message"]
        .as_str()
        .unwrap()
        .contains(connect_url));
    assert!(lender.minted().is_empty());
}
