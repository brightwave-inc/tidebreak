//! Turn-end lucid rewrites over the wire, against the scripted engine.
//!
//! The engine never touches a model provider here — the scripted adapter plays
//! the turn — so every request the stub endpoint sees is a background
//! derivation. Failure, no model, and a setting left off all leave the original
//! closing message in the journal and store nothing on the rewrite column.

use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{ScriptedAdapter, plain_text_script};
use axum::Router;
use tidebreak_harness::AdapterRegistry;
use tokio::net::TcpListener;

/// The model an OpenAI-only install resolves the `utility` role to.
const UTILITY_MODEL: &str = "gpt-5.4-nano";

/// What the stub answers every rewrite call with.
const DERIVED_REWRITE: &str = "The retry test passes. Fold the same backoff into the refresh path.";

/// The scripted engine's closing message, settled from its delta.
const ORIGINAL_CLOSING: &str = "hello from the scripted engine";

/// A stub OpenAI Responses endpoint that answers by requested schema.
struct RewriteStub {
    requests: Mutex<Vec<serde_json::Value>>,
    fail: AtomicBool,
}

async fn answer_as_stub(
    axum::extract::State(stub): axum::extract::State<Arc<RewriteStub>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    let body = request.to_string();
    stub.requests.lock().unwrap().push(request);
    if body.contains("turn_rewrite") {
        if stub.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                "no".to_owned(),
            );
        }
        let answer = serde_json::json!({ "rewrite": DERIVED_REWRITE }).to_string();
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"type": "response.output_text.delta", "delta": answer}),
            serde_json::json!({"type": "response.completed", "response": {}}),
        );
        return (
            axum::http::StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            body,
        );
    }
    let answer = serde_json::json!({ "title": "Fix the flaky auth retry test" }).to_string();
    let body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({"type": "response.output_text.delta", "delta": answer}),
        serde_json::json!({"type": "response.completed", "response": {}}),
    );
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
}

/// An app whose code mode runs the scripted engine, whose only credentialed
/// provider is the stub endpoint, and whose runtime has the rewrite hook
/// installed the way `lib.rs` installs it in a real process.
async fn code_rewrite_app(
    enabled: bool,
    fail: bool,
) -> (
    Router,
    String,
    Arc<RewriteStub>,
    Arc<CodeRuntime>,
    tempfile::TempDir,
) {
    let stub = Arc::new(RewriteStub {
        requests: Mutex::new(Vec::new()),
        fail: std::sync::atomic::AtomicBool::new(fail),
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
            dir.path().join("code-rewrite.db").display()
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
    if enabled {
        store
            .set_setting(
                crate::code::rewrite::REWRITE_CLOSING_SETTING,
                &serde_json::json!(true),
            )
            .await
            .unwrap();
    }
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(ScriptedAdapter::new(plain_text_script())));
    let runtime = Arc::new(CodeRuntime::with_registry(
        db,
        dir.path().to_path_buf(),
        registry,
    ));
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let resolver: Arc<dyn ProviderResolver> = Arc::new(resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            provisioned_policy.clone(),
            os_policy.clone(),
        ),
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        provisioned_policy.clone(),
        os_policy.clone(),
    ));
    runtime.install_rewrite(Arc::new(crate::code::rewrite::TurnRewriter::new(
        runtime.db.clone(),
        runtime.bus.clone(),
        store.clone(),
        resolver.clone(),
        secrets.clone(),
        provisioned_policy.clone(),
        os_policy.clone(),
    )));
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        resolver,
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
        assert!(
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&repo)
                .env("GIT_TERMINAL_PROMPT", "0")
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    for args in [
        ["git", "add", "README.md"].as_slice(),
        ["git", "commit", "-m", "init"].as_slice(),
    ] {
        assert!(
            std::process::Command::new(args[0])
                .args(&args[1..])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
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

async fn complete_one_turn(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    repo: &std::path::Path,
) -> String {
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
        .json(&serde_json::json!({ "repo_id": repo_body["id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let workspace: serde_json::Value = created.json().await.unwrap();
    let workspace_id = workspace["id"].as_str().unwrap().to_owned();

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{workspace_id}/sessions"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();
    let session_id = session["id"].as_str().unwrap().to_owned();

    let turn = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "message": "Fix the flaky retry test in the auth crate"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    session_id
}

/// A completed turn is rewritten on the utility model, the line is stored on
/// the turn it describes, and the original journal text stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_completed_turn_stores_the_derived_rewrite() {
    let (router, token, stub, runtime, dir) = code_rewrite_app(true, false).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());

    let mut updates = runtime
        .bus
        .subscribe_updates(&tidebreak_core::OwnerId::local());

    let session_id = complete_one_turn(&client, addr, &token, &repo).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let update = tokio::time::timeout_at(deadline, updates.recv())
            .await
            .expect("a rewrite notice is published")
            .expect("the updates channel stays open");
        if let crate::code::bus::CodeLiveUpdate::TurnRewrite(notice) = update {
            if notice.state == crate::code::bus::TurnRewriteState::Rewritten
                && notice.rewrite.as_deref() == Some(DERIVED_REWRITE)
            {
                break;
            }
        }
    }

    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        tidebreak_core::CodeSessionId(uuid::Uuid::parse_str(&session_id).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(
        turns.last().and_then(|turn| turn.rewrite.as_deref()),
        Some(DERIVED_REWRITE),
    );

    let events = tidebreak_core::db::code::list_recent_events(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        tidebreak_core::CodeSessionId(uuid::Uuid::parse_str(&session_id).unwrap()),
        400,
    )
    .await
    .unwrap();
    let closing = events.iter().find_map(|sequenced| match &sequenced.event {
        tidebreak_core::CodeEvent::AssistantMessage {
            text,
            parent_call_id: None,
        } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(closing, Some(ORIGINAL_CLOSING));

    let requests = stub.requests.lock().unwrap().clone();
    let rewrite_calls: Vec<_> = requests
        .iter()
        .filter(|request| request.to_string().contains("turn_rewrite"))
        .collect();
    assert_eq!(
        rewrite_calls.len(),
        1,
        "one completed turn, one rewrite call"
    );
    assert_eq!(rewrite_calls[0]["model"], UTILITY_MODEL);
    let input = rewrite_calls[0]["input"].to_string();
    assert!(
        input.contains("Fix the flaky retry test in the auth crate"),
        "the rewrite reads the turn it describes: {input}"
    );
    assert!(
        input.contains(ORIGINAL_CLOSING),
        "the rewrite reads the closing message: {input}"
    );
}

/// No utility model, no rewrite — and no failed turn either.
#[tokio::test(flavor = "multi_thread")]
async fn a_machine_with_no_utility_model_stores_no_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("code-rewrite-none.db").display()
        ))
        .await
        .unwrap(),
    );
    let store: Arc<dyn Store> = db.clone();
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let resolver: Arc<dyn ProviderResolver> = Arc::new(resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            provisioned_policy.clone(),
            os_policy.clone(),
        ),
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        provisioned_policy.clone(),
        os_policy.clone(),
    ));
    let rewriter = crate::code::rewrite::TurnRewriter::new(
        db.clone(),
        Arc::new(crate::code::bus::CodeEventBus::default()),
        store.clone(),
        resolver,
        secrets,
        provisioned_policy,
        os_policy,
    );
    let outcome = rewriter
        .derive(
            &tidebreak_core::OwnerId::local(),
            tidebreak_core::CodeSessionId(uuid::Uuid::new_v4()),
            tidebreak_core::CodeTurnId(uuid::Uuid::new_v4()),
        )
        .await
        .unwrap();
    assert_eq!(outcome, crate::code::rewrite::Outcome::NotApplicable);
}

/// A failing utility call leaves the original journal text and stores nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_rewrite_leaves_the_original() {
    let (router, token, stub, runtime, dir) = code_rewrite_app(true, true).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());

    let mut updates = runtime
        .bus
        .subscribe_updates(&tidebreak_core::OwnerId::local());

    let session_id = complete_one_turn(&client, addr, &token, &repo).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let update = tokio::time::timeout_at(deadline, updates.recv())
            .await
            .expect("a rewrite failure notice is published")
            .expect("the updates channel stays open");
        if let crate::code::bus::CodeLiveUpdate::TurnRewrite(notice) = update {
            if notice.state == crate::code::bus::TurnRewriteState::Failed {
                break;
            }
        }
    }

    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        tidebreak_core::CodeSessionId(uuid::Uuid::parse_str(&session_id).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(turns.last().and_then(|turn| turn.rewrite.as_deref()), None);

    let events = tidebreak_core::db::code::list_recent_events(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        tidebreak_core::CodeSessionId(uuid::Uuid::parse_str(&session_id).unwrap()),
        400,
    )
    .await
    .unwrap();
    let closing = events.iter().find_map(|sequenced| match &sequenced.event {
        tidebreak_core::CodeEvent::AssistantMessage {
            text,
            parent_call_id: None,
        } => Some(text.as_str()),
        _ => None,
    });
    assert_eq!(closing, Some(ORIGINAL_CLOSING));
    assert!(
        stub.requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.to_string().contains("turn_rewrite")),
        "the failing call still reached the utility model"
    );
}
