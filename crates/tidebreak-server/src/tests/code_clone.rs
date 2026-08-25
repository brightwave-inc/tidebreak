//! Clone-job routes against a local bare origin. No network.

use super::*;

use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use futures::StreamExt;
use tokio::net::TcpListener;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::code::CodeRuntime;
use crate::obo_gateway::test_support::FakeLender;
use crate::obo_gateway::{GitCredentialLender, GitForgeError};
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_harness::AdapterRegistry;

async fn code_app() -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    code_app_with(None).await
}

/// The clone app, optionally on a "hosted machine" that lends gateway git
/// credentials (decision 63).
async fn code_app_with(
    lender: Option<Arc<dyn GitCredentialLender>>,
) -> (Router, Arc<str>, Arc<CodeRuntime>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code-clone.db").await;
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

fn init_bare_origin(dir: &std::path::Path) -> std::path::PathBuf {
    let bare = dir.join("origin.git");
    let work = dir.join("seed");
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
    bare
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

async fn wait_job(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    job_id: &str,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let response = client
            .get(format!("http://{addr}/code/repos/clone/{job_id}"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await.unwrap();
        if body["done"].as_bool() == Some(true) {
            return body;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("clone job did not finish: {body}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    loop {
        let Some(frame) = socket.next().await else {
            panic!("updates socket closed");
        };
        let WsMessage::Text(text) = frame.expect("ws frame") else {
            continue;
        };
        return serde_json::from_str(text.as_str()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clone_from_a_local_origin_registers_the_repo() {
    let (router, token, _runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let origin = init_bare_origin(dir.path());
    let parent = dir.path().join("checkouts");
    std::fs::create_dir_all(&parent).unwrap();

    let mut request = format!("ws://{addr}/code/updates")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let snapshot = next_json(&mut socket).await;
    assert_eq!(snapshot["type"], "snapshot");

    let started = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": origin,
            "parent_dir": parent,
            "name": "demo",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = started.json().await.unwrap();
    let job_id = job["id"].as_str().expect("job id");

    let mut saw_progress = false;
    let mut done = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let notice = tokio::time::timeout(Duration::from_millis(500), next_json(&mut socket))
            .await
            .ok();
        let Some(notice) = notice else {
            continue;
        };
        if notice["type"] == "clone_progress" && notice["job"] == job_id {
            saw_progress = true;
            if notice["done"].as_bool() == Some(true) {
                done = Some(notice);
                break;
            }
        }
    }
    assert!(saw_progress, "updates channel must carry clone_progress");
    let done = done.expect("terminal clone_progress notice");
    assert!(done["error"].is_null());
    let repo_id = done["repo_id"].as_str().expect("registered repo id");

    let finished = wait_job(&client, addr, &token, job_id).await;
    assert_eq!(finished["repo_id"], repo_id);
    assert_eq!(finished["done"], true);

    let listed = client
        .get(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], repo_id);
    assert!(
        listed[0]["root_path"]
            .as_str()
            .unwrap()
            .ends_with("checkouts/demo"),
        "{}",
        listed[0]["root_path"]
    );

    let defaults = client
        .get(format!("http://{addr}/code/repos/clone-defaults"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        defaults["parent_dir"].as_str().unwrap(),
        parent.to_str().unwrap()
    );
}

#[tokio::test]
async fn clone_rejects_bad_url_existing_target_and_unusable_parent() {
    let (router, token, _runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let origin = init_bare_origin(dir.path());
    let parent = dir.path().join("checkouts");
    std::fs::create_dir_all(&parent).unwrap();

    let missing = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": origin,
            "parent_dir": dir.path().join("no-such-parent"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(body["kind"], "clone_parent_missing");

    let file_parent = dir.path().join("not-a-dir");
    std::fs::write(&file_parent, "nope\n").unwrap();
    let dirty = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": origin,
            "parent_dir": file_parent,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dirty.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = dirty.json().await.unwrap();
    assert_eq!(body["kind"], "clone_parent_not_dir");

    std::fs::create_dir_all(parent.join("demo")).unwrap();
    let exists = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": origin,
            "parent_dir": parent,
            "name": "demo",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(exists.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = exists.json().await.unwrap();
    assert_eq!(body["kind"], "clone_target_exists");

    let started = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": dir.path().join("missing-origin.git"),
            "parent_dir": parent,
            "name": "bad",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = started.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert_eq!(finished["done"], true);
    assert!(
        finished["error"].as_str().unwrap().contains("git"),
        "{finished}"
    );
    assert!(finished["repo_id"].is_null());
}

#[tokio::test]
async fn github_clone_uses_the_url_gh_reports_when_signed_in() {
    let (router, token, runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let origin = init_bare_origin(dir.path());
    let parent = dir.path().join("checkouts");
    std::fs::create_dir_all(&parent).unwrap();

    let shim_dir = tempfile::TempDir::new().unwrap();
    write_executable(
        &shim_dir.path().join("gh"),
        &format!(
            r#"#!/bin/sh
if [ "$1" = auth ]; then
  echo '{{"hosts":{{"github.com":[{{"active":true,"state":"success","login":"tester"}}]}}}}'
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then
  printf '{{"url":"{url}"}}\n'
  exit 0
fi
echo unexpected "$@" >&2
exit 3
"#,
            url = origin.display()
        ),
    );
    runtime.set_gh_search_path(Some(shim_dir.path().display().to_string()));

    let started = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "github": "acme/demo",
            "parent_dir": parent,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = started.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert_eq!(finished["done"], true, "{finished}");
    assert!(finished["error"].is_null(), "{finished}");
    assert!(finished["repo_id"].is_string());
}

#[tokio::test]
async fn a_machine_that_remembers_a_destination_clones_without_being_given_one() {
    let (router, token, _runtime, dir) = code_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let origin = init_bare_origin(dir.path());
    let parent = dir.path().join("checkouts");
    std::fs::create_dir_all(&parent).unwrap();

    // Nothing remembered yet, and nothing named: refuse rather than invent a
    // directory. This is the state a fresh machine is in.
    let sources: serde_json::Value = client
        .get(format!("http://{addr}/code/repos/sources"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sources["chooses_destination"], false);
    let kinds: Vec<&str> = sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"local"),
        "a machine can always register a checkout it already holds"
    );
    assert!(kinds.contains(&"git_url"));
    assert!(kinds.contains(&"github"));

    let unaimed = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": origin }))
        .send()
        .await
        .unwrap();
    assert_eq!(unaimed.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = unaimed.json().await.unwrap();
    assert_eq!(body["kind"], "clone_parent_missing");

    // One clone that names a destination is what teaches the machine where
    // its checkouts live.
    let first = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "url": origin,
            "parent_dir": parent,
            "name": "first",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = first.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert!(finished["error"].is_null(), "{finished:?}");

    let sources: serde_json::Value = client
        .get(format!("http://{addr}/code/repos/sources"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sources["chooses_destination"], true);

    // Now a caller who cannot see the machine's filesystem can clone: no
    // path, and the checkout still lands under the remembered destination.
    let second = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": origin, "name": "second" }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = second.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert!(finished["error"].is_null(), "{finished:?}");
    assert!(
        parent.join("second").exists(),
        "the clone lands under the remembered destination"
    );
}

#[tokio::test]
async fn a_self_host_default_places_clones_without_being_given_a_destination() {
    let (router, token, _runtime, dir) = {
        let (dir, store) = temp_db_store("code-clone-default.db").await;
        let db = std::sync::Arc::new(store);
        let store_trait: std::sync::Arc<dyn Store> = db.clone();
        let mut registry = AdapterRegistry::new();
        registry.register(std::sync::Arc::new(ScriptedAdapter::new(
            plain_text_script(),
        )));
        let default = dir.path().join("code").join("src");
        let runtime = std::sync::Arc::new(
            CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry)
                .with_clone_parent_default(default.clone()),
        );
        let mut state = AppState::new(
            Config::desktop(dir.path()),
            store_trait,
            std::sync::Arc::new(FixedResolver(std::sync::Arc::new(FakeProvider))),
            std::sync::Arc::new(MemSecrets::default()),
            std::sync::Arc::new(ToolRegistry::new()),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        state.code = Some(runtime.clone());
        (app(state), token, runtime, dir)
    };
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let origin = init_bare_origin(dir.path());

    let sources: serde_json::Value = client
        .get(format!("http://{addr}/code/repos/sources"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        sources["chooses_destination"], true,
        "a self-host default means the machine places clones itself"
    );

    let started = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "url": origin, "name": "ship" }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = started.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert!(finished["error"].is_null(), "{finished:?}");
    assert!(
        dir.path().join("code").join("src").join("ship").exists(),
        "the clone lands under the self-host default"
    );
}

#[tokio::test]
async fn a_hosted_machine_lists_the_callers_github_repositories() {
    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, _runtime, _dir) = code_app_with(Some(lender)).await;
    let addr = serve(router).await;
    let listed: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/code/repos/github"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["repositories"][0]["full_name"], "mira-chen/notes");
    assert_eq!(listed["repositories"][0]["private"], true);
}

async fn fetch_sources(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
) -> serde_json::Value {
    client
        .get(format!("http://{addr}/code/repos/sources"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn source<'a>(sources: &'a serde_json::Value, kind: &str) -> &'a serde_json::Value {
    sources["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["kind"] == kind)
        .expect(kind)
}

/// Decision 63: on a hosted machine the `github` source reflects the
/// gateway's per-caller answer. An offered forge carries the attribution
/// sentence; a deployment with no forge reads as "not offered, because…",
/// never as an error.
#[tokio::test]
async fn a_hosted_machine_offers_github_per_caller_from_the_gateway() {
    let lender = Arc::new(FakeLender::offering("acme-ship[bot]"));
    let (router, token, _runtime, _dir) =
        code_app_with(Some(lender as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let sources = fetch_sources(&client, addr, &token).await;
    let github = source(&sources, "github");
    assert_eq!(github["available"], true);
    let hint = github["remediation"].as_str().unwrap();
    assert!(hint.contains("acme-ship[bot]"), "{hint}");
    assert!(hint.contains("not as your GitHub account"), "{hint}");
    // Local registration and plain git URLs stay offered as before.
    assert_eq!(source(&sources, "local")["available"], true);
    assert_eq!(source(&sources, "git_url")["available"], true);

    let refused = Arc::new(FakeLender::refusing(GitForgeError::NoGitForge));
    let (router, token, _runtime, _dir) =
        code_app_with(Some(refused as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let sources = fetch_sources(&client, addr, &token).await;
    let github = source(&sources, "github");
    assert_eq!(github["available"], false);
    let reason = github["remediation"].as_str().unwrap();
    assert!(reason.contains("no git forge"), "{reason}");
}

/// Decision 65: a caller who connected their own account reads the person
/// attribution sentence, and a caller who has not reads "not offered" with
/// the gateway's connect page as the remediation — never a silent fall back
/// to another identity.
#[tokio::test]
async fn a_hosted_machine_offers_github_as_the_person_per_caller() {
    let lender = Arc::new(FakeLender::offering_person("mira-chen"));
    let (router, token, _runtime, _dir) =
        code_app_with(Some(lender as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let sources = fetch_sources(&client, addr, &token).await;
    let github = source(&sources, "github");
    assert_eq!(github["available"], true);
    let hint = github["remediation"].as_str().unwrap();
    assert!(hint.contains("your own GitHub account"), "{hint}");
    assert!(hint.contains("mira-chen"), "{hint}");
    assert!(!hint.contains("not as your GitHub account"), "{hint}");

    let unconnected = Arc::new(FakeLender::refusing(GitForgeError::NotConnected {
        connect_url: Some("https://gateway.example/account/apps".to_owned()),
    }));
    let (router, token, _runtime, _dir) =
        code_app_with(Some(unconnected as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let sources = fetch_sources(&client, addr, &token).await;
    let github = source(&sources, "github");
    assert_eq!(github["available"], false);
    let reason = github["remediation"].as_str().unwrap();
    assert!(reason.contains("connect your GitHub account"), "{reason}");
    assert!(
        reason.contains("https://gateway.example/account/apps"),
        "{reason}"
    );
}

/// Decision 63 rule 4: a clone the gateway refuses fails with the gateway's
/// reason — before any network is touched — and the mint was asked for
/// exactly the repository the caller named.
#[tokio::test]
async fn a_hosted_clone_fails_closed_with_the_gateway_refusal() {
    let lender = Arc::new(FakeLender::refusing(GitForgeError::RepositoryNotInstalled));
    let (router, token, _runtime, dir) =
        code_app_with(Some(lender.clone() as Arc<dyn GitCredentialLender>)).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let parent = dir.path().join("checkouts");
    std::fs::create_dir_all(&parent).unwrap();

    let started = client
        .post(format!("http://{addr}/code/repos/clone"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "github": "acme/private",
            "parent_dir": parent,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let job: serde_json::Value = started.json().await.unwrap();
    let finished = wait_job(&client, addr, &token, job["id"].as_str().unwrap()).await;
    assert_eq!(finished["done"], true, "{finished}");
    let error = finished["error"].as_str().expect("the clone fails");
    assert!(error.contains("does not cover"), "{error}");
    assert_eq!(lender.minted(), vec!["acme/private".to_owned()]);
}
