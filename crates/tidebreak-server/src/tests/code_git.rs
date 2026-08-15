//! Commit, push, pull-request, and quick-action routes against temp repos.

use super::*;

use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{QuickAction, RepoId};
use tidebreak_harness::AdapterRegistry;

async fn code_app() -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code-git.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
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
if [ "$1" = auth ]; then echo logged in; exit 0; fi
if [ "$1" = pr ] && [ "$2" = create ]; then
  echo https://github.com/example/demo/pull/12
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  echo '{{"number":12,"url":"https://github.com/example/demo/pull/12","state":"OPEN"}}'
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
    assert!(logged.contains("pr view"), "{logged}");
    assert!(logged.contains("pr checks"), "{logged}");
    assert!(!logged.contains("merge"), "{logged}");
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
    let mut stored = runtime.get_repo(repo_id).await.unwrap();
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
    let mut stored = runtime.get_repo(repo_id).await.unwrap();
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
