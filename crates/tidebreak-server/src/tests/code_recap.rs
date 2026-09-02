//! Turn recaps over the wire, against the scripted engine.
//!
//! The engine never touches a model provider here — the scripted adapter plays
//! the turn — so every request the stub endpoint sees is a background
//! derivation. The stub answers each one by the output schema it asked for,
//! which is what lets one test watch naming and recapping side by side.

use super::*;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use axum::Router;
use tidebreak_harness::{AdapterRegistry, HarnessEvent};
use tokio::net::TcpListener;

/// The model an OpenAI-only install resolves the `utility` role to.
const UTILITY_MODEL: &str = "gpt-5.4-nano";

/// What the stub answers every recap call with.
const DERIVED_RECAP: &str =
    "The retry test is passing again. Next: fold the same backoff into the refresh path.";

/// A stub OpenAI Responses endpoint that answers by requested schema.
struct RecapStub {
    requests: Mutex<Vec<serde_json::Value>>,
}

async fn answer_as_stub(
    axum::extract::State(stub): axum::extract::State<Arc<RecapStub>>,
    axum::Json(request): axum::Json<serde_json::Value>,
) -> impl axum::response::IntoResponse {
    // Both derivations run on the same endpoint and the same model. The schema
    // name is what tells them apart, and asserting on it here is also how the
    // test knows a recap call was actually constrained to the recap shape.
    let answer = if request.to_string().contains("session_recap") {
        serde_json::json!({ "recap": DERIVED_RECAP }).to_string()
    } else {
        serde_json::json!({ "title": "Fix the flaky auth retry test" }).to_string()
    };
    stub.requests.lock().unwrap().push(request);
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

/// An app whose code mode runs the scripted engine, whose only credentialed
/// provider is the stub endpoint, and whose runtime has the recap hook
/// installed the way `lib.rs` installs it in a real process.
async fn code_recap_app(
    harness_kind: tidebreak_core::HarnessKind,
) -> (
    Router,
    String,
    Arc<RecapStub>,
    Arc<CodeRuntime>,
    tempfile::TempDir,
) {
    let stub = Arc::new(RecapStub {
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
            dir.path().join("code-recap.db").display()
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
    let mut script = plain_text_script();
    if let Some(HarnessEvent::SessionStarted {
        harness_kind: started,
        ..
    }) = script.first_mut()
    {
        *started = harness_kind;
    }
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(
        ScriptedAdapter::new(script).with_kind(harness_kind),
    ));
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
    runtime.install_recap(Arc::new(crate::code::recap::TurnRecapper::new(
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

async fn start_turn(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    dir: &std::path::Path,
    harness_kind: tidebreak_core::HarnessKind,
) -> tidebreak_core::CodeSessionId {
    let repo = init_git_repo(dir);
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

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            workspace["id"].as_str().unwrap()
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "harness": harness_kind.as_str(),
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(session.status(), reqwest::StatusCode::CREATED);
    let session: serde_json::Value = session.json().await.unwrap();
    let session_id = tidebreak_core::CodeSessionId(
        uuid::Uuid::parse_str(session["id"].as_str().unwrap()).unwrap(),
    );

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

async fn wait_for_completed_turn(
    runtime: &CodeRuntime,
    session_id: tidebreak_core::CodeSessionId,
) -> tidebreak_core::CodeTurn {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let turns = tidebreak_core::db::code::list_turns(
            &runtime.db,
            &tidebreak_core::OwnerId::local(),
            session_id,
        )
        .await
        .unwrap();
        if let Some(turn) = turns
            .last()
            .filter(|turn| turn.status == tidebreak_core::CodeTurnStatus::Completed)
        {
            return turn.clone();
        }
        tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(20)))
            .await
            .expect("the scripted turn completes before the deadline");
    }
}

/// The whole feature over the wire: a completed turn is recapped on the
/// utility model, the line is stored on the turn it describes, and it reaches
/// every list surface on the digest channel — without the turn waiting for any
/// of it.
#[tokio::test(flavor = "multi_thread")]
async fn a_completed_turn_is_recapped_onto_the_digest() {
    let (router, token, stub, runtime, dir) =
        code_recap_app(tidebreak_core::HarnessKind::Codex).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let mut updates = runtime
        .bus
        .subscribe_updates(&tidebreak_core::OwnerId::local());
    let session_id = start_turn(
        &client,
        addr,
        &token,
        dir.path(),
        tidebreak_core::HarnessKind::Codex,
    )
    .await;

    // The recap reaches the digest channel every list surface watches, so a
    // rail row can say where the session stands without a refresh.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let update = tokio::time::timeout_at(deadline, updates.recv())
            .await
            .expect("a digest carrying the recap is published")
            .expect("the digest channel stays open");
        if let crate::code::bus::CodeLiveUpdate::Digest(digest) = update {
            if digest.recap.as_deref() == Some(DERIVED_RECAP) {
                break;
            }
        }
    }

    // It is stored on the turn it describes, so it survives a reopen.
    let turns = tidebreak_core::db::code::list_turns(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        turns.last().and_then(|turn| turn.narrative.as_deref()),
        Some(DERIVED_RECAP),
    );

    // The recap ran on the utility model, was constrained to the recap schema,
    // and read what the turn was asked to do.
    let requests = stub.requests.lock().unwrap().clone();
    let recap_calls: Vec<_> = requests
        .iter()
        .filter(|request| request.to_string().contains("session_recap"))
        .collect();
    assert_eq!(recap_calls.len(), 1, "one completed turn, one recap call");
    assert_eq!(recap_calls[0]["model"], UTILITY_MODEL);
    let input = recap_calls[0]["input"].to_string();
    assert!(
        input.contains("Fix the flaky retry test in the auth crate"),
        "the recap reads the turn it describes: {input}"
    );
}

/// Claude Code already supplies the closing recap that the transcript keeps.
/// A second utility-model call would duplicate that line and its cost.
#[tokio::test(flavor = "multi_thread")]
async fn a_claude_turn_skips_the_fallback_recap() {
    let (router, token, stub, runtime, dir) =
        code_recap_app(tidebreak_core::HarnessKind::ClaudeCode).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session_id = start_turn(
        &client,
        addr,
        &token,
        dir.path(),
        tidebreak_core::HarnessKind::ClaudeCode,
    )
    .await;

    wait_for_completed_turn(&runtime, session_id).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let turn = wait_for_completed_turn(&runtime, session_id).await;

    assert!(turn.narrative.is_none());
    let recap_calls = stub
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.to_string().contains("session_recap"))
        .count();
    assert_eq!(recap_calls, 0);

    // Older builds may have stored a fallback on a Claude turn. The digest
    // must not carry that text forward and attach it to a newer closing recap.
    tidebreak_core::db::code::set_turn_narrative(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        turn.id,
        "A stale Tidebreak fallback.",
    )
    .await
    .unwrap();
    let digest =
        crate::code::attention::list_digests(&runtime.db, &tidebreak_core::OwnerId::local())
            .await
            .unwrap()
            .into_iter()
            .find(|digest| digest.session == session_id)
            .expect("the Claude session stays listed");
    assert!(digest.recap.is_none());
}

/// Turning recaps off prevents the utility-model call for later turns.
#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_recap_setting_skips_the_model_call() {
    let (router, token, stub, runtime, dir) =
        code_recap_app(tidebreak_core::HarnessKind::Codex).await;
    runtime
        .db
        .set_setting(
            crate::code::recap::TURN_RECAPS_SETTING,
            &serde_json::json!(false),
        )
        .await
        .unwrap();
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let session_id = start_turn(
        &client,
        addr,
        &token,
        dir.path(),
        tidebreak_core::HarnessKind::Codex,
    )
    .await;
    let turn = wait_for_completed_turn(&runtime, session_id).await;
    assert!(turn.narrative.is_none());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let recap_calls = stub
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request.to_string().contains("session_recap"))
        .count();
    assert_eq!(recap_calls, 0);
}

/// No utility model, no recap — and no failed turn either.
///
/// A machine with nothing to run background work on is the ordinary case for a
/// bring-your-own-engine install, so the absence has to be silent rather than
/// an error the reader has to dismiss.
#[tokio::test(flavor = "multi_thread")]
async fn a_machine_with_no_utility_model_stores_no_recap() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("code-recap-none.db").display()
        ))
        .await
        .unwrap(),
    );
    let store: Arc<dyn Store> = db.clone();
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let provisioned_policy = crate::managed_policy::MemoryProvisionedPolicy::new();
    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    // No provider configured and no credential written: the utility role has
    // nothing to resolve to.
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
    let recapper = crate::code::recap::TurnRecapper::new(
        db.clone(),
        Arc::new(crate::code::bus::CodeEventBus::default()),
        store.clone(),
        resolver,
        secrets,
        provisioned_policy,
        os_policy,
    );
    // A turn id that does not resolve is the same silent no-op as a machine
    // with no model: neither is an error the reader should see.
    let outcome = recapper
        .derive(
            &tidebreak_core::OwnerId::local(),
            tidebreak_core::CodeSessionId(uuid::Uuid::new_v4()),
            tidebreak_core::CodeTurnId(uuid::Uuid::new_v4()),
        )
        .await
        .unwrap();
    assert_eq!(outcome, crate::code::recap::Outcome::NotApplicable);
}
