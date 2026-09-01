//! Background workspace naming over the wire, against the scripted engine.
//!
//! The engine never touches a model provider here — the scripted adapter plays
//! the turn — so every request the stub endpoint sees is a titling call. That
//! is the assertion surface: what the call carried, which model it ran on, and
//! what the workspace was called afterwards.

use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use axum::Router;
use tidebreak_harness::AdapterRegistry;
use tokio::net::TcpListener;

/// The model an OpenAI-only install resolves the `utility` role to.
const UTILITY_MODEL: &str = "gpt-5.4-nano";

/// What the stub answers every constrained call with.
const DERIVED_TITLE: &str = "Fix the flaky auth retry test";

/// A stub OpenAI Responses endpoint that names everything `DERIVED_TITLE`.
struct WorkspaceTitlingStub {
    requests: Mutex<Vec<serde_json::Value>>,
}

async fn answer_as_stub(
    axum::extract::State(stub): axum::extract::State<Arc<WorkspaceTitlingStub>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    stub.requests.lock().unwrap().push(request);
    let answer = serde_json::json!({ "title": DERIVED_TITLE }).to_string();
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"type": "response.output_text.delta", "delta": answer}),
        serde_json::json!({"type": "response.completed", "response": {}}),
    );
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

/// An app whose code mode runs the scripted engine and whose only credentialed
/// provider is the stub endpoint.
///
/// The registry-enforcing resolver is what makes the `utility` role resolve to
/// a real model, so turn submission actually starts a naming call instead of
/// skipping the work — the same wiring the chat-titling tests use.
async fn code_titling_app() -> (
    Router,
    String,
    Arc<WorkspaceTitlingStub>,
    Arc<CodeRuntime>,
    tempfile::TempDir,
) {
    let stub = Arc::new(WorkspaceTitlingStub {
        requests: Mutex::new(Vec::new()),
    });
    let endpoint = axum::Router::new()
        .route("/v1/responses", axum::routing::post(answer_as_stub))
        .with_state(stub.clone());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, endpoint).await;
    });
    let provider_base_url = format!("http://{address}/v1");
    providers::allow_test_loopback_provider_base_url(&provider_base_url);

    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("code-titling.db").display()
        ))
        .await
        .unwrap(),
    );
    let store: Arc<dyn Store> = db.clone();
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some(provider_base_url),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-test"),
    )
    .await
    .unwrap();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(resolver::ConfiguredResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(
                store.clone(),
                secrets.clone(),
                crate::managed_policy::MemoryProvisionedPolicy::new(),
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
            Arc::new(
                crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone())
                    .unwrap(),
            ),
            crate::managed_policy::MemoryProvisionedPolicy::new(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        )),
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "openai::gpt-5.4-nano".into(),
            ..AgentConfig::default()
        },
    );
    state.code = Some(runtime.clone());
    let token = state.token.to_string();
    (app(state), token, stub, runtime, dir)
}

fn init_git_repo(dir: &std::path::Path) -> std::path::PathBuf {
    let repo = dir.join("origin");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        ["git", "init", "-b", "main"].as_slice(),
        ["git", "config", "user.email", "dev@example.com"].as_slice(),
        ["git", "config", "user.name", "Dev"].as_slice(),
    ] {
        assert!(std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(&repo)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    for args in [
        ["git", "add", "README.md"].as_slice(),
        ["git", "commit", "-m", "init"].as_slice(),
    ] {
        assert!(std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    repo
}

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

/// The whole feature over the wire: a workspace created without a title gets
/// the generated two-word placeholder, and the first turn replaces it with a
/// name derived from what the user asked for, on the utility model, announced
/// on the digest channel — without the turn waiting for any of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_first_turn_names_an_untitled_workspace() {
    let (router, token, stub, runtime, dir) = code_titling_app().await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());

    let registered = client
        .post(format!("http://{addr}/code/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "path": repo }))
        .send()
        .await
        .unwrap();
    assert_eq!(registered.status(), reqwest::StatusCode::CREATED);
    let repo_body: serde_json::Value = registered.json().await.unwrap();

    // No title: the create answers with the deterministic two-word placeholder.
    let created = client
        .post(format!("http://{addr}/code/workspaces"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "repo_id": repo_body["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id = workspace["id"].as_str().unwrap().to_owned();
    let original_branch = workspace["branch_name"].as_str().unwrap().to_owned();
    let placeholder = crate::code::worktree::two_word_name(
        uuid::Uuid::parse_str(&workspace_id).unwrap().as_u128(),
    );
    assert_eq!(workspace["title"], serde_json::json!(placeholder));

    let mut updates = runtime
        .bus
        .subscribe_updates(&tidebreak_core::OwnerId::local());

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/sessions"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();

    let turn = client
        .post(format!(
            "http://{addr}/code/sessions/{}/turns",
            session["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "message": "Fix the flaky retry test in the auth crate"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);

    // The derived name lands in the store.
    let mut title = String::new();
    let mut branch = original_branch.clone();
    let mut worktree_path = String::new();
    for _ in 0..300 {
        let current: serde_json::Value = client
            .get(format!("http://{addr}/code/workspaces/{workspace_id}"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        title = current["title"].as_str().unwrap().to_owned();
        branch = current["branch_name"].as_str().unwrap().to_owned();
        worktree_path = current["worktree_path"].as_str().unwrap().to_owned();
        if title != placeholder && branch != original_branch {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(title, DERIVED_TITLE);
    assert_eq!(branch, "tidebreak/fix-the-flaky-auth-retry-test");
    let checked_out = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(worktree_path)
        .output()
        .unwrap();
    assert!(checked_out.status.success());
    assert_eq!(
        String::from_utf8(checked_out.stdout).unwrap().trim(),
        branch
    );

    // The naming call ran on the utility model and read the user's message.
    let requests = stub.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "one turn, one naming call");
    assert_eq!(requests[0]["model"], UTILITY_MODEL);
    let input = requests[0]["input"].to_string();
    assert!(
        input.contains("Fix the flaky retry test in the auth crate"),
        "the naming call reads the submitted turn: {input}"
    );

    // The rename was announced on the digest channel every list surface
    // watches, so an open sidebar re-labels without a refresh.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let update = tokio::time::timeout_at(deadline, updates.recv())
            .await
            .expect("a digest with the derived title is published")
            .expect("the digest channel stays open");
        if let crate::code::bus::CodeLiveUpdate::Digest(digest) = update {
            if digest.title == DERIVED_TITLE {
                break;
            }
        }
    }
}
