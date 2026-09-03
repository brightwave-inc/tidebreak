//! Engine doctor probes, hosted model listings, and session health sweeps.

use super::code::*;
use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::Router;

use crate::code::CodeRuntime;
use crate::scripted_harness::{plain_text_script, ScriptedAdapter};
use tidebreak_core::{
    AttentionState, CapLevel, HarnessKind, ReasoningEffort, SessionId, SessionLifecycle, Store,
};
use tidebreak_harness::AdapterRegistry;

#[tokio::test]
async fn stall_sweep_marks_a_silent_running_session() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let parsed: SessionId = json_id(&session).parse().unwrap();
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    row.lifecycle = SessionLifecycle::Running;
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();

    crate::code::attention::sweep_stalled(&runtime.db, &runtime.bus, 0)
        .await
        .unwrap();
    let row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(row.attention.state, AttentionState::Stalled { .. }),
        "{:?}",
        row.attention
    );
}

/// A monitor tool is silent by design: the engine is waiting on background
/// work, not stuck, and the rail says "Monitoring" beside it. The sweep must
/// not call that same session stalled.
#[tokio::test]
async fn stall_sweep_leaves_a_monitoring_session_working() {
    let (router, token, runtime, dir) = code_app(plain_text_script()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let parsed: SessionId = json_id(&session).parse().unwrap();
    let owner = tidebreak_core::OwnerId::local();
    let mut row = tidebreak_core::db::code::get_session(&runtime.db, &owner, parsed)
        .await
        .unwrap()
        .unwrap();
    row.lifecycle = SessionLifecycle::Running;
    tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap();
    tidebreak_core::db::code::replace_session_attention(
        &runtime.db,
        &owner,
        parsed,
        &tidebreak_core::Attention::working(tidebreak_core::AttentionSource::Lifecycle),
        false,
    )
    .await
    .unwrap();
    tidebreak_core::db::code::append_event(
        &runtime.db,
        &owner,
        parsed,
        row.spawn_epoch,
        &tidebreak_core::Event::ToolStarted {
            call_id: "monitor-1".into(),
            name: "Monitor".into(),
            detail: tidebreak_core::ToolDetail::Other {
                summary: "watching CI".into(),
            },
            parent_call_id: None,
        },
    )
    .await
    .unwrap();

    crate::code::attention::sweep_stalled(&runtime.db, &runtime.bus, 0)
        .await
        .unwrap();
    let row = tidebreak_core::db::code::get_session(&runtime.db, &owner, parsed)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(row.attention.state, AttentionState::Working),
        "a session parked on a monitor must stay Working, got {:?}",
        row.attention
    );
}

/// The doctor serves memoized probes, and refresh is the on-demand re-probe
/// (decision 0034). A cold probe spends an interactive login shell plus a
/// version and an authentication subprocess per harness, and the code-mode
/// surface reads this route on every navigation.
#[tokio::test]
async fn the_doctor_caches_probes_and_refresh_re_probes() {
    let adapter = ScriptedAdapter::new(plain_text_script());
    let (router, token, _runtime, _dir) = code_app_with(adapter.clone()).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(report["harnesses"][0]["kind"], "claude_code");
    assert_eq!(report["harnesses"][0]["found"], true);

    let again = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(again["harnesses"][0]["found"], true);
    assert_eq!(
        adapter.probe_count(),
        1,
        "a second doctor read must be served from the cache"
    );

    let refreshed = client
        .post(format!("http://{addr}/code/harnesses/refresh"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(refreshed["harnesses"][0]["found"], true);
    assert_eq!(
        adapter.probe_count(),
        2,
        "refresh must re-probe rather than repeat the cached answer"
    );
}

/// opencode fixes permission mode at session create; Claude and Codex
/// recompose it on relaunch. The doctor carries that so the picker can lock.
#[tokio::test]
async fn the_doctor_reports_whether_relaunch_composes_permission_mode() {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script()).with_kind(HarnessKind::ClaudeCode),
    ));
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script()).with_kind(HarnessKind::Codex),
    ));
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script())
            .with_kind(HarnessKind::Opencode)
            .with_posture_fixed_at_session_start(),
    ));
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
    state.code = Some(runtime);
    let token = state.token.clone();
    let addr = serve(app(state)).await;
    let client = reqwest::Client::new();

    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    let by_kind: std::collections::BTreeMap<String, bool> = report["harnesses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["kind"].as_str().unwrap().to_owned(),
                row["relaunch_composes_permission_mode"]
                    .as_bool()
                    .expect("doctor must send relaunch_composes_permission_mode"),
            )
        })
        .collect();
    assert_eq!(by_kind.get("claude_code"), Some(&true));
    assert_eq!(by_kind.get("codex"), Some(&true));
    assert_eq!(by_kind.get("opencode"), Some(&false));
}

/// Decision 71's doctor half: on a gateway-hosted machine the relay-covered
/// engines are ready without a local sign-in — nobody can open a terminal
/// there — and the uncovered ones say they are not available hosted yet,
/// rather than demanding that terminal sign-in.
#[tokio::test]
async fn the_doctor_reports_relay_engines_ready_on_a_hosted_machine() {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script()).with_authenticated(Some(false)),
    ));
    registry.register(Arc::new(
        ScriptedAdapter::new(plain_text_script())
            .with_kind(HarnessKind::Opencode)
            .with_authenticated(Some(false)),
    ));
    let gateway = Arc::new(
        crate::obo_gateway::OboGateway::new(
            "https://gateway.example",
            "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed".to_owned(),
        )
        .unwrap(),
    );
    let runtime = Arc::new(
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry).with_harness_llm(
            Arc::new(crate::code::harness_llm::HarnessLlmRelay::new(gateway)),
        ),
    );
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
    state.code = Some(runtime);
    let token = state.token.clone();
    let addr = serve(app(state)).await;
    let client = reqwest::Client::new();

    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    let claude = &report["harnesses"][0];
    assert_eq!(claude["kind"], "claude_code");
    assert_eq!(claude["auth_mode"], "gateway_relay");
    assert_eq!(
        claude["authenticated"], false,
        "the local probe observation stays on the row; it is just no longer the verdict"
    );
    assert_eq!(claude["remediation"], "");

    let opencode = &report["harnesses"][1];
    assert_eq!(opencode["kind"], "opencode");
    assert_eq!(opencode["auth_mode"], "gateway_relay");
    assert_eq!(opencode["remediation"], "");
}

/// A caller-scoped fake for the exact compat surfaces hosted engine pickers
/// read. Either surface may be absent, which lets tests prove a one-protocol
/// engine never depends on the other protocol's listing.
#[derive(Clone)]
struct FakeModelGateway {
    anthropic: Option<serde_json::Value>,
    openai: Option<serde_json::Value>,
    anthropic_reads: Arc<AtomicUsize>,
    openai_reads: Arc<AtomicUsize>,
}

impl FakeModelGateway {
    fn new(anthropic: Option<serde_json::Value>, openai: Option<serde_json::Value>) -> Self {
        Self {
            anthropic,
            openai,
            anthropic_reads: Arc::new(AtomicUsize::new(0)),
            openai_reads: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn full() -> Self {
        Self::new(
            Some(serde_json::json!({
                "data": [
                    {
                        "type": "model",
                        "id": " claude-opus-5 ",
                        "display_name": " Claude Opus 5 ",
                        "is_family_default": true,
                    },
                    {
                        "type": "model",
                        "id": "shared/model",
                        "display_name": "Shared via Anthropic",
                    },
                    { "type": "model", "id": "   ", "display_name": "Dead row" },
                    { "type": "model", "display_name": "Missing id" },
                ],
                "has_more": false,
            })),
            Some(serde_json::json!({
                "object": "list",
                "data": [
                    {
                        "id": "glm-5.3",
                        "object": "model",
                        "created": 0,
                        "owned_by": "model-gateway",
                    },
                    {
                        "id": "shared/model",
                        "object": "model",
                        "created": 0,
                        "owned_by": "model-gateway",
                    },
                    {
                        "id": " trim-me ",
                        "display_name": "   ",
                        "object": "model",
                    },
                    { "id": "", "object": "model" },
                    { "id": 7, "object": "model" },
                ],
            })),
        )
    }

    async fn start(
        self,
    ) -> (
        Arc<crate::obo_gateway::OboGateway>,
        tokio::task::JoinHandle<()>,
    ) {
        let mut app = axum::Router::new().route(
            "/oauth/token",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "access_token": "mg_it_fresh",
                    "expires_in": 3600,
                    "token_type": "Bearer",
                }))
            }),
        );
        if let Some(listing) = self.anthropic {
            let reads = self.anthropic_reads.clone();
            app = app.route(
                "/compat/anthropic/v1/models",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let listing = listing.clone();
                    let reads = reads.clone();
                    async move {
                        assert_eq!(
                            headers
                                .get(axum::http::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok()),
                            Some("Bearer mg_it_fresh"),
                            "the listing must ride the caller's exchanged inference grant"
                        );
                        reads.fetch_add(1, Ordering::SeqCst);
                        axum::Json(listing)
                    }
                }),
            );
        }
        if let Some(listing) = self.openai {
            let reads = self.openai_reads.clone();
            app = app.route(
                "/compat/openai/v1/models",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let listing = listing.clone();
                    let reads = reads.clone();
                    async move {
                        assert_eq!(
                            headers
                                .get(axum::http::header::AUTHORIZATION)
                                .and_then(|value| value.to_str().ok()),
                            Some("Bearer mg_it_fresh"),
                            "the listing must ride the caller's exchanged inference grant"
                        );
                        reads.fetch_add(1, Ordering::SeqCst);
                        axum::Json(listing)
                    }
                }),
            );
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let gateway = Arc::new(
            crate::obo_gateway::OboGateway::new(
                &format!("http://{address}"),
                "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed"
                    .to_owned(),
            )
            .unwrap(),
        );
        (gateway, server)
    }
}

async fn hosted_model_app(
    gateway: Arc<crate::obo_gateway::OboGateway>,
    record_caller: bool,
) -> (Router, Arc<str>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("code.db").await;
    let db = Arc::new(store);
    let store_trait: Arc<dyn Store> = db.clone();
    let mut registry = AdapterRegistry::new();
    for &kind in HarnessKind::ALL {
        let mut adapter = ScriptedAdapter::new(plain_text_script())
            .with_kind(kind)
            .with_models(vec![listed_model("local-only-model", true, &[], true)]);
        if kind == HarnessKind::ClaudeCode {
            adapter = adapter.with_reasoning_levels(CapLevel::Supported);
        }
        registry.register(Arc::new(adapter));
    }
    if record_caller {
        gateway.record_caller(&tidebreak_core::OwnerId::local(), "mg_at_live".into());
    }
    let runtime = Arc::new(
        CodeRuntime::with_registry(db, dir.path().to_path_buf(), registry).with_harness_llm(
            Arc::new(crate::code::harness_llm::HarnessLlmRelay::new(gateway)),
        ),
    );
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
    state.code = Some(runtime);
    let token = state.token.clone();
    (app(state), token, dir)
}

async fn fetch_harness_models(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    token: &str,
    kind: HarnessKind,
) -> reqwest::Response {
    client
        .get(format!("http://{addr}/code/harnesses/{kind}/models"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
}

fn model_ids(listing: &serde_json::Value) -> Vec<&str> {
    listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect()
}

/// Issue 2755: all four hosted engines list the caller-usable gateway rows
/// their relay wiring can run. OpenCode's ids name the configured provider,
/// duplicate raw ids prefer the Anthropic surface, malformed rows disappear,
/// and no engine's local-only row leaks into the hosted picker.
#[tokio::test]
async fn a_hosted_machine_lists_the_gateway_catalog_as_an_engines_models() {
    let fake = FakeModelGateway::full();
    let observed = fake.clone();
    let (gateway, gateway_server) = fake.start().await;
    let (router, token, _dir) = hosted_model_app(gateway, true).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();

    let claude = fetch_harness_models(&client, addr, token.as_ref(), HarnessKind::ClaudeCode)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let rows = claude["models"].as_array().unwrap();
    assert_eq!(
        model_ids(&claude),
        ["claude-opus-5", "shared/model"],
        "the Anthropic surface's rows are the picker truth: {claude}"
    );
    assert_eq!(
        rows[0]["label"], "Claude Opus 5",
        "display name maps to the label"
    );
    assert_eq!(
        rows[0]["default"], true,
        "the catalog's family default claims the picker default"
    );
    assert_eq!(rows[1]["default"], false);
    assert_eq!(
        rows[0]["fast_mode"], false,
        "a gateway row promises no fast mode the catalog does not state"
    );
    assert_eq!(
        claude["reasoning_efforts"],
        serde_json::to_value(ReasoningEffort::ALL).unwrap(),
        "the engine's own ladder stays the outer bound"
    );

    let codex = fetch_harness_models(&client, addr, token.as_ref(), HarnessKind::Codex)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        model_ids(&codex),
        ["glm-5.3", "shared/model", "trim-me"],
        "Codex sees the OpenAI surface, which the Anthropic-only row stays off: {codex}"
    );
    assert_eq!(
        codex["models"][2]["label"], "trim-me",
        "a blank display name falls back to the trimmed id"
    );

    let grok = fetch_harness_models(&client, addr, token.as_ref(), HarnessKind::Grok)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(model_ids(&grok), model_ids(&codex));

    let opencode = fetch_harness_models(&client, addr, token.as_ref(), HarnessKind::Opencode)
        .await
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        model_ids(&opencode),
        [
            "anthropic/claude-opus-5",
            "anthropic/shared/model",
            "model-gateway/glm-5.3",
            "model-gateway/trim-me",
        ],
        "OpenCode must post every pick through the provider wired to the listing surface: {opencode}"
    );
    for listing in [&claude, &codex, &grok, &opencode] {
        assert_eq!(
            listing["source"], "model_gateway",
            "hosted rows must identify the gateway catalog: {listing}"
        );
        assert!(
            !model_ids(listing).contains(&"local-only-model"),
            "the engine's local listing is never the hosted truth: {listing}"
        );
    }
    assert_eq!(
        observed.anthropic_reads.load(Ordering::SeqCst),
        4,
        "every hosted picker reads both listings; four engines, four Anthropic reads"
    );
    assert_eq!(
        observed.openai_reads.load(Ordering::SeqCst),
        4,
        "every hosted picker reads both listings; four engines, four OpenAI reads"
    );
    gateway_server.abort();
}

/// A one-protocol engine still succeeds when the unused listing is down.
/// OpenCode still requires both because it offers both providers.
#[tokio::test]
async fn a_hosted_engine_does_not_require_an_unrelated_gateway_listing() {
    let anthropic_only = FakeModelGateway::new(
        Some(serde_json::json!({
            "data": [{ "id": "claude-opus-5", "display_name": "Claude Opus 5" }]
        })),
        None,
    );
    let observed_anthropic = anthropic_only.clone();
    let (gateway, gateway_server) = anthropic_only.start().await;
    let (router, token, _dir) = hosted_model_app(gateway, true).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let claude = fetch_harness_models(&client, addr, token.as_ref(), HarnessKind::ClaudeCode).await;
    assert_eq!(claude.status(), reqwest::StatusCode::OK);
    assert_eq!(observed_anthropic.anthropic_reads.load(Ordering::SeqCst), 1);
    assert_eq!(observed_anthropic.openai_reads.load(Ordering::SeqCst), 0);
    gateway_server.abort();

    let openai_only = FakeModelGateway::new(
        None,
        Some(serde_json::json!({
            "object": "list",
            "data": [{ "id": "glm-5.3", "object": "model" }]
        })),
    );
    let observed_openai = openai_only.clone();
    let (gateway, gateway_server) = openai_only.start().await;
    let (router, token, _dir) = hosted_model_app(gateway, true).await;
    let addr = serve(router).await;
    for kind in [HarnessKind::Codex, HarnessKind::Grok] {
        let response = fetch_harness_models(&client, addr, token.as_ref(), kind).await;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{kind}");
    }
    assert_eq!(observed_openai.anthropic_reads.load(Ordering::SeqCst), 0);
    assert_eq!(observed_openai.openai_reads.load(Ordering::SeqCst), 2);
    gateway_server.abort();
}

/// The hosted branch is caller-owned and fail-closed. If authentication has
/// not recorded this request's principal, the route returns an error and
/// never falls back to a local CLI row or reads a gateway listing unauthenticated.
#[tokio::test]
async fn a_hosted_model_listing_without_caller_ownership_fails_closed() {
    let fake = FakeModelGateway::full();
    let observed = fake.clone();
    let (gateway, gateway_server) = fake.start().await;
    let (router, token, _dir) = hosted_model_app(gateway, false).await;
    let addr = serve(router).await;
    let response = fetch_harness_models(
        &reqwest::Client::new(),
        addr,
        token.as_ref(),
        HarnessKind::ClaudeCode,
    )
    .await;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["kind"], "authentication", "{body}");
    assert!(
        !body.to_string().contains("local-only-model"),
        "a missing caller grant must never fall back locally: {body}"
    );
    assert_eq!(observed.anthropic_reads.load(Ordering::SeqCst), 0);
    assert_eq!(observed.openai_reads.load(Ordering::SeqCst), 0);
    gateway_server.abort();
}

/// Issue 2755's other half: away from a hosted machine the models route is
/// the engine's own listing, exactly as before.
#[tokio::test]
async fn a_local_machine_keeps_the_engine_own_model_listing() {
    let adapter = ScriptedAdapter::new(plain_text_script())
        .with_reasoning_levels(CapLevel::Supported)
        .with_models(vec![
            listed_model("fast-thinker", true, &[ReasoningEffort::High], true),
            listed_model("steady", false, &[ReasoningEffort::Low], false),
        ]);
    let (router, token, _runtime, _dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let listed = client
        .get(format!("http://{addr}/code/harnesses/claude_code/models"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(listed["kind"], "claude_code");
    assert_eq!(listed["source"], "harness");
    let rows = listed["models"].as_array().unwrap();
    let ids: Vec<&str> = rows.iter().map(|row| row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["fast-thinker", "steady"]);
    assert_eq!(rows[0]["default"], true);
    assert_eq!(rows[0]["reasoning_efforts"], serde_json::json!(["high"]));
    assert_eq!(rows[0]["fast_mode"], true);
    assert_eq!(rows[1]["reasoning_efforts"], serde_json::json!(["low"]));
    assert_eq!(
        listed["reasoning_efforts"],
        serde_json::to_value(ReasoningEffort::ALL).unwrap()
    );
}

/// Decision 0031's honesty mechanism, end to end: a parser that could not read
/// part of a stream must leave a durable, readable count behind. A build that
/// counts drops but never persists them is indistinguishable from one that
/// drops silently, which is the failure the record exists to prevent.
#[tokio::test]
async fn unread_engine_events_accumulate_on_the_session_row_and_reach_the_doctor() {
    let (router, token, _runtime, dir) =
        code_app_with(ScriptedAdapter::new(plain_text_script()).with_unrecognized_per_turn(2))
            .await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;

    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(session["unrecognized_event_count"], 0);

    for message in ["hello", "again"] {
        let turn = client
            .post(format!(
                "http://{addr}/code/sessions/{}/turns",
                json_id(&session)
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "message": message }))
            .send()
            .await
            .unwrap();
        assert_eq!(turn.status(), reqwest::StatusCode::ACCEPTED);
    }

    // Both turns, not just the last: the row accumulates rather than being
    // overwritten with whatever the newest turn happened to see.
    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<Vec<serde_json::Value>>()
        .await
        .unwrap();
    assert_eq!(listed[0]["unrecognized_event_count"], 4);

    let report = client
        .get(format!("http://{addr}/code/harnesses"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(report["harnesses"][0]["unrecognized_event_count"], 4);
}

/// A resume ref the engine has lost wedges the session otherwise: every turn
/// fails identically, the session stays idle, and nothing offers a reap.
#[tokio::test]
async fn a_lost_resume_fences_the_session_instead_of_failing_every_turn() {
    let adapter =
        ScriptedAdapter::new(plain_text_script()).with_lost_resume("thread not found: dead-thread");
    let (router, token, runtime, dir) = code_app_with(adapter).await;
    let addr = serve(router).await;
    let client = reqwest::Client::new();
    let repo = init_git_repo(dir.path());
    let (_repo, workspace) = register_and_workspace(&client, addr, &token, &repo).await;
    let session = client
        .post(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "harness": "claude_code",
            "permission_mode": "plan",
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let session_id = json_id(&session).to_owned();
    let parsed: SessionId = session_id.parse().unwrap();

    // The session carries a ref from an earlier engine process.
    let mut row = tidebreak_core::db::code::get_session(
        &runtime.db,
        &tidebreak_core::OwnerId::local(),
        parsed,
    )
    .await
    .unwrap()
    .unwrap();
    row.harness_resume_ref = Some("dead-thread".into());
    assert!(tidebreak_core::db::code::save_session(&runtime.db, &row)
        .await
        .unwrap());

    let failed = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "carry on" }))
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);

    let listed = client
        .get(format!(
            "http://{addr}/code/workspaces/{}/sessions",
            json_id(&workspace)
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let after = listed[0].clone();
    assert_eq!(after["lifecycle"], "fenced");
    assert_eq!(after["fence_reason"]["type"], "resume_lost");
    assert_eq!(
        after["fence_reason"]["detail"],
        "thread not found: dead-thread"
    );
    assert_eq!(after["attention"]["state"]["type"], "fenced");
    assert!(
        after["harness_resume_ref"].is_null(),
        "the fence must drop the dead ref so a reap starts a fresh session: {after}"
    );

    // Fenced, so the next turn is refused with the reap the UI offers rather
    // than another identical failure.
    let refused = client
        .post(format!("http://{addr}/code/sessions/{session_id}/turns"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "message": "again" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::CONFLICT);
    let refused_body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(refused_body["kind"], "session_fenced");
}
