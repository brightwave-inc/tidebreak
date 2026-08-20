use super::*;

struct AppearingGatewaySecrets {
    gateway: String,
    reads: AtomicUsize,
}

#[async_trait]
impl SecretProvider for AppearingGatewaySecrets {
    async fn get_secret(&self, key: &str) -> Result<Option<String>> {
        if key != crate::connectors::GATEWAY_SECRET_KEY {
            return Ok(None);
        }
        Ok((self.reads.fetch_add(1, Ordering::SeqCst) > 0).then(|| self.gateway.clone()))
    }

    async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
        Ok(())
    }

    async fn delete_secret(&self, _key: &str) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_model_takes_precedence_over_the_default() {
    let recorder = RecordingProvider::default();
    let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
    let bearer = format!("Bearer {token}");

    // A global default is set, but the chat picks its own model — the chat wins.
    let set_default = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "default-model"}),
    )
    .await;
    assert_eq!(set_default.status(), StatusCode::OK);
    let chat = make_chat(&router, &bearer).await;
    let patched = patch_chat(
        &router,
        &bearer,
        chat.id,
        serde_json::json!({"model": "chat-model"}),
    )
    .await;
    assert_eq!(patched.status(), StatusCode::OK);

    assert_eq!(
        send_message(&router, &bearer, chat.id, "hi").await,
        StatusCode::ACCEPTED
    );
    wait_for_turn(&store, chat.id).await;
    assert!(
        recorder
            .models
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "chat-model"),
        "the chat's own model should win over the global default"
    );
}

#[tokio::test]
async fn settings_default_then_update_roundtrips() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Default: no model configured.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(response).await;
    assert!(settings["model"].is_null());
    assert_eq!(
        settings["max_active_background_agents"],
        tidebreak_core::AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS
    );
    assert_eq!(settings["sandbox_agent_checkin_steps"], 100);
    assert_eq!(settings["sandbox_agent_error_checkin"], 5);
    assert_eq!(settings["compaction"]["threshold_fraction"], 0.75);
    assert_eq!(settings["compaction"]["target_fraction"], 0.25);
    assert_eq!(settings["compaction"]["min_threshold_tokens"], 50000);
    assert_eq!(settings["compaction"]["protect_recent_messages"], 5);
    assert!(settings.get("code_mode_enabled").is_none());

    // PUT a model, and it comes back.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "model": "claude-x",
                        "max_active_background_agents": 7,
                        "sandbox_agent_checkin_steps": 250,
                        "sandbox_agent_error_checkin": 3,
                        "compaction": {
                            "threshold_fraction": 0.8,
                            "target_fraction": 0.3,
                            "min_threshold_tokens": 40000,
                            "protect_recent_messages": 8
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(settings["model"], "claude-x");
    assert_eq!(settings["max_active_background_agents"], 7);
    assert_eq!(settings["sandbox_agent_checkin_steps"], 250);
    assert_eq!(settings["sandbox_agent_error_checkin"], 3);
    assert_eq!(settings["compaction"]["threshold_fraction"], 0.8);
    assert_eq!(settings["compaction"]["target_fraction"], 0.3);
    assert_eq!(settings["compaction"]["min_threshold_tokens"], 40000);
    assert_eq!(settings["compaction"]["protect_recent_messages"], 8);

    // GET reflects the update.
    let response = router
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(settings["model"], "claude-x");
    assert_eq!(settings["max_active_background_agents"], 7);
    assert_eq!(settings["sandbox_agent_checkin_steps"], 250);
    assert_eq!(settings["sandbox_agent_error_checkin"], 3);
    assert_eq!(settings["compaction"]["threshold_fraction"], 0.8);
    assert_eq!(settings["compaction"]["protect_recent_messages"], 8);
}

#[tokio::test]
async fn settings_reject_active_background_agents_above_concurrency_limit() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let over_cap = tidebreak_core::AgentRun::MAX_CONCURRENCY_LIMIT + 1;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({ "max_active_background_agents": over_cap }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(
        settings["max_active_background_agents"],
        tidebreak_core::AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS
    );
}

async fn put_mcp_servers(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn put_native_mcp_servers(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/native/mcp/servers")
                .header(header::AUTHORIZATION, bearer)
                .header(
                    crate::auth::CLIENT_EXECUTOR_HEADER,
                    crate::state::TEST_CLIENT_EXECUTOR_TOKEN,
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_mcp_servers(router: &Router, bearer: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

#[tokio::test]
async fn mcp_settings_roundtrip_disabled_typed_definitions_without_credentials() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/mcp/servers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let response = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({
            "servers": [{
                "name": "private_docs",
                "command": "/not/started/while/disabled",
                "args": ["--stdio"],
                "env": ["LOG_LEVEL"],
                "env_values": {"LOG_LEVEL": "value-hunter2-not-a-real-key"},
                "env_from": ["PATH"],
                "cwd": "/tmp",
                "request_timeout_ms": 2500,
                "enabled": false
            }]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(response).await;
    assert_eq!(info["servers"][0]["health"], "disabled");
    assert_eq!(info["servers"][0]["tool_count"], 0);
    assert_eq!(info["servers"][0]["env_from"], serde_json::json!(["PATH"]));
    let encoded = info.to_string();
    assert!(!encoded.contains("resolved_value"));
    assert!(!encoded.contains("inherit_env"));
    // The name comes back; the value the request set does not, on the
    // response or on the fetch after it.
    assert_eq!(info["servers"][0]["env"], serde_json::json!(["LOG_LEVEL"]));
    assert!(
        !encoded.contains("value-hunter2"),
        "an environment value leaked into renderer JSON"
    );
    if let Ok(path) = std::env::var("PATH") {
        assert!(
            !encoded.contains(&path),
            "resolved PATH leaked into renderer JSON"
        );
    }

    let fetched = get_mcp_servers(&router, &bearer).await;
    assert_eq!(fetched, info);
}

#[tokio::test]
async fn enabled_local_mcp_commands_require_the_native_executor_surface() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let body = serde_json::json!({
        "servers": [{
            "name": "local_command",
            "command": "/not/started",
            "enabled": true
        }]
    });

    let renderer = put_mcp_servers(&router, &bearer, body.clone()).await;
    assert_eq!(renderer.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(renderer).await;
    assert_eq!(error.kind, "native_confirmation_required");

    let native = put_native_mcp_servers(&router, &bearer, body).await;
    assert_ne!(native.status(), StatusCode::UNAUTHORIZED);
    let error: AgentErrorInfo = json_body(native).await;
    assert_ne!(error.kind, "native_confirmation_required");
}

#[tokio::test]
async fn failed_mcp_candidate_does_not_replace_the_active_configuration() {
    const MISSING: &str = "TIDEBREAK_TEST_MCP_ROUTE_MISSING_ENV_65B7A2";
    assert!(std::env::var_os(MISSING).is_none());
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let initial = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({
            "servers": [{
                "name": "kept",
                "command": "/not/started",
                "enabled": false
            }]
        }),
    )
    .await;
    assert_eq!(initial.status(), StatusCode::OK);

    let failed = put_native_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({
            "servers": [{
                "name": "broken",
                "command": "/not/reached",
                "env_from": [MISSING]
            }]
        }),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(failed).await;
    assert_eq!(error.kind, "bad_request");
    assert!(error.message.contains("broken"));
    assert!(error.message.contains(MISSING));

    let active = get_mcp_servers(&router, &bearer).await;
    assert_eq!(active["servers"][0]["name"], "kept");
}

#[tokio::test]
async fn mcp_settings_reject_environment_inheritance_and_unknown_fields() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({
            "servers": [{
                "name": "unsafe",
                "command": "/bin/false",
                "inherit_env": true
            }]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(response).await;
    assert_eq!(error.kind, "bad_request");
    assert!(error.message.contains("unknown field"));
}

/// PUT /settings with a raw JSON body, returning the response.
async fn put_settings(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn put_empty_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = put_settings(&router, &bearer, serde_json::json!({"model": ""})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn put_non_string_model_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    // A number where a string is expected fails extraction as a JSON 400.
    let response = put_settings(&router, &bearer, serde_json::json!({"model": 5})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn explicit_null_model_clears_a_configured_one() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Set, then clear with an explicit null.
    let set = put_settings(&router, &bearer, serde_json::json!({"model": "claude-x"})).await;
    assert_eq!(set.status(), StatusCode::OK);
    let cleared = put_settings(&router, &bearer, serde_json::json!({"model": null})).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(cleared).await;
    assert!(
        settings["model"].is_null(),
        "explicit null resets the model"
    );

    // An empty body leaves the (now-cleared) value unchanged.
    let untouched = put_settings(&router, &bearer, serde_json::json!({})).await;
    let settings: serde_json::Value = json_body(untouched).await;
    assert!(settings["model"].is_null());
}

#[tokio::test]
async fn model_visibility_overrides_replace_rather_than_merge() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let stored = put_settings(
        &router,
        &bearer,
        serde_json::json!({
            "model_visibility_overrides": {
                "anthropic::claude-opus-4-6": "show",
                "openai::gpt-5.6-sol": "hide",
            }
        }),
    )
    .await;
    assert_eq!(stored.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(stored).await;
    assert_eq!(
        settings["model_visibility_overrides"],
        serde_json::json!({
            "anthropic::claude-opus-4-6": "show",
            "openai::gpt-5.6-sol": "hide",
        })
    );

    // Absent leaves the map alone; present replaces it wholesale, so a single
    // remaining key drops the other override rather than merging over it.
    let untouched = put_settings(&router, &bearer, serde_json::json!({})).await;
    let settings: serde_json::Value = json_body(untouched).await;
    assert_eq!(
        settings["model_visibility_overrides"]
            .as_object()
            .unwrap()
            .len(),
        2
    );

    let replaced = put_settings(
        &router,
        &bearer,
        serde_json::json!({
            "model_visibility_overrides": {"openai::gpt-5.6-sol": "hide"}
        }),
    )
    .await;
    let settings: serde_json::Value = json_body(replaced).await;
    assert_eq!(
        settings["model_visibility_overrides"],
        serde_json::json!({"openai::gpt-5.6-sol": "hide"})
    );

    // An empty map is how a client resets everything to the curated defaults.
    let reset = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model_visibility_overrides": {}}),
    )
    .await;
    let settings: serde_json::Value = json_body(reset).await;
    assert_eq!(
        settings["model_visibility_overrides"],
        serde_json::json!({})
    );

    // A key that is not provider-qualified is rejected: it could never match a
    // catalog row, so storing it would silently do nothing forever.
    let malformed = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model_visibility_overrides": {"claude-opus-5": "hide"}}),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

/// `has_api_key` from `GET /settings`.
async fn api_key_configured(router: &Router, bearer: &str) -> bool {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/settings")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    json_body::<serde_json::Value>(response).await["has_api_key"]
        .as_bool()
        .unwrap()
}

#[tokio::test]
async fn api_key_put_configures_it_and_delete_reverts() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Capture the env-dependent baseline so the test is deterministic wherever
    // it runs, then assert the transitions the API drives.
    let baseline = api_key_configured(&router, &bearer).await;

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": "sk-test"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::NO_CONTENT);
    assert!(api_key_configured(&router, &bearer).await);

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);
    assert_eq!(api_key_configured(&router, &bearer).await, baseline);
}

#[tokio::test]
async fn put_empty_api_key_is_rejected() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/api-key")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"api_key": ""}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "bad_request");
}

#[tokio::test]
async fn web_search_credential_routes_are_authenticated_and_never_return_keys() {
    let (router, token, secrets, _dir) = test_app_with_secrets().await;
    let bearer = format!("Bearer {token}");

    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/web-search/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let initial = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/web-search/credentials")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(initial).await,
        serde_json::json!({
            "credentials": [
                {"provider": "exa", "has_credential": false},
                {"provider": "tavily", "has_credential": false},
                {"provider": "brave", "has_credential": false}
            ]
        })
    );

    let key = "exa-secret-that-must-not-cross-the-api-boundary";
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/web-search/credentials/exa")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"api_key": key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_body = to_bytes(put.into_body(), usize::MAX).await.unwrap();
    assert!(!std::str::from_utf8(&put_body).unwrap().contains(key));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&put_body).unwrap(),
        serde_json::json!({"provider": "exa", "has_credential": true})
    );
    assert_eq!(
        secrets
            .get_secret("web_search.exa.api_key")
            .await
            .unwrap()
            .as_deref(),
        Some(key)
    );
    assert_eq!(
        secrets
            .get_secret("web_search.tavily.api_key")
            .await
            .unwrap(),
        None
    );

    let deleted = router
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/web-search/credentials/exa")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(deleted).await,
        serde_json::json!({"provider": "exa", "has_credential": false})
    );
    assert_eq!(
        secrets.get_secret("web_search.exa.api_key").await.unwrap(),
        None
    );
}

#[tokio::test]
async fn web_search_credential_write_validates_fixed_provider_and_key_bounds() {
    let (router, token, secrets, _dir) = test_app_with_secrets().await;
    let bearer = format!("Bearer {token}");

    for body in [
        serde_json::json!({"api_key": ""}),
        serde_json::json!({"api_key": " \n\t "}),
        serde_json::json!({"api_key": "x".repeat(8 * 1024 + 1)}),
        serde_json::json!({"api_key": "valid", "unexpected": true}),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/web-search/credentials/exa")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let unknown = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/web-search/credentials/arbitrary-secret-name")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": "valid"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert!(secrets.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn code_execution_config_route_is_authenticated_and_preserves_explicit_disable() {
    let (router, token, secrets, _dir) = test_app_with_secrets().await;
    let bearer = format!("Bearer {token}");

    let unauthenticated = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let initial = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    let initial: serde_json::Value = json_body(initial).await;
    // The untouched default selects Local only where its sandbox exists, so
    // the truthful initial read differs by host: "local" on a supporting
    // platform, null elsewhere.
    if tidebreak_code_execution::LocalExecutionProvider::availability().is_ok() {
        assert_eq!(initial["provider"], "local");
    } else {
        assert!(initial["provider"].is_null());
    }
    assert_eq!(
        initial["timeout_ms"],
        crate::code_execution::DEFAULT_TIMEOUT_MS
    );
    assert!(initial["available"].is_boolean());
    assert_eq!(initial["has_credential"], false);

    // The detached-admission wire shape: one row per execution provider, each
    // carrying the gate's named denial reasons. No provider is admitted today,
    // and the local row names exactly the structural gaps — the scoped-token
    // issuer, the external lifetime cap, and image verification.
    let admission = initial["detached_admission"]
        .as_array()
        .expect("detached_admission is an array");
    assert_eq!(
        admission
            .iter()
            .map(|row| row["provider"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["local", "e2b", "daytona", "docker"]
    );
    let local = &admission[0];
    assert_eq!(local["admitted"], false);
    // With the published digest pinned, the local backend's image precondition
    // is satisfied, so its denial list carries only the two remaining gates.
    assert_eq!(
        local["denials"],
        serde_json::json!(["no_scoped_model_token", "no_external_lifetime_cap"])
    );

    let unauthenticated_credentials = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        unauthenticated_credentials.status(),
        StatusCode::UNAUTHORIZED
    );

    let credentials = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution/credentials")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(credentials.status(), StatusCode::OK);
    assert_eq!(
        json_body::<serde_json::Value>(credentials).await,
        serde_json::json!({
            "credentials": [
                {"provider": "e2b", "has_credential": false},
                {"provider": "daytona", "has_credential": false}
            ]
        }),
        "local execution needs no credential and has no slot to report"
    );

    let disabled = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": null,
                        "timeout_ms": crate::code_execution::MIN_TIMEOUT_MS,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disabled.status(), StatusCode::OK);
    let disabled: serde_json::Value = json_body(disabled).await;
    assert!(
        disabled.get("provider").is_none(),
        "an explicit disable omits the provider key"
    );
    assert_eq!(
        disabled["timeout_ms"],
        crate::code_execution::MIN_TIMEOUT_MS
    );
    assert_eq!(disabled["available"], false);
    assert_eq!(disabled["has_credential"], false);

    let e2b = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "e2b",
                        "timeout_ms": crate::code_execution::DEFAULT_TIMEOUT_MS,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(e2b.status(), StatusCode::OK);
    let e2b: serde_json::Value = json_body(e2b).await;
    assert_eq!(e2b["provider"], "e2b");
    assert_eq!(e2b["timeout_ms"], crate::code_execution::DEFAULT_TIMEOUT_MS);
    assert_eq!(e2b["available"], false);
    assert_eq!(e2b["has_credential"], false);
    // Egress defaults to open and is disclosed on the same surface: managed
    // sandboxes stay open-internet until a policy is configured.
    assert_eq!(e2b["egress"]["policy"]["mode"], "open");

    let unknown_credential = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution/credentials/arbitrary-secret-name")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": "must-not-be-stored"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown_credential.status(), StatusCode::NOT_FOUND);
    assert!(secrets.0.lock().unwrap().is_empty());

    let key = "test-e2b-key";
    let saved = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution/credentials/e2b")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"api_key": key}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let saved_body = to_bytes(saved.into_body(), usize::MAX).await.unwrap();
    assert!(!std::str::from_utf8(&saved_body).unwrap().contains(key));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&saved_body).unwrap(),
        serde_json::json!({"provider": "e2b", "has_credential": true})
    );
    assert_eq!(
        secrets
            .get_secret(tidebreak_code_execution::E2B_CREDENTIAL_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(key)
    );

    let ready = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ready: serde_json::Value = json_body(ready).await;
    assert_eq!(ready["provider"], "e2b");
    assert_eq!(ready["available"], true);
    assert_eq!(ready["has_credential"], true);

    let daytona = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "provider": "daytona",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(daytona.status(), StatusCode::OK);
    let daytona: serde_json::Value = json_body(daytona).await;
    assert_eq!(daytona["provider"], "daytona");
    assert_eq!(daytona["available"], false);
    assert_eq!(daytona["has_credential"], false);

    let daytona_key = "test-daytona-key";
    let saved = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/code-execution/credentials/daytona")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"api_key": daytona_key}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    let saved_body = to_bytes(saved.into_body(), usize::MAX).await.unwrap();
    assert!(!std::str::from_utf8(&saved_body)
        .unwrap()
        .contains(daytona_key));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&saved_body).unwrap(),
        serde_json::json!({"provider": "daytona", "has_credential": true})
    );
    assert_eq!(
        secrets
            .get_secret(tidebreak_code_execution::DAYTONA_CREDENTIAL_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(daytona_key)
    );
    assert_eq!(
        secrets
            .get_secret(tidebreak_code_execution::E2B_CREDENTIAL_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(key),
        "managed providers retain separate fixed credential slots"
    );

    let credentials = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution/credentials")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        json_body::<serde_json::Value>(credentials).await,
        serde_json::json!({
            "credentials": [
                {"provider": "e2b", "has_credential": true},
                {"provider": "daytona", "has_credential": true}
            ]
        }),
        "readiness is reported per provider, not only for the selected one"
    );

    let ready = router
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ready: serde_json::Value = json_body(ready).await;
    assert_eq!(ready["provider"], "daytona");
    assert_eq!(ready["available"], true);
    assert_eq!(ready["has_credential"], true);
}

#[tokio::test]
async fn code_execution_egress_policy_round_trips_and_rejects_secrets() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let put_egress = |body: serde_json::Value| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri("/code-execution")
                        .header(header::AUTHORIZATION, &bearer)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };

    // The default surface discloses open egress plus each managed provider's
    // enforcement status: E2B applied-with-gaps, Daytona a boundary conditional
    // on its org tier.
    let initial = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let initial: serde_json::Value = json_body(initial).await;
    assert_eq!(initial["egress"]["policy"]["mode"], "open");
    let enforcement = initial["egress"]["enforcement"].as_array().unwrap();
    let row = |provider: &str| {
        enforcement
            .iter()
            .find(|row| row["provider"] == provider)
            .unwrap_or_else(|| panic!("{provider} enforcement is disclosed"))
            .clone()
    };
    // E2B is applied but honestly not a full boundary. Daytona's per-sandbox
    // policy is a strict, live-confirmed boundary, but gated on org tier 3+, so
    // it surfaces as a conditional boundary with the requirement inline — never
    // an unconditional boundary, and with no phantom curated-service gaps.
    assert_eq!(row("e2b")["status"], "applied_with_gaps");
    assert!(!row("e2b")["gaps"].as_array().unwrap().is_empty());
    assert_eq!(
        row("daytona")["status"],
        "conditional_boundary",
        "Daytona is a strict per-sandbox boundary, conditional on org tier 3+"
    );
    assert_eq!(row("daytona")["requirement"], "Daytona org tier 3+");
    assert!(
        row("daytona")["gaps"].as_array().unwrap().is_empty(),
        "the phantom curated-service exceptions must be gone"
    );

    // A configured allowlist round-trips through the store and back out.
    let saved = put_egress(serde_json::json!({
        "egress": {
            "mode": "allowlist",
            "domains": ["*.pypi.org", "crates.io"],
            "cidrs": ["140.82.112.0/20"],
        }
    }))
    .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved: serde_json::Value = json_body(saved).await;
    assert_eq!(saved["egress"]["policy"]["mode"], "allowlist");
    assert_eq!(
        saved["egress"]["policy"]["domains"],
        serde_json::json!(["*.pypi.org", "crates.io"])
    );
    assert_eq!(
        saved["egress"]["policy"]["cidrs"],
        serde_json::json!(["140.82.112.0/20"])
    );

    // A malformed grant is a bad request, never a silent widening to open.
    let malformed = put_egress(serde_json::json!({
        "egress": { "mode": "allowlist", "domains": ["not a host"], "cidrs": [] }
    }))
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    // No secret or endpoint is accepted on this surface: an extra field is
    // rejected rather than stored.
    let with_endpoint = put_egress(serde_json::json!({
        "egress": { "mode": "allowlist", "domains": [], "cidrs": [] },
        "endpoint": "https://exfil.example",
    }))
    .await;
    assert!(
        with_endpoint.status().is_client_error(),
        "an unknown field is rejected, never stored: {}",
        with_endpoint.status()
    );

    // The last valid policy is still in place after the rejected writes.
    let current = router
        .oneshot(
            Request::builder()
                .uri("/code-execution")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let current: serde_json::Value = json_body(current).await;
    assert_eq!(current["egress"]["policy"]["mode"], "allowlist");
    assert_eq!(
        current["egress"]["policy"]["domains"],
        serde_json::json!(["*.pypi.org", "crates.io"])
    );
}

#[tokio::test]
async fn providers_list_and_put_roundtrip() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(list).await;
    let providers = body["providers"].as_array().unwrap();
    assert!(providers.iter().any(|p| p["kind"] == "anthropic"));
    assert!(providers.iter().any(|p| p["kind"] == "openai"));
    assert!(providers.iter().any(|p| {
        p["kind"] == "fireworks" && p["base_url"] == "https://api.fireworks.ai/inference/v1"
    }));
    assert!(providers
        .iter()
        .any(|p| p["kind"] == "together" && p["base_url"] == "https://api.together.ai/v1"));
    assert!(providers
        .iter()
        .any(|p| { p["kind"] == "openrouter" && p["base_url"] == "https://openrouter.ai/api/v1" }));
    assert!(providers
        .iter()
        .any(|p| { p["kind"] == "ollama" && p["base_url"] == "http://127.0.0.1:11434/v1" }));
    assert!(providers.iter().any(|p| p["kind"] == "openai_compatible"));

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "sk-openai"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(put).await;
    assert_eq!(info["kind"], "openai");
    assert_eq!(info["enabled"], true);
    assert_eq!(info["has_credential"], true);
    assert!(info.get("credential").is_none());
    assert!(!info.to_string().contains("sk-openai"));

    // Credential never appears on the list either.
    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = json_body(list).await;
    let openai = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "openai")
        .unwrap();
    assert_eq!(openai["has_credential"], true);
    assert!(openai.get("credential").is_none());
    assert!(!body.to_string().contains("sk-openai"));

    let delete = router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/providers/openai/credential")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::NO_CONTENT);

    let list = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value = json_body(list).await;
    let openai = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "openai")
        .unwrap();
    assert_eq!(openai["has_credential"], false);
}

/// Storing a credential is enough to route: the provider turns on even when
/// the write omits `enabled`, matching ChatGPT sign-in completion and the
/// CLI's "set-key enables the provider" contract. An explicit later disable
/// still leaves the credential in place.
#[tokio::test]
async fn writing_a_provider_credential_enables_the_provider() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Start disabled with no credential.
    let disable = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"enabled": false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(disable.status(), StatusCode::OK);

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "credential": {"type": "api_key", "key": "sk-auto-enable"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(put).await;
    assert_eq!(info["enabled"], true);
    assert_eq!(info["has_credential"], true);
    assert_eq!(info["auth_mode"], "api_key");

    // Disable without touching the credential — still credentialed, just off.
    let off = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"enabled": false}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(off.status(), StatusCode::OK);
    let off_info: serde_json::Value = json_body(off).await;
    assert_eq!(off_info["enabled"], false);
    assert_eq!(off_info["has_credential"], true);
}

#[tokio::test]
async fn openai_compatible_requires_base_url_when_enabled() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai_compatible")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn direct_compatible_presets_refuse_endpoint_overrides() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/fireworks")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"base_url": "https://example.test/v1"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unknown_provider_kind_is_404() {
    let (router, token, _store, _dir) = test_app().await;
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/not-a-provider")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"enabled": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn models_catalog_includes_enabled_credentialed_providers() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "sk-openai"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    let models = catalog["models"].as_array().unwrap();
    assert!(models
        .iter()
        .any(|m| m["provider"] == "openai" && m["available"] == true));
    // API-key auth keeps the API-only nano row usable.
    assert!(models
        .iter()
        .any(|m| { m["key"] == "openai::gpt-5.4-nano" && m["available"] == true }));
}

#[tokio::test]
async fn chatgpt_auth_marks_api_only_openai_models_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::Oauth {},
    )
    .await
    .unwrap();
    secrets
        .set_secret(
            crate::connectors::CHATGPT_SECRET_KEY,
            &serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "account_id": "acct-test",
                "expires_at_unix": 4_102_444_800_u64,
            })
            .to_string(),
        )
        .await
        .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();
    let catalog = providers::catalog_models(&*store, &*secrets, &policy)
        .await
        .unwrap();
    let nano = catalog
        .iter()
        .find(|m| m.policy.id == "gpt-5.4-nano")
        .expect("nano stays in the catalog");
    assert!(
        !nano.available,
        "ChatGPT/Codex cannot run gpt-5.4-nano; available must be false"
    );
    let sol = catalog
        .iter()
        .find(|m| m.policy.id == "gpt-5.6-sol")
        .expect("sol stays in the catalog");
    assert!(sol.available, "a ChatGPT flagship must remain selectable");

    // Selection through model_is_usable must agree with the catalog stance.
    let nano_policy = providers::ResolvedModelPolicy::curated(
        crate::model_registry::find_for(providers::ProviderKind::Openai, "gpt-5.4-nano").unwrap(),
    );
    assert!(
        !providers::model_is_usable(&*store, &*secrets, &nano_policy, &policy)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn xai_settings_publish_curated_and_explicit_model_capabilities() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    let endpoint_override = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/xai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"base_url": "https://example.test/v1"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(endpoint_override.status(), StatusCode::BAD_REQUEST);

    let key_only = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/xai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "xai-key"},
                        "models": []
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(key_only.status(), StatusCode::OK);
    let configured: serde_json::Value = json_body(key_only).await;
    assert_eq!(configured["models"], serde_json::json!([]));

    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/xai")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "credential": {"type": "api_key", "key": "xai-key"},
                        "models": [{
                            "id": "grok-account-model",
                            "display_name": "Grok account model",
                            "context_window": 500000,
                            "max_output_tokens": 32768,
                            "input_modalities": ["text", "image"],
                            "supports_reasoning": true,
                            "reasoning_efforts": ["none", "low", "medium", "high", "xhigh"]
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    let grok_ids: Vec<_> = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|model| model["provider"] == "xai")
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert!(grok_ids.contains(&"grok-4.6"));
    assert!(grok_ids.contains(&"grok-4.5"));
    let grok = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["key"] == "xai::grok-4.6")
        .unwrap();
    assert!(grok["available"].as_bool().unwrap());
    let model = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["key"] == "xai::grok-account-model")
        .unwrap();
    assert_eq!(model["provider"], "xai");
    assert_eq!(
        model["input_modalities"],
        serde_json::json!(["text", "image"])
    );
    assert_eq!(
        model["reasoning_efforts"],
        serde_json::json!(["none", "low", "medium", "high", "xhigh"])
    );
    assert!(model["supports_reasoning"].as_bool().unwrap());
    assert!(model["available"].as_bool().unwrap());
}

#[tokio::test]
async fn xai_config_builds_a_provider_qualified_native_route() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Xai,
        &providers::ProviderCredential::api_key("xai-key"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Xai,
        &providers::ProviderConfig {
            enabled: true,
            // Seed a value that the public update path refuses, as a stale DB
            // row or direct write could still contain it. Route collection
            // must not let it redirect the credential.
            base_url: Some("https://attacker.invalid/v1".into()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, tidebreak_router::RouteKind::Xai);
    assert_eq!(routes[0].base_url, None);
    assert_eq!(routes[0].curated_models, ["grok-4.6", "grok-4.5"]);

    let grok = providers::resolve_model_policy(&*store, "grok-4.5", false)
        .await
        .unwrap()
        .expect("a bare curated xAI id resolves to its direct provider");
    assert_eq!(grok.provider, providers::ProviderKind::Xai);
    let curated = providers::resolve_model_policy(&*store, "gpt-5.6-sol", false)
        .await
        .unwrap()
        .expect("a bare curated id keeps the registry's direct owner");
    assert_eq!(curated.provider, providers::ProviderKind::Openai);

    let router = tidebreak_router::Router::build(routes);
    assert_eq!(
        router.select_for(Some(&tidebreak_core::ProviderId::new("xai")), "grok-4.5"),
        Some(tidebreak_router::RouteKind::Xai)
    );
    assert_eq!(
        router.select_for(Some(&tidebreak_core::ProviderId::new("openai")), "grok-4.5"),
        None
    );
}

#[tokio::test]
async fn configured_router_canonicalizes_typed_models_and_rejects_wrong_or_unavailable_providers() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("typed-models.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_config(
        &*store,
        providers::ProviderKind::Openai,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openai,
        &providers::ProviderCredential::api_key("sk-openai"),
    )
    .await
    .unwrap();
    let state = AppState::new(
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
            model: "gpt-5.6-sol".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    // Old bare curated ids are accepted but persisted under their exact owner.
    let configured = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "gpt-5.6-sol"}),
    )
    .await;
    assert_eq!(configured.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(configured).await;
    assert_eq!(settings["model"], "openai::gpt-5.6-sol");

    let wrong_provider = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "anthropic::gpt-5.6-sol"}),
    )
    .await;
    assert_eq!(wrong_provider.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(wrong_provider).await;
    assert_eq!(error.kind, "unknown_model");
    assert_eq!(
        error.message,
        "model `gpt-5.6-sol` is not registered for provider `anthropic`"
    );

    for (selection, message) in [
        (
            "fireworks::accounts/fireworks/models/not-a-model",
            "model `accounts/fireworks/models/not-a-model` is not registered for provider `fireworks`",
        ),
        (
            "together::not-a-model",
            "model `not-a-model` is not registered for provider `together`",
        ),
    ] {
        let response =
            put_settings(&router, &bearer, serde_json::json!({"model": selection})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: AgentErrorInfo = json_body(response).await;
        assert_eq!(error.kind, "unknown_model");
        assert_eq!(error.message, message);
        assert!(!error.message.contains("OpenAI-compatible"));
    }

    let custom_unknown = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "openai_compatible::not-a-model"}),
    )
    .await;
    assert_eq!(custom_unknown.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(custom_unknown).await;
    assert_eq!(error.kind, "unknown_model");
    assert_eq!(
        error.message,
        "model `not-a-model` is not configured under OpenAI-compatible models"
    );

    let unavailable = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "anthropic::claude-opus-4-8"}),
    )
    .await;
    assert_eq!(unavailable.status(), StatusCode::CONFLICT);
    let error: AgentErrorInfo = json_body(unavailable).await;
    assert_eq!(error.kind, "model_provider_unavailable");

    let chat_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "openai::gpt-5.6-sol"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chat_response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(chat_response).await;
    assert_eq!(chat.model.as_deref(), Some("openai::gpt-5.6-sol"));

    let turn_id = TurnId::new();
    let accepted = send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await;
    assert_eq!(accepted, StatusCode::ACCEPTED);
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().model,
        "openai::gpt-5.6-sol"
    );

    // The same raw id may be explicitly configured under another provider.
    // Once two owners exist, the old bare value is ambiguous and fails closed.
    let configure_compatible = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai_compatible")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "base_url": "https://compat.example/v1",
                        "credential": {"type": "api_key", "key": "sk-local"},
                        "models": [{
                            "id": "gpt-5.6-sol",
                            "context_window": 32768,
                            "max_output_tokens": 4096
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(configure_compatible.status(), StatusCode::OK);

    let ambiguous = put_settings(
        &router,
        &bearer,
        serde_json::json!({"model": "gpt-5.6-sol"}),
    )
    .await;
    assert_eq!(ambiguous.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(ambiguous).await;
    assert_eq!(error.kind, "unknown_model");

    for qualified in ["openai::gpt-5.6-sol", "openai_compatible::gpt-5.6-sol"] {
        let exact = put_settings(&router, &bearer, serde_json::json!({"model": qualified})).await;
        assert_eq!(exact.status(), StatusCode::OK);
        let settings: serde_json::Value = json_body(exact).await;
        assert_eq!(settings["model"], qualified);
    }
}

/// Roles resolve on read, against whatever the user has credentialed: the
/// ordered defaults skip providers that cannot serve a request, a pin overrides
/// them, and enabling a provider changes the answer without a restart. When the
/// profile flips to managed, utility re-routes to an entitled gateway model.
/// An unresolved explicit chat pin remains unresolved until the user chooses,
/// while an implicit boot default may still use the gateway's first model.
#[tokio::test]
async fn model_roles_resolve_at_read_time_and_honor_an_explicit_pin() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("model-roles.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let state = AppState::new(
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
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "gpt-5.6-sol".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    let configure_provider = |kind: &'static str, body: serde_json::Value| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!("/providers/{kind}"))
                        .header(header::AUTHORIZATION, &bearer)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
    };
    let put_role = |role: &'static str, body: serde_json::Value| {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            router
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!("/models/roles/{role}"))
                        .header(header::AUTHORIZATION, &bearer)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    let role_rows = || {
        let router = router.clone();
        let bearer = bearer.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/models")
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let catalog: serde_json::Value = json_body(response).await;
            catalog["roles"].clone()
        }
    };
    let role_row = |roles: &serde_json::Value, role: &str| {
        roles
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["role"] == role)
            .cloned()
            .unwrap_or_else(|| panic!("the catalog reports the {role} role"))
    };

    // Nothing credentialed: no utility model, so its consumers skip their work
    // rather than borrowing the conversation's model.
    let roles = role_rows().await;
    assert_eq!(
        role_row(&roles, "utility")["resolved_key"],
        serde_json::Value::Null
    );

    configure_provider(
        "openai",
        serde_json::json!({
            "enabled": true,
            "credential": {"type": "api_key", "key": "sk-openai"}
        }),
    )
    .await;

    // No restart between the two reads: credentialing OpenAI is enough for the
    // ordered defaults to land on its cheapest row.
    let roles = role_rows().await;
    let utility = role_row(&roles, "utility");
    assert_eq!(utility["selection"], serde_json::Value::Null);
    assert_eq!(utility["resolved_key"], "openai::gpt-5.4-nano");
    // The chat role is unchanged: its last resort is still the boot default.
    assert_eq!(
        role_row(&roles, "chat")["resolved_key"],
        "openai::gpt-5.6-sol"
    );

    // A pin wins over the defaults, and only a model that could actually run is
    // accepted.
    let pinned = put_role(
        "utility",
        serde_json::json!({"selection": "openai::gpt-5.4-mini"}),
    )
    .await;
    assert_eq!(pinned.status(), StatusCode::OK);
    let pinned: serde_json::Value = json_body(pinned).await;
    assert_eq!(pinned["resolved_key"], "openai::gpt-5.4-mini");
    assert_eq!(
        role_row(&role_rows().await, "utility")["selection"],
        "openai::gpt-5.4-mini"
    );

    configure_provider(
        "together",
        serde_json::json!({
            "enabled": true,
            "credential": {"type": "api_key", "key": "together-key"}
        }),
    )
    .await;

    let catalog_response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(catalog_response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(catalog_response).await;
    for id in [
        "moonshotai/Kimi-K3",
        "moonshotai/Kimi-K2.7-Code",
        "moonshotai/Kimi-K2.6",
    ] {
        let model = catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["provider"] == "together" && model["id"] == id)
            .unwrap_or_else(|| panic!("the API reports Together `{id}`"));
        assert!(model["supports_structured_output"].as_bool().unwrap());
    }
    let kimi = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["key"] == "together::moonshotai/Kimi-K3")
        .unwrap();
    assert!(kimi["supports_tools"].as_bool().unwrap());

    let kimi_pin = put_role(
        "utility",
        serde_json::json!({"selection": "together::moonshotai/Kimi-K3"}),
    )
    .await;
    assert_eq!(kimi_pin.status(), StatusCode::OK);
    let kimi_pin: serde_json::Value = json_body(kimi_pin).await;
    assert_eq!(kimi_pin["resolved_key"], "together::moonshotai/Kimi-K3");

    let incompatible = put_role(
        "utility",
        serde_json::json!({"selection": "together::thinkingmachines/Inkling-Small"}),
    )
    .await;
    assert_eq!(incompatible.status(), StatusCode::CONFLICT);
    let error: AgentErrorInfo = json_body(incompatible).await;
    assert_eq!(error.kind, "model_structured_output_unsupported");
    assert_eq!(
        role_row(&role_rows().await, "utility")["selection"],
        "together::moonshotai/Kimi-K3",
        "a rejected utility pin must not replace the capable selection"
    );

    let unavailable = put_role(
        "utility",
        serde_json::json!({"selection": "anthropic::claude-haiku-4-5-20251001"}),
    )
    .await;
    assert_eq!(unavailable.status(), StatusCode::CONFLICT);

    let unknown_role = put_role("titling", serde_json::json!({"selection": null})).await;
    assert_eq!(unknown_role.status(), StatusCode::NOT_FOUND);

    // Cleared, then OpenAI turned off and Gemini turned on: the walk moves to
    // the next entry it can serve.
    let cleared = put_role("utility", serde_json::json!({"selection": null})).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    configure_provider("openai", serde_json::json!({"enabled": false})).await;
    configure_provider(
        "gemini",
        serde_json::json!({
            "enabled": true,
            "credential": {"type": "api_key", "key": "gemini-key"}
        }),
    )
    .await;
    assert_eq!(
        role_row(&role_rows().await, "utility")["resolved_key"],
        "gemini::gemini-3.5-flash-lite"
    );
    // Unmanaged, the chat role reports even a dead default (OpenAI is now
    // disabled) rather than re-routing it: the open experience refuses loudly
    // at send instead of silently moving a foreground turn. This pins the
    // unmanaged early return in `effective_chat_policy`.
    assert_eq!(
        role_row(&role_rows().await, "chat")["resolved_key"],
        "openai::gpt-5.6-sol"
    );

    // A managed flip with BYOK chat choices carried in from before it. Direct
    // selections remain durable; managed reads and turns resolve a unique
    // current gateway equivalent without rewriting them.
    configure_provider("openai", serde_json::json!({"enabled": true})).await;
    configure_provider(
        "anthropic",
        serde_json::json!({
            "enabled": true,
            "credential": {"type": "api_key", "key": "sk-anthropic"}
        }),
    )
    .await;
    let pinned = put_role(
        "chat",
        serde_json::json!({"selection": "openai::gpt-5.6-sol"}),
    )
    .await;
    assert_eq!(pinned.status(), StatusCode::OK);
    // A chat with its own explicit override, created while OpenAI could still
    // serve it — the re-route must not cover it after the flip.
    let overridden_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "openai::gpt-5.6-sol"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(overridden_response.status(), StatusCode::CREATED);
    let overridden: Chat = json_body(overridden_response).await;
    let gateway_overridden_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"model": "anthropic::claude-opus-5"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gateway_overridden_response.status(), StatusCode::CREATED);
    let gateway_overridden: Chat = json_body(gateway_overridden_response).await;

    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(dir.path()),
        "https://corp.gateway",
    )
    .unwrap();
    let credentials: crate::connectors::GatewayCredentials =
        serde_json::from_value(serde_json::json!({
            "base_url": "https://corp.gateway/",
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
    crate::connectors::CredentialVault::new(secrets)
        .save(&credentials)
        .await
        .unwrap();
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: "https://corp.gateway/".to_string(),
            installation_id: Some("install-1".into()),
            // Flagship-first, as a gateway well might list them: an implicit
            // chat default takes the first pick, while utility must not.
            models: vec![
                providers::CustomModelConfig {
                    id: "gateway-flagship".to_string(),
                    upstream_id: Some("claude-opus-5".to_string()),
                    display_name: Some("Gateway Flagship".to_string()),
                    context_window: 1_000_000,
                    max_output_tokens: 64_000,
                    ..Default::default()
                },
                providers::CustomModelConfig {
                    id: "gateway-haiku".to_string(),
                    upstream_id: Some("claude-haiku-4-5-20251001".to_string()),
                    display_name: Some("Gateway Haiku".to_string()),
                    context_window: 200_000,
                    max_output_tokens: 8_192,
                    ..Default::default()
                },
            ],
            model_protocols: Default::default(),
            member_catalog: None,
            catalog_etag: None,
        },
    )
    .await
    .unwrap();

    let roles = role_rows().await;
    let chat = role_row(&roles, "chat");
    assert_eq!(chat["selection"], "openai::gpt-5.6-sol");
    assert_eq!(chat["resolved_key"], serde_json::Value::Null);
    assert_eq!(
        role_row(&roles, "utility")["resolved_key"],
        "model_gateway::gateway-haiku"
    );

    // The explicit per-chat canonical selection is portable. The durable row
    // remains canonical while this turn freezes the unique current gateway
    // route that represents it.
    let gateway_override_turn = TurnId::new();
    assert_eq!(
        send_message_with_id(
            &router,
            &bearer,
            gateway_overridden.id,
            gateway_override_turn,
            "hello"
        )
        .await,
        StatusCode::ACCEPTED
    );
    let gateway_override_model = store
        .get_turn_run(gateway_override_turn)
        .await
        .unwrap()
        .unwrap()
        .model;
    assert!(gateway_override_model.starts_with("model_gateway::__tidebreak_gateway_v1."));
    assert_eq!(
        providers::resolve_model_policy(&*store, &gateway_override_model, false)
            .await
            .unwrap()
            .unwrap()
            .id,
        "gateway-flagship"
    );
    assert_eq!(
        store
            .get_chat(gateway_overridden.id)
            .await
            .unwrap()
            .unwrap()
            .model
            .as_deref(),
        Some("anthropic::claude-opus-5")
    );

    // Clear the global pin: the explicit canonical sticky choice made by the
    // prior chat survives independently and resolves through the same unique
    // gateway route without being rewritten.
    let cleared = put_role("chat", serde_json::json!({"selection": null})).await;
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared: serde_json::Value = json_body(cleared).await;
    assert_eq!(cleared["resolved_key"], "model_gateway::gateway-flagship");
    let chat_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(chat_response.status(), StatusCode::CREATED);
    let chat: Chat = json_body(chat_response).await;
    assert_eq!(chat.model.as_deref(), Some("anthropic::claude-opus-5"));
    let sticky_turn = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, sticky_turn, "hello").await,
        StatusCode::ACCEPTED
    );
    let sticky_model = store
        .get_turn_run(sticky_turn)
        .await
        .unwrap()
        .unwrap()
        .model;
    assert!(sticky_model.starts_with("model_gateway::__tidebreak_gateway_v1."));
    assert_eq!(
        providers::resolve_model_policy(&*store, &sticky_model, false)
            .await
            .unwrap()
            .unwrap()
            .id,
        "gateway-flagship"
    );

    // An unmatched sticky choice remains explicit and is refused honestly
    // instead of degrading to the first gateway model.
    store
        .set_setting(
            "chat_default.model",
            &serde_json::Value::String("openai::gpt-5.6-sol".into()),
        )
        .await
        .unwrap();
    let unmatched_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unmatched_response.status(), StatusCode::CREATED);
    let unmatched: Chat = json_body(unmatched_response).await;
    assert_eq!(unmatched.model.as_deref(), Some("openai::gpt-5.6-sol"));
    assert_eq!(
        send_message_with_id(&router, &bearer, unmatched.id, TurnId::new(), "hello").await,
        StatusCode::CONFLICT
    );

    // Clearing the sticky value restores automatic behavior. The boot default
    // is process state rather than persisted user intent, so managed chat may
    // fall back to the gateway's first entitled model.
    store
        .set_setting("chat_default.model", &serde_json::Value::Null)
        .await
        .unwrap();
    let fallback_chat_response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/chats")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(fallback_chat_response.status(), StatusCode::CREATED);
    let fallback_chat: Chat = json_body(fallback_chat_response).await;
    let fallback_turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(
            &router,
            &bearer,
            fallback_chat.id,
            fallback_turn_id,
            "hello"
        )
        .await,
        StatusCode::ACCEPTED
    );
    let fallback_model = store
        .get_turn_run(fallback_turn_id)
        .await
        .unwrap()
        .unwrap()
        .model;
    assert!(fallback_model.starts_with("model_gateway::__tidebreak_gateway_v1."));
    assert_eq!(
        providers::resolve_model_policy(&*store, &fallback_model, false)
            .await
            .unwrap()
            .unwrap()
            .id,
        "gateway-flagship"
    );

    // An unmatched per-chat override remains explicit and is refused honestly.
    assert_eq!(
        send_message_with_id(&router, &bearer, overridden.id, TurnId::new(), "hello").await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn custom_models_live_under_openai_compatible_with_conservative_capabilities() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai_compatible")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "base_url": "https://compat.example/v1",
                        "credential": {"type": "api_key", "key": "sk-local"},
                        "models": [{
                            "id": "vendor/model",
                            "display_name": "Vendor Model",
                            "context_window": 65536,
                            "max_output_tokens": 8192
                        }]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = router
        .oneshot(
            Request::builder()
                .uri("/models")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let catalog: serde_json::Value = json_body(response).await;
    let custom = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["key"] == "openai_compatible::vendor/model")
        .unwrap();
    assert_eq!(custom["display_name"], "Vendor Model");
    assert_eq!(custom["context_window"], 65_536);
    assert_eq!(custom["max_output_tokens"], 8_192);
    assert_eq!(custom["input_modalities"], serde_json::json!(["text"]));
    assert!(!custom["supports_reasoning"].as_bool().unwrap());
    assert!(!custom["supports_structured_output"].as_bool().unwrap());
    assert!(custom["available"].as_bool().unwrap());
}

#[tokio::test]
async fn resolver_builds_a_router_from_enabled_providers() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-test"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let resolver = resolver::KeyedResolver::new(
        store.clone(),
        secrets.clone(),
        crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            crate::managed_policy::MemoryProvisionedPolicy::new(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        ),
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        crate::managed_policy::MemoryProvisionedPolicy::new(),
        Arc::new(crate::managed_policy::NoOsPolicy),
    );
    let resolved = resolver.resolve().await;
    // Composite router — selection happens on stream from req.model.
    assert_eq!(resolved.id().0, "router");

    // Same route set ⇒ the cached provider is reused.
    let again = resolver.resolve().await;
    assert!(Arc::ptr_eq(&resolved, &again));

    // Changing the key rebuilds it.
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-different"),
    )
    .await
    .unwrap();
    let rebuilt = resolver.resolve().await;
    assert!(!Arc::ptr_eq(&resolved, &rebuilt));
    assert_eq!(rebuilt.id().0, "router");

    // Disabling Anthropic with no other providers fails closed.
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: false,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    assert_eq!(resolver.resolve().await.id().0, "unconfigured");
}

#[tokio::test]
async fn resolver_includes_configured_curated_api_key_providers() {
    for (kind, model, route_kind) in [
        (
            providers::ProviderKind::Openai,
            "gpt-5.6-sol",
            tidebreak_router::RouteKind::Openai,
        ),
        (
            providers::ProviderKind::Gemini,
            "gemini-3.6-flash",
            tidebreak_router::RouteKind::Gemini,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}/test.db?mode=rwc",
                dir.path().display()
            ))
            .await
            .unwrap(),
        );
        let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
        providers::write_credential(
            &*secrets,
            kind,
            &providers::ProviderCredential::api_key("test-api-key"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            kind,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();

        let policy = crate::managed_policy::resolve(
            &*crate::managed_policy::MemoryProvisionedPolicy::new(),
            &crate::managed_policy::NoOsPolicy,
        )
        .unwrap();
        let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, route_kind);

        let resolver = resolver::KeyedResolver::new(
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
        );
        let provider = resolver.resolve().await;
        assert_eq!(provider.id().0, "router");

        // Curated models stay on their explicitly configured native route;
        // the absence of a compatibility fallback must not cross providers.
        let router = tidebreak_router::Router::build(routes);
        assert_eq!(router.select(model), Some(route_kind));
        assert_eq!(router.select("claude-opus-4-8"), None);
    }
}

#[tokio::test]
async fn openai_compatible_route_is_free_form_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderCredential::api_key("sk-local"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::OpenaiCompatible,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://compat.example/v1".into()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
    let router = tidebreak_router::Router::build(routes);
    assert_eq!(
        router.select("llama-3-local"),
        Some(tidebreak_router::RouteKind::OpenaiCompatible)
    );
}

#[tokio::test]
async fn direct_compatible_presets_use_fixed_endpoints_and_distinct_routes() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());

    for kind in [
        providers::ProviderKind::Fireworks,
        providers::ProviderKind::Together,
    ] {
        providers::write_credential(
            &*secrets,
            kind,
            &providers::ProviderCredential::api_key(format!("{kind}-key")),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            kind,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
    }

    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
    let fireworks = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Fireworks)
        .unwrap();
    let together = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Together)
        .unwrap();

    assert_eq!(
        fireworks.base_url.as_deref(),
        Some("https://api.fireworks.ai/inference/v1")
    );
    assert_eq!(
        together.base_url.as_deref(),
        Some("https://api.together.ai/v1")
    );
    assert!(fireworks
        .curated_models
        .contains(&"accounts/fireworks/models/kimi-k3".to_owned()));
    assert!(together
        .curated_models
        .contains(&"moonshotai/Kimi-K3".to_owned()));

    let router = tidebreak_router::Router::build(routes);
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("fireworks")),
            "accounts/fireworks/models/kimi-k3"
        ),
        Some(tidebreak_router::RouteKind::Fireworks)
    );
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("together")),
            "moonshotai/Kimi-K3"
        ),
        Some(tidebreak_router::RouteKind::Together)
    );
}

#[tokio::test]
async fn ollama_is_usable_without_a_credential_and_serves_only_configured_models() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();

    assert!(
        !providers::provider_is_usable(
            &*store,
            &*secrets,
            providers::ProviderKind::Ollama,
            &policy
        )
        .await
        .unwrap(),
        "a disabled Ollama card is not a route"
    );

    providers::write_config(
        &*store,
        providers::ProviderKind::Ollama,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: vec![providers::CustomModelConfig {
                id: "qwen3:0.6b".into(),
                display_name: Some("Qwen 3 0.6B".into()),
                ..providers::CustomModelConfig::default()
            }],
        },
    )
    .await
    .unwrap();

    assert!(providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::Ollama,
        &policy
    )
    .await
    .unwrap());
    assert!(
        !providers::has_credential(&*secrets, providers::ProviderKind::Ollama).await,
        "usability must not invent a stored credential"
    );

    let listed = providers::list_providers(&*store, &*secrets, &policy)
        .await
        .unwrap();
    let ollama = listed
        .iter()
        .find(|info| info.kind == providers::ProviderKind::Ollama)
        .expect("Ollama is a first-class provider");
    assert!(ollama.enabled);
    assert!(!ollama.has_credential);
    assert_eq!(
        ollama.base_url.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );

    let catalog = providers::catalog_models(&*store, &*secrets, &policy)
        .await
        .unwrap();
    let model = catalog
        .iter()
        .find(|entry| entry.policy.key == "ollama::qwen3:0.6b")
        .expect("the configured Ollama model is in the catalog");
    assert!(model.available);
    assert_eq!(model.policy.display_name, "Qwen 3 0.6B");
    assert!(model.policy.supports_tools);
    assert!(!model.policy.supports_reasoning);

    let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
    let route = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Ollama)
        .expect("an enabled Ollama daemon is a route");
    assert!(route.api_key.is_empty());
    assert_eq!(route.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
    assert_eq!(route.curated_models, ["qwen3:0.6b"]);

    let router = tidebreak_router::Router::build(routes);
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("ollama")),
            "qwen3:0.6b"
        ),
        Some(tidebreak_router::RouteKind::Ollama)
    );
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("ollama")),
            "llama3.2:1b"
        ),
        None
    );
}

#[tokio::test]
async fn openrouter_uses_its_fixed_endpoint_and_serves_only_configured_models() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let policy = crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap();

    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Openrouter,
        &providers::ProviderCredential::api_key("sk-or"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Openrouter,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://attacker.invalid/v1".into()),
            models: vec![providers::CustomModelConfig {
                id: "anthropic/claude-sonnet-4".into(),
                display_name: Some("Claude Sonnet 4".into()),
                ..providers::CustomModelConfig::default()
            }],
        },
    )
    .await
    .unwrap();

    assert!(providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::Openrouter,
        &policy
    )
    .await
    .unwrap());

    let listed = providers::list_providers(&*store, &*secrets, &policy)
        .await
        .unwrap();
    let openrouter = listed
        .iter()
        .find(|info| info.kind == providers::ProviderKind::Openrouter)
        .expect("OpenRouter is a first-class provider");
    assert!(openrouter.enabled);
    assert!(openrouter.has_credential);
    assert_eq!(
        openrouter.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );

    let catalog = providers::catalog_models(&*store, &*secrets, &policy)
        .await
        .unwrap();
    let model = catalog
        .iter()
        .find(|entry| entry.policy.key == "openrouter::anthropic/claude-sonnet-4")
        .expect("the configured OpenRouter model is in the catalog");
    assert!(model.available);
    assert_eq!(model.policy.display_name, "Claude Sonnet 4");
    assert!(model.policy.supports_tools);
    assert!(!model.policy.supports_reasoning);
    assert!(!model.policy.supports_vendor_web_search);

    let routes = providers::collect_routes(&*store, &*secrets, None, None, &policy).await;
    let route = routes
        .iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Openrouter)
        .expect("an enabled OpenRouter key is a route");
    assert_eq!(route.api_key, "sk-or");
    assert_eq!(
        route.base_url.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(route.curated_models, ["anthropic/claude-sonnet-4"]);

    let router = tidebreak_router::Router::build(routes);
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("openrouter")),
            "anthropic/claude-sonnet-4"
        ),
        Some(tidebreak_router::RouteKind::Openrouter)
    );
    assert_eq!(
        router.select_for(
            Some(&tidebreak_core::ProviderId::new("openrouter")),
            "openai/gpt-4o"
        ),
        None
    );
}

/// Serializes and scopes process-environment mutation in tests: restores the
/// previous value on drop (unwind included), and the lock keeps concurrent
/// env-mutating tests from interleaving set/restore windows.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct ScopedEnv {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnv {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        match &self.previous {
            Some(previous) => std::env::set_var(self.key, previous),
            None => std::env::remove_var(self.key),
        }
    }
}

/// The one behavioral fork managed lockdown introduces on the read path:
/// route collection offers the gateway alone, aimed at the policy URL.
#[tokio::test]
async fn a_managed_profile_offers_only_the_gateway_route() {
    struct StaticTokens;

    #[async_trait]
    impl tidebreak_router::BearerTokenSource for StaticTokens {
        async fn bearer_token(&self) -> tidebreak_core::Result<String> {
            Ok("mg_at_test".into())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let tokens: Arc<dyn tidebreak_router::BearerTokenSource> = Arc::new(StaticTokens);

    // A fully configured BYOK spread: Anthropic on a stored key, Gemini on the
    // env-var fallback, and a gateway row whose stored base URL is stale.
    let _env = ENV_LOCK.lock().await;
    let _gemini_key = ScopedEnv::set("GEMINI_API_KEY", "env-fallback-key");
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-stored"),
    )
    .await
    .unwrap();
    for (kind, base_url) in [
        (providers::ProviderKind::Anthropic, None),
        (providers::ProviderKind::Gemini, None),
        (
            providers::ProviderKind::ModelGateway,
            Some("https://stale.example".to_string()),
        ),
    ] {
        providers::write_config(
            &*store,
            kind,
            &providers::ProviderConfig {
                enabled: true,
                base_url,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();
    }

    // Unmanaged control: the same store offers the BYOK routes, and the env
    // fallback is honored — so the managed delta below is load-bearing. The
    // enabled legacy gateway row builds nothing even with a token source in
    // hand: policy is the only gateway source in both directions.
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    let kinds: Vec<_> =
        providers::collect_routes(&*store, &*secrets, Some(tokens.clone()), None, &policy)
            .await
            .into_iter()
            .map(|route| route.kind)
            .collect();
    assert!(kinds.contains(&tidebreak_router::RouteKind::Anthropic));
    assert!(kinds.contains(&tidebreak_router::RouteKind::Gemini));
    assert!(!kinds.contains(&tidebreak_router::RouteKind::ModelGateway));

    // Provisioned: only the gateway remains, aimed at the policy URL. The
    // stale stored row and every BYOK credential — stored or env — are inert.
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    let routes =
        providers::collect_routes(&*store, &*secrets, Some(tokens.clone()), None, &policy).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, tidebreak_router::RouteKind::ModelGateway);
    assert_eq!(
        routes[0].base_url.as_deref(),
        Some("https://corp.gateway/compat/anthropic")
    );

    // The same authority on the picker side: no BYOK provider is usable.
    assert!(!providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::Anthropic,
        &policy
    )
    .await
    .unwrap());

    // Even a disabled — or, for a pure-MDM profile, absent — stored gateway
    // row cannot drop the managed route: policy is the authority.
    providers::write_config(
        &*store,
        providers::ProviderKind::ModelGateway,
        &providers::ProviderConfig::disabled(),
    )
    .await
    .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, Some(tokens), None, &policy).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, tidebreak_router::RouteKind::ModelGateway);
}

/// A deployment with no stored endpoint takes its provider base URL from the
/// environment, and a stored endpoint still wins over it.
#[tokio::test]
async fn a_base_url_environment_fallback_reaches_the_route() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-stored"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let _env = ENV_LOCK.lock().await;
    let _base_url = ScopedEnv::set("ANTHROPIC_BASE_URL", "https://relay.example/v1");
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    let route = providers::collect_routes(&*store, &*secrets, None, None, &policy)
        .await
        .into_iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Anthropic)
        .expect("the stored key must offer an Anthropic route");
    assert_eq!(route.base_url.as_deref(), Some("https://relay.example/v1"));

    // A stored endpoint outranks the variable.
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: Some("https://stored.example/v1".to_string()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let route = providers::collect_routes(&*store, &*secrets, None, None, &policy)
        .await
        .into_iter()
        .find(|route| route.kind == tidebreak_router::RouteKind::Anthropic)
        .expect("the stored key must offer an Anthropic route");
    assert_eq!(route.base_url.as_deref(), Some("https://stored.example/v1"));
}

/// The resolver's fail-closed branch: a profile whose stored policy exists
/// but cannot be read must resolve to no egress, not to its BYOK routes.
#[tokio::test]
async fn an_unreadable_policy_fails_the_resolver_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-ready"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let provisioned_policy: Arc<dyn crate::managed_policy::ProvisionedPolicySource> = Arc::new(
        crate::managed_policy::ProvisionedPolicyFile::in_data_dir(dir.path()),
    );
    let resolver = resolver::KeyedResolver::new(
        store.clone(),
        secrets.clone(),
        crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            provisioned_policy.clone(),
            Arc::new(crate::managed_policy::NoOsPolicy),
        ),
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        provisioned_policy,
        Arc::new(crate::managed_policy::NoOsPolicy),
    );
    assert_eq!(resolver.resolve().await.id().0, "router");

    // Corrupt the persisted policy file: the profile now claims management
    // the process cannot read, and the resolver fails closed.
    std::fs::write(
        dir.path().join("gateway-policy.json"),
        br#"{"gateway_url": 42}"#,
    )
    .unwrap();
    assert_eq!(resolver.resolve().await.id().0, "unconfigured");
}

/// `misconfigured` is a fail-closed security state end to end: `/policy`
/// reporting it is the exact wire signal the renderer blocks the whole app
/// on, and the accept path refuses the turn a blocked gate can no longer
/// send. Driven over the real routes with the production resolver, against a
/// profile whose BYOK setup demonstrably serves a turn until the policy
/// breaks — so the refusal below is the policy's doing and nothing else's.
#[tokio::test]
async fn a_misconfigured_policy_gates_the_renderer_and_refuses_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("misconfigured.db").display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    providers::write_credential(
        &*secrets,
        providers::ProviderKind::Anthropic,
        &providers::ProviderCredential::api_key("sk-ready"),
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Anthropic,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();
    let state = AppState::new(
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
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "claude-opus-5".into(),
            ..AgentConfig::default()
        },
    );
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    // Control: with the policy healthy, this profile accepts a turn.
    let healthy_chat = make_chat(&router, &bearer).await;
    assert_eq!(
        send_message(&router, &bearer, healthy_chat.id, "hello").await,
        StatusCode::ACCEPTED
    );

    // The profile now claims to be managed but the claim cannot be read.
    std::fs::write(
        dir.path().join("gateway-policy.json"),
        br#"{"gateway_url": 42}"#,
    )
    .unwrap();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/policy")
                .header(header::AUTHORIZATION, &bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy: serde_json::Value = json_body(response).await;
    assert_eq!(policy["managed"], serde_json::json!(true));
    assert_eq!(policy["misconfigured"], serde_json::json!(true));

    // A turn cannot run: the send is refused at accept, and nothing was
    // queued for the worker to pick up behind the blocked gate.
    let gated_chat = make_chat(&router, &bearer).await;
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, gated_chat.id, turn_id, "hello").await,
        StatusCode::CONFLICT
    );
    assert!(store.get_turn_run(turn_id).await.unwrap().is_none());
}

pub(super) async fn put_json(
    router: &Router,
    bearer: &str,
    uri: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn delete(router: &Router, bearer: &str, uri: &str) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Every locked write path over the real routes: BYOK credential/endpoint
/// writes, the legacy api-key shim, and credential deletion refuse with the
/// stable `managed_profile` kind — while an enable toggle stays open — and
/// the model gateway kind refuses every write in every state with its own
/// stable `gateway_policy` kind: policy is the only gateway source.
#[tokio::test]
async fn a_managed_profile_refuses_byok_and_gateway_repoint_writes() {
    let (router, token, _store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");

    // Unmanaged first: the retired additive configuration is not writable
    // even on an open profile — there is no URL field to type into any more.
    let response = put_json(
        &router,
        &bearer,
        "/providers/model_gateway",
        serde_json::json!({"enabled": true, "base_url": "https://byo.gateway"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "gateway_policy");
    assert!(info.message.contains("pair via your gateway"));

    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(dir.path()),
        "https://corp.gateway",
    )
    .unwrap();

    // A BYOK credential write is refused with the stable managed kind.
    let response = put_json(
        &router,
        &bearer,
        "/providers/anthropic",
        serde_json::json!({"credential": {"type": "api_key", "key": "sk-blocked"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "managed_profile");
    assert!(!info.message.contains("sk-blocked"));

    // A BYOK endpoint write is refused; a plain enable toggle is not locked.
    let response = put_json(
        &router,
        &bearer,
        "/providers/openai_compatible",
        serde_json::json!({"base_url": "https://byo.example"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = put_json(
        &router,
        &bearer,
        "/providers/anthropic",
        serde_json::json!({"enabled": false}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Managed: still refused wholesale — a re-point, and even a write that
    // matches the policy URL, because the row it would write no longer
    // exists as a gateway source.
    for body in [
        serde_json::json!({"base_url": "https://evil.example"}),
        serde_json::json!({"base_url": "https://corp.gateway"}),
        serde_json::json!({"enabled": false}),
    ] {
        let response = put_json(&router, &bearer, "/providers/model_gateway", body).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "gateway_policy");
    }

    // The legacy api-key shim and credential deletion refuse the same way.
    let response = put_json(
        &router,
        &bearer,
        "/settings/api-key",
        serde_json::json!({"api_key": "sk-blocked"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = delete(&router, &bearer, "/settings/api-key").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = delete(&router, &bearer, "/providers/anthropic/credential").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The managed MCP write path over the real route: a manual transport is
/// refused with the same stable `managed_profile` kind the provider lockdown
/// uses and changes nothing, while the sanctioned gateway-endpoint mount is
/// accepted — carrying an already-configured manual server along untouched,
/// so mounting is not blocked by history the profile cannot edit any more.
#[tokio::test]
async fn a_managed_profile_refuses_manual_mcp_servers_and_accepts_gateway_mounts() {
    let (router, token, _store, dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let legacy = serde_json::json!({
        "name": "legacy_docs",
        "command": "/not/started/while/disabled",
        "enabled": false
    });

    // Configured before the profile was managed.
    let saved = put_mcp_servers(&router, &bearer, serde_json::json!({"servers": [legacy]})).await;
    assert_eq!(saved.status(), StatusCode::OK);
    crate::managed_policy::provision(
        &crate::managed_policy::ProvisionedPolicyFile::in_data_dir(dir.path()),
        "https://corp.gateway",
    )
    .unwrap();

    for candidate in [
        serde_json::json!([legacy, {"name": "added", "command": "/bin/docs", "enabled": false}]),
        serde_json::json!([
            legacy,
            {"name": "added", "url": "http://127.0.0.1:1/mcp", "enabled": false}
        ]),
        // Every edit of the pre-existing one is a change, whole-definition:
        // its command, its name, and — the one a coarser check would wave
        // through — flipping it back on.
        serde_json::json!([{"name": "legacy_docs", "command": "/bin/other", "enabled": false}]),
        serde_json::json!([{"name": "renamed", "command": "/not/started/while/disabled",
                            "enabled": false}]),
        serde_json::json!([{"name": "legacy_docs", "command": "/not/started/while/disabled",
                            "enabled": true}]),
    ] {
        let refused =
            put_native_mcp_servers(&router, &bearer, serde_json::json!({"servers": candidate}))
                .await;
        assert_eq!(refused.status(), StatusCode::CONFLICT, "{candidate}");
        let error: AgentErrorInfo = json_body(refused).await;
        assert_eq!(error.kind, "managed_profile");
    }

    // Nothing the refusals touched: the active configuration is what it was.
    let active = get_mcp_servers(&router, &bearer).await;
    assert_eq!(active["servers"].as_array().unwrap().len(), 1);
    assert_eq!(active["servers"][0]["name"], "legacy_docs");

    // A gateway mount is accepted — signed out it degrades in place rather
    // than failing the save — and the untouched manual definition rides along.
    let mounted = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({
            "servers": [legacy, {"name": "tools", "gateway_endpoint": "tools"}]
        }),
    )
    .await;
    assert_eq!(mounted.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(mounted).await;
    assert_eq!(info["servers"][1]["gateway_endpoint"], "tools");
    // The manual definition survives, forced down with a legible reason
    // instead of vanishing from the profile.
    assert_eq!(info["servers"][0]["health"], "disabled");
    assert_eq!(
        info["servers"][0]["diagnostic"],
        crate::mcp_config::MANAGED_DISABLED_DIAGNOSTIC
    );

    // Removing it is still possible: a candidate without it is not an edit.
    let removed = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({"servers": [{"name": "tools", "gateway_endpoint": "tools"}]}),
    )
    .await;
    assert_eq!(removed.status(), StatusCode::OK);
    let info: serde_json::Value = json_body(removed).await;
    assert_eq!(info["servers"].as_array().unwrap().len(), 1);
}

/// After an MDM re-point the stored session belongs to the superseded
/// deployment. Every route and token path already refuses it, so the
/// readiness surfaces must agree: reading it as usable would advertise
/// models no route can serve.
#[tokio::test]
async fn a_superseded_gateway_session_never_reads_usable() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}/test.db?mode=rwc",
            dir.path().display()
        ))
        .await
        .unwrap(),
    );
    let secrets: Arc<dyn SecretProvider> = Arc::new(MemSecrets::default());
    let seed = |secrets: Arc<dyn SecretProvider>, base_url: &'static str| async move {
        let credentials: crate::connectors::GatewayCredentials =
            serde_json::from_value(serde_json::json!({
                "base_url": base_url,
                "installation_id": "install-1",
                "user_id": "user-1",
                "refresh_token": "mg_rt_seed",
                "access_tokens": {}
            }))
            .unwrap();
        crate::connectors::CredentialVault::new(secrets)
            .save(&credentials)
            .await
            .unwrap();
    };
    seed(secrets.clone(), "https://old.gateway").await;
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    let policy =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();

    assert!(!providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::ModelGateway,
        &policy
    )
    .await
    .unwrap());
    let gateway = providers::list_providers(&*store, &*secrets, &policy)
        .await
        .unwrap()
        .into_iter()
        .find(|provider| provider.kind == providers::ProviderKind::ModelGateway)
        .unwrap();
    assert!(!gateway.has_credential);

    // A session and catalog for the policy's own deployment — trailing slash
    // included, per the shared normalization — read usable again. A matching
    // session alone is intentionally insufficient because no executable model
    // route can be admitted until its installation-bound catalog is present.
    seed(secrets.clone(), "https://corp.gateway/").await;
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: "https://corp.gateway/".to_string(),
            installation_id: Some("install-1".into()),
            models: vec![providers::CustomModelConfig {
                id: "sample-claude".into(),
                upstream_id: Some("claude-opus-5".into()),
                context_window: 200_000,
                max_output_tokens: 32_000,
                ..Default::default()
            }],
            model_protocols: std::collections::BTreeMap::new(),
            member_catalog: Some("v1".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();
    assert!(providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::ModelGateway,
        &policy
    )
    .await
    .unwrap());
}

#[tokio::test]
async fn a_credential_appearance_cannot_admit_a_plain_gateway_execution_key() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn Store> = Arc::new(
        DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("gateway-admission-race.db").display()
        ))
        .await
        .unwrap(),
    );
    let credentials: crate::connectors::GatewayCredentials =
        serde_json::from_value(serde_json::json!({
            "base_url": "https://corp.gateway/",
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
    let secrets: Arc<dyn SecretProvider> = Arc::new(AppearingGatewaySecrets {
        gateway: serde_json::to_string(&credentials).unwrap(),
        reads: AtomicUsize::new(0),
    });
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://corp.gateway").unwrap();
    let os_policy: Arc<dyn crate::managed_policy::OsPolicySource> =
        Arc::new(crate::managed_policy::NoOsPolicy);
    let gateway = crate::gateway_runtime::GatewayRuntime::new(
        store.clone(),
        secrets.clone(),
        provisioned.clone(),
        os_policy.clone(),
    );
    let resolver = Arc::new(resolver::ConfiguredResolver::new(
        store.clone(),
        secrets.clone(),
        gateway,
        Arc::new(
            crate::chatgpt_runtime::ChatGptRuntime::new(store.clone(), secrets.clone()).unwrap(),
        ),
        provisioned.clone(),
        os_policy.clone(),
    ));
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        resolver,
        secrets,
        Arc::new(ToolRegistry::new()),
        AgentConfig::default(),
    );
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: "https://corp.gateway/".into(),
            installation_id: Some("install-1".into()),
            models: vec![providers::CustomModelConfig {
                id: "gateway-opus".into(),
                upstream_id: Some("claude-opus-5".into()),
                context_window: 200_000,
                max_output_tokens: 32_000,
                ..Default::default()
            }],
            model_protocols: Default::default(),
            member_catalog: Some("v1".into()),
            catalog_etag: None,
        },
    )
    .await
    .unwrap();
    let router = app(state.clone());
    let bearer = format!("Bearer {}", state.token);
    let chat = make_chat(&router, &bearer).await;
    store
        .set_chat_model(chat.id, Some("model_gateway::gateway-opus".into()))
        .await
        .unwrap();
    let chat = store.get_chat(chat.id).await.unwrap().unwrap();

    let error = crate::routes::resolve_executable_chat_model(&state, &chat)
        .await
        .expect_err("a transiently unavailable managed route must not fall back to its plain key");
    assert_eq!(error.kind(), "model_provider_unavailable");
}

/// A router whose state carries a code-execution provider loaded from the
/// repository's own bundled skills and plugins, over `store`.
///
/// The catalog routes are only interesting against a real load: what they
/// project is exactly what staging and prompt composition read.
fn plugin_app(store: Arc<dyn Store>, data_dir: &std::path::Path) -> (Router, Arc<str>) {
    plugin_app_with_broker(store, data_dir, None)
}

fn plugin_app_with_broker(
    store: Arc<dyn Store>,
    data_dir: &std::path::Path,
    host_tool_broker: Option<Arc<dyn tidebreak_code_execution::HostToolBroker>>,
) -> (Router, Arc<str>) {
    let secrets = Arc::new(MemSecrets::default());
    let mut state = AppState::new(
        Config::desktop(data_dir),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    state.code_execution = Some(Arc::new(
        crate::code_execution::ConfiguredCodeExecutionProvider::new(
            store,
            secrets,
            data_dir.join("scratch"),
        )
        .with_skills(Some(root.join("skills")))
        .with_plugins(Some(root.join("plugins")))
        .with_host_tool_broker(host_tool_broker),
    ));
    let token = state.token.clone();
    (app(state), token)
}

async fn get_plugins(router: &Router, bearer: &str) -> serde_json::Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/plugins")
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json_body(response).await
}

async fn put_plugins_enabled(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/plugins/enabled")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[derive(Clone)]
struct FixedPluginArchiveFetcher {
    expected_url: String,
    archive: Arc<Vec<u8>>,
}

#[async_trait]
impl crate::plugin_install::PluginArchiveFetcher for FixedPluginArchiveFetcher {
    async fn fetch(
        &self,
        url: &str,
    ) -> std::result::Result<Vec<u8>, crate::plugin_install::PluginInstallError> {
        assert_eq!(url, self.expected_url);
        Ok((*self.archive).clone())
    }
}

fn plugin_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write as _;

    let cursor = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default();
    for (path, content) in files {
        archive.start_file(path, options).unwrap();
        archive.write_all(content).unwrap();
    }
    archive.finish().unwrap().into_inner()
}

fn plugin_install_app(
    store: Arc<dyn Store>,
    data_dir: &std::path::Path,
    fetcher: FixedPluginArchiveFetcher,
) -> (Router, String) {
    let secrets = Arc::new(MemSecrets::default());
    let mut state = AppState::new(
        Config::desktop(data_dir),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    state.code_execution = Some(Arc::new(
        crate::code_execution::ConfiguredCodeExecutionProvider::new(
            store,
            secrets,
            data_dir.join("scratch"),
        )
        .with_skills(Some(root.join("skills")))
        .with_plugins(Some(root.join("plugins")))
        .with_user_skills(Some(data_dir.join("skills")))
        .with_user_plugins(Some(data_dir.join("plugins")))
        .with_plugin_archive_fetcher(Arc::new(fetcher)),
    ));
    let token = state.token.clone();
    (app(state), format!("Bearer {token}"))
}

async fn post_plugin_install(
    router: &Router,
    bearer: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/install")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Contract: a standard repository containing one Agent Skills manifest is
/// fetched at the requested immutable revision, wrapped as a one-skill plugin,
/// copied into the data directory, and immediately visible through the normal
/// catalog loader.
#[tokio::test]
async fn installs_a_single_skill_plugin_end_to_end() {
    let (dir, store) = temp_db_store("plugin-install.db").await;
    let revision = "0123456789abcdef0123456789abcdef01234567";
    let archive = plugin_archive(&[
        (
            "meeting-notes-repo/SKILL.md",
            b"---\nname: meeting-notes\ndescription: Turn a discussion into concise notes.\nlicense: Apache-2.0\n---\n# Meeting notes\nCapture decisions and actions.\n",
        ),
        ("meeting-notes-repo/README.md", b"Published skill."),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: format!(
                "https://github.com/example/meeting-notes/archive/{revision}.tar.gz"
            ),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "git",
                "url": "https://github.com/example/meeting-notes.git",
                "revision": revision
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(installed["plugin"], "meeting-notes");
    assert_eq!(installed["compatibility"]["status"], "compatible");
    assert_eq!(installed["skipped"][0]["path"], "README.md");

    assert!(dir.path().join("plugins/meeting-notes/PLUGIN.md").is_file());
    assert!(dir.path().join("skills/meeting-notes/SKILL.md").is_file());
    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "meeting-notes")
        .unwrap();
    assert_eq!(plugin["origin"], "user");
    assert_eq!(plugin["enabled"], false);
    assert_eq!(plugin["skills"][0]["name"], "meeting-notes");
    assert_eq!(plugin["compatibility"]["status"], "compatible");
}

/// Contract: every declared skill is parsed before publication. One malformed
/// member rejects the entire bundle, leaving neither the plugin nor the valid
/// sibling behind as a standalone skill.
#[tokio::test]
async fn rejects_the_whole_plugin_when_any_skill_fails_to_parse() {
    let (dir, store) = temp_db_store("plugin-reject.db").await;
    let archive = plugin_archive(&[
        (
            "bad-plugin/PLUGIN.md",
            b"---\nname: imported-notes\ndisplay-name: Imported notes\ndescription: Two note-taking skills.\ncategory: other\nskills: [\"good-notes\", \"bad-notes\"]\n---\n",
        ),
        (
            "bad-plugin/skills/good-notes/SKILL.md",
            b"---\nname: good-notes\ndescription: Valid notes.\n---\nInstructions.\n",
        ),
        (
            "bad-plugin/skills/bad-notes/SKILL.md",
            b"---\nname: bad-notes\ndescription: Missing its closing fence.\n",
        ),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/imported-notes-v1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/imported-notes-v1.0.0.zip",
                "revision": "v1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!dir.path().join("plugins/imported-notes").exists());
    assert!(!dir.path().join("skills/good-notes").exists());
    assert!(!dir.path().join("skills/bad-notes").exists());
}

/// Contract: import compares exact declared pins with the prepared image
/// closure and records scripts as opaque assumptions. The stamp on disk and
/// the catalog projection must report the same visible limitation.
#[tokio::test]
async fn stamps_and_surfaces_static_plugin_compatibility() {
    let (dir, store) = temp_db_store("plugin-compatibility.db").await;
    let archive = plugin_archive(&[
        (
            "deploy-skill/SKILL.md",
            b"---\nname: deploy-reports\ndescription: Publish a generated report.\ndeps: { python: [\"not-in-tidebreak-image==1.2.3\"] }\n---\nRun the helper.\n",
        ),
        (
            "deploy-skill/scripts/deploy.py",
            b"print('requires external deployment tooling')\n",
        ),
        // A helper script that shares the standard manifest's name is skill
        // content, not a package manifest: this archive must still install
        // through the internal path.
        ("deploy-skill/scripts/plugin.json", b"{\"tool\": \"config\"}"),
        // Bundled MCP configuration is a standard-format component; a bare
        // skill package shipping one is told it will not be read.
        ("deploy-skill/mcp.json", b"{\"mcpServers\": {}}"),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/deploy-reports-v2.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/deploy-reports-v2.0.0.zip",
                "revision": "v2.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(installed["compatibility"]["status"], "limited");
    assert_eq!(
        installed["skipped"][0],
        serde_json::json!({
            "path": "mcp.json",
            "reason": "bundled MCP configuration is only read from Agent Plugins packages"
        })
    );
    assert_eq!(
        installed["compatibility"]["issues"],
        serde_json::json!([
            {
                "kind": "missing_sandbox_dependency",
                "skill": "deploy-reports",
                "dependency": "not-in-tidebreak-image==1.2.3"
            },
            {"kind": "scripts_present", "skill": "deploy-reports"}
        ])
    );

    let stamp: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            dir.path()
                .join("plugins/deploy-reports/.tidebreak-install.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stamp["compatibility"], installed["compatibility"]);
    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "deploy-reports")
        .unwrap();
    assert_eq!(plugin["compatibility"], installed["compatibility"]);
}

/// The only manifest schema the standard-format importer accepts.
const AGENT_PLUGIN_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";

/// The matching schema for a package's bundled MCP server configuration.
const AGENT_PLUGIN_MCP_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json";

/// Contract: a package published in the Agent Plugins format
/// (<https://agent-plugins.org>) is recognized by its root `plugin.json`, its
/// skills are discovered structurally under `skills/`, and a nonconforming
/// skill is skipped and reported rather than sinking the package — the
/// per-component failure grading that specification requires, and the
/// deliberate difference from the `PLUGIN.md` path, which refuses an import
/// whose named member does not parse.
#[tokio::test]
async fn installs_a_standard_format_plugin_and_skips_nonconforming_skills() {
    let (dir, store) = temp_db_store("plugin-standard.db").await;
    let manifest = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"reporting\", \
          \"version\": \"0.4.1\", \"license\": \"Apache-2.0\", \
          \"description\": \"Weekly and monthly reporting skills.\"}}"
    );
    let archive = plugin_archive(&[
        ("reporting-1.0.0/plugin.json", manifest.as_bytes()),
        (
            "reporting-1.0.0/skills/weekly-report/SKILL.md",
            b"---\nname: weekly-report\ndescription: Draft the weekly report.\n---\nInstructions.\n",
        ),
        (
            "reporting-1.0.0/skills/broken-report/SKILL.md",
            b"---\nname: broken-report\ndescription: Missing its closing fence.\n",
        ),
        (
            "reporting-1.0.0/skills/drafts/notes.md",
            b"A directory that is not a skill.\n",
        ),
        ("reporting-1.0.0/agents/reviewer.md", b"A foreign component.\n"),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/reporting-1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/reporting-1.0.0.zip",
                "revision": "1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(installed["plugin"], "reporting");
    assert_eq!(
        installed["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["agents", "skills/broken-report", "skills/drafts"]
    );

    // The conforming skill installed; the nonconforming sibling did not.
    assert!(dir.path().join("skills/weekly-report/SKILL.md").is_file());
    assert!(!dir.path().join("skills/broken-report").exists());
    let stamp: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("plugins/reporting/.tidebreak-install.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        stamp["source_format"],
        serde_json::json!({"kind": "agent_plugins", "spec_version": "1.0.0"})
    );

    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "reporting")
        .unwrap();
    assert_eq!(plugin["origin"], "user");
    assert_eq!(
        plugin["description"],
        "Weekly and monthly reporting skills."
    );
    assert_eq!(plugin["category"], "other");
    assert_eq!(plugin["skills"][0]["name"], "weekly-report");
    assert_eq!(plugin["skills"].as_array().unwrap().len(), 1);
}

/// Contract: `$schema` is how the standard selects validation rules, so an
/// identifier this client does not implement rejects the package whole rather
/// than being guessed at — and a package name Tidebreak cannot address is
/// refused with a reason instead of being silently rewritten.
#[tokio::test]
async fn rejects_standard_packages_this_client_cannot_honour() {
    for (case, manifest) in [
        (
            "unsupported schema version",
            "{\"$schema\": \"https://agent-plugins.org/schemas/9.9.9/plugin.schema.json\", \
              \"name\": \"reporting\"}"
                .to_owned(),
        ),
        (
            // Legal in the standard, which admits `.` in a package name;
            // Tidebreak addresses a plugin by a dot-free slug everywhere.
            "name Tidebreak cannot address",
            format!(
                "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"io.example.reporting\"}}"
            ),
        ),
    ] {
        let (dir, store) = temp_db_store("plugin-standard-reject.db").await;
        let archive = plugin_archive(&[
            ("pkg/plugin.json", manifest.as_bytes()),
            (
                "pkg/skills/weekly-report/SKILL.md",
                b"---\nname: weekly-report\ndescription: Draft the weekly report.\n---\nBody.\n",
            ),
        ]);
        let (router, bearer) = plugin_install_app(
            Arc::new(store),
            dir.path(),
            FixedPluginArchiveFetcher {
                expected_url: "https://example.com/reporting-1.0.0.zip".to_owned(),
                archive: Arc::new(archive),
            },
        );
        let response = post_plugin_install(
            &router,
            &bearer,
            serde_json::json!({
                "source": {
                    "kind": "archive",
                    "url": "https://example.com/reporting-1.0.0.zip",
                    "revision": "1.0.0"
                }
            }),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{case} should refuse the package"
        );
        assert!(!dir.path().join("skills/weekly-report").exists());
    }
}

/// Contract: the two violations the standard grades as non-fatal — an unknown
/// top-level field and a non-object `extensions` — keep the package
/// installable and are reported, and the data in Tidebreak's own extension
/// namespace reaches the installed manifest. Namespaces this client does not
/// implement are passed over untouched.
#[tokio::test]
async fn standard_manifests_report_ignored_fields_and_carry_our_extension() {
    let (dir, store) = temp_db_store("plugin-standard-extensions.db").await;
    let manifest = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"reporting\", \
          \"description\": \"Reporting skills.\", \"future-field\": [1, 2], \
          \"extensions\": {{\
            \"com.example.other-client\": {{\"whatever\": true}}, \
            \"io.brightwave.tidebreak\": {{\
              \"category\": \"data\", \
              \"router-preamble\": \"Pick by the reporting cadence the user asked for.\"}}}}}}"
    );
    let archive = plugin_archive(&[
        ("pkg/plugin.json", manifest.as_bytes()),
        (
            "pkg/skills/weekly-report/SKILL.md",
            b"---\nname: weekly-report\ndescription: Draft the weekly report.\n---\nBody.\n",
        ),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/reporting-1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/reporting-1.0.0.zip",
                "revision": "1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(
        installed["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["plugin.json#future-field"]
    );

    let written = std::fs::read_to_string(dir.path().join("plugins/reporting/PLUGIN.md")).unwrap();
    assert!(
        written.contains("category: data\n")
            && written
                .contains("router-preamble: Pick by the reporting cadence the user asked for.\n"),
        "our extension data should reach the installed manifest, got {written:?}"
    );
    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "reporting")
        .unwrap();
    assert_eq!(plugin["category"], "data");
}

/// Contract: an `mcp.json` bundled with a standard package is validated,
/// regenerated from what passed, and retained beside the installed manifest,
/// which is what earns the plugin its `mcp` badge. A single bad entry is
/// dropped and reported without touching the rest — the per-entry grading
/// <https://agent-plugins.org/specification> §7.2.2 requires.
#[tokio::test]
async fn retains_validated_mcp_servers_and_derives_the_badge() {
    let (dir, store) = temp_db_store("plugin-mcp.db").await;
    let manifest = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"reporting\", \
          \"description\": \"Reporting skills.\"}}"
    );
    let mcp = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_MCP_SCHEMA}\", \"mcpServers\": {{\
           \"local\": {{\"type\": \"stdio\", \"command\": \"./bin/serve\", \
             \"args\": [\"--root\", \"${{PLUGIN_ROOT}}\"]}}, \
           \"remote\": {{\"type\": \"streamable-http\", \
             \"url\": \"https://mcp.example.com/v1\"}}, \
           \"host-rce\": {{\"type\": \"stdio\", \"command\": \"python3\"}}, \
           \"bogus\": {{\"type\": \"stdio\", \"command\": \"serve\", \"retries\": 3}}}}}}"
    );
    let archive = plugin_archive(&[
        ("pkg/plugin.json", manifest.as_bytes()),
        ("pkg/mcp.json", mcp.as_bytes()),
        (
            "pkg/skills/weekly-report/SKILL.md",
            b"---\nname: weekly-report\ndescription: Draft the weekly report.\n---\nBody.\n",
        ),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/reporting-1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/reporting-1.0.0.zip",
                "revision": "1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(
        installed["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["mcp.json#bogus", "mcp.json#host-rce"]
    );

    // Only the entries that validated are written back.
    let retained: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("plugins/reporting/mcp.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(retained["$schema"], AGENT_PLUGIN_MCP_SCHEMA);
    assert_eq!(
        retained["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["local", "remote"]
    );

    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "reporting")
        .unwrap();
    assert_eq!(plugin["enabled"], false);
    assert!(plugin["capabilities"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("mcp")));
}

/// Contract: a public import that ships `mcp.json` is recorded off, so the
/// install-time reconcile does not start its servers. Enabling the plugin is
/// what connects them. Reverting either half — default-on at install, or
/// starting MCP without the enable flag — fails this.
#[tokio::test]
async fn imported_plugin_mcp_does_not_start_until_enabled() {
    let (dir, store) = temp_db_store("plugin-mcp-autostart.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let manifest = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"toolbox\", \
          \"description\": \"Bundled tools.\"}}"
    );
    let mcp_config = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_MCP_SCHEMA}\", \"mcpServers\": {{\
           \"local\": {{\"type\": \"stdio\", \"command\": \"./serve\"}}}}}}"
    );
    let archive = plugin_archive(&[
        ("pkg/plugin.json", manifest.as_bytes()),
        ("pkg/mcp.json", mcp_config.as_bytes()),
        (
            "pkg/skills/toolbox-notes/SKILL.md",
            b"---\nname: toolbox-notes\ndescription: Notes.\n---\nBody.\n",
        ),
    ]);

    let secrets = Arc::new(MemSecrets::default());
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let exec = Arc::new(
        crate::code_execution::ConfiguredCodeExecutionProvider::new(
            store.clone(),
            secrets,
            dir.path().join("scratch"),
        )
        .with_user_skills(Some(dir.path().join("skills")))
        .with_user_plugins(Some(dir.path().join("plugins")))
        .with_plugin_archive_fetcher(Arc::new(FixedPluginArchiveFetcher {
            expected_url: "https://example.com/toolbox-1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        })),
    );
    state.code_execution = Some(exec.clone());
    state
        .mcp
        .set_plugin_catalog(Arc::new(crate::plugin_mcp::InstalledPluginMcpCatalog::new(
            exec,
            store,
            dir.path().join("plugin-data"),
        )));
    let token = state.token.clone();
    let router = app(state);
    let bearer = format!("Bearer {token}");

    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/toolbox-1.0.0.zip",
                "revision": "1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "toolbox")
        .unwrap();
    assert_eq!(plugin["enabled"], false);
    assert!(get_mcp_servers(&router, &bearer).await["servers"]
        .as_array()
        .unwrap()
        .is_empty());

    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"toolbox": true}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let listed = get_mcp_servers(&router, &bearer).await;
    assert_eq!(listed["servers"].as_array().unwrap().len(), 1);
    assert_eq!(listed["servers"][0]["name"], "toolbox-local");
    assert_eq!(listed["servers"][0]["plugin"], "toolbox");
}

/// Contract: a top-level problem in `mcp.json` disables that plugin's MCP
/// configuration and nothing else — the skills still install, no server
/// configuration is retained, and the badge is not earned.
#[tokio::test]
async fn an_invalid_mcp_file_disables_mcp_without_sinking_the_plugin() {
    let (dir, store) = temp_db_store("plugin-mcp-invalid.db").await;
    let manifest = format!(
        "{{\"$schema\": \"{AGENT_PLUGIN_SCHEMA}\", \"name\": \"reporting\", \
          \"description\": \"Reporting skills.\"}}"
    );
    let archive = plugin_archive(&[
        ("pkg/plugin.json", manifest.as_bytes()),
        (
            "pkg/mcp.json",
            b"{\"$schema\": \"https://agent-plugins.org/schemas/9.9.9/mcp.schema.json\", \
              \"mcpServers\": {\"remote\": {\"type\": \"streamable-http\", \
              \"url\": \"https://mcp.example.com/v1\"}}}",
        ),
        (
            "pkg/skills/weekly-report/SKILL.md",
            b"---\nname: weekly-report\ndescription: Draft the weekly report.\n---\nBody.\n",
        ),
    ]);
    let (router, bearer) = plugin_install_app(
        Arc::new(store),
        dir.path(),
        FixedPluginArchiveFetcher {
            expected_url: "https://example.com/reporting-1.0.0.zip".to_owned(),
            archive: Arc::new(archive),
        },
    );
    let response = post_plugin_install(
        &router,
        &bearer,
        serde_json::json!({
            "source": {
                "kind": "archive",
                "url": "https://example.com/reporting-1.0.0.zip",
                "revision": "1.0.0"
            }
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let installed: serde_json::Value = json_body(response).await;
    assert_eq!(installed["skipped"][0]["path"], "mcp.json");
    assert!(!dir.path().join("plugins/reporting/mcp.json").exists());
    assert!(dir.path().join("skills/weekly-report/SKILL.md").is_file());

    let catalog = get_plugins(&router, &bearer).await;
    let plugin = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "reporting")
        .unwrap();
    assert!(!plugin["capabilities"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("mcp")));
}

/// Contract: the catalog reports host-derived badges and enable state,
/// toggles are a merge patch that survives a restart, and a disabled bundle
/// gates its members without erasing their own flags — so re-enabling it
/// restores the member choices that were in place.
#[tokio::test]
async fn the_plugin_catalog_reports_badges_and_persists_toggles() {
    let (dir, store) = temp_db_store("plugins.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let (router, token) = plugin_app(store.clone(), dir.path());
    let bearer = format!("Bearer {token}");

    let catalog = get_plugins(&router, &bearer).await;
    let documents = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "documents")
        .expect("the bundled document plugin is listed")
        .clone();
    assert_eq!(documents["enabled"], true);
    // Its members declare pinned Python deps and LibreOffice, so the badges
    // are derived rather than absent — no manifest states any of this.
    assert_eq!(
        documents["capabilities"],
        serde_json::json!(["write-files", "network", "host-install"])
    );

    // Turn the bundle off and one member off, in one merge patch.
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({
            "plugins": {"documents": false},
            "skills": {"pdf-documents": false}
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // A second server over the same store sees both, so the state is durable
    // rather than process-local.
    let (reloaded, token) = plugin_app(store.clone(), dir.path());
    let bearer = format!("Bearer {token}");
    let catalog = get_plugins(&reloaded, &bearer).await;
    let documents = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "documents")
        .unwrap()
        .clone();
    assert_eq!(documents["enabled"], false);
    let member = |name: &str| {
        documents["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["name"] == name)
            .unwrap()["enabled"]
            .clone()
    };
    // The bundle's gate did not rewrite the members: one is off because the
    // patch said so, the others are untouched.
    assert_eq!(member("pdf-documents"), serde_json::json!(false));
    assert_eq!(member("word-documents"), serde_json::json!(true));

    // Re-enabling the bundle brings back exactly those choices.
    let restored = put_plugins_enabled(
        &reloaded,
        &bearer,
        serde_json::json!({"plugins": {"documents": true}}),
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(restored).await;
    let documents = catalog["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["name"] == "documents")
        .unwrap()
        .clone();
    assert_eq!(documents["enabled"], true);
    assert_eq!(
        documents["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|skill| skill["name"] == "pdf-documents")
            .unwrap()["enabled"],
        false
    );

    // A name that is not a slug is refused rather than recorded.
    let refused = put_plugins_enabled(
        &reloaded,
        &bearer,
        serde_json::json!({"skills": {"Not A Slug": false}}),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
}

/// Contract: switching a bundle back on starts making it real. The host tools
/// its members declare are ensured through the broker on the same write that
/// moved the flag — including the Node runtime, which only becomes required
/// because the bundle brought a skill with npm pins back to life — so nothing
/// waits for the first turn that reaches for them. A write that switches
/// nothing on provisions nothing.
#[tokio::test(flavor = "multi_thread")]
async fn enabling_a_plugin_provisions_the_host_tools_its_skills_declare() {
    #[derive(Default)]
    struct RecordingBroker {
        ensured: std::sync::Mutex<Vec<tidebreak_code_execution::HostDep>>,
    }

    #[async_trait]
    impl tidebreak_code_execution::HostToolBroker for RecordingBroker {
        fn ensure(&self, tool: tidebreak_code_execution::HostDep) {
            self.ensured.lock().unwrap().push(tool);
        }

        async fn status(
            &self,
            _tool: tidebreak_code_execution::HostDep,
        ) -> tidebreak_code_execution::HostToolStatus {
            tidebreak_code_execution::HostToolStatus::Available
        }

        async fn managed_root(
            &self,
            _tool: tidebreak_code_execution::HostDep,
        ) -> Option<std::path::PathBuf> {
            None
        }
    }

    let (dir, store) = temp_db_store("plugin-provisioning.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    // No execution provider selected, so the pass stops at the host tools
    // instead of driving pip against a real package index from a unit test.
    store
        .set_setting(
            crate::code_execution::CODE_EXECUTION_SETTING,
            &serde_json::json!({"provider": null, "timeout_ms": 60_000}),
        )
        .await
        .unwrap();
    let mut flags = crate::plugin_state::read_plugin_enable_state(&*store).await;
    flags.set_plugin("documents", false);
    crate::plugin_state::write_plugin_enable_state(&*store, &flags)
        .await
        .unwrap();

    let broker = Arc::new(RecordingBroker::default());
    let (router, token) = plugin_app_with_broker(store.clone(), dir.path(), Some(broker.clone()));
    let bearer = format!("Bearer {token}");

    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"documents": true}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The pass is spawned rather than awaited, so the response can land first.
    let ensured = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ensured = broker.ensured.lock().unwrap().clone();
            if ensured.contains(&tidebreak_code_execution::HostDep::Node) {
                return ensured;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the enable write should provision the bundle's host tools");
    assert!(ensured.contains(&tidebreak_code_execution::HostDep::LibreOffice));

    // Re-asserting the same state switches nothing on, so nothing is
    // provisioned again.
    broker.ensured.lock().unwrap().clear();
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"documents": true}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(broker.ensured.lock().unwrap().is_empty());
}

/// Contract: a bundle the user wrote in their data directory reaches the
/// catalog attributed as theirs, the built-in tree keeps the floor when the
/// two collide, and a flag set against a user bundle outlives the bundle
/// itself — reinstalling one the user switched off must not quietly switch it
/// back on.
#[tokio::test]
async fn user_plugins_load_from_the_data_directory_and_keep_their_flags() {
    let (dir, store) = temp_db_store("user-plugins.db").await;
    let store: Arc<dyn Store> = Arc::new(store);

    let builtin = tempfile::tempdir().unwrap();
    let write = |root: &std::path::Path, kind: &str, name: &str, manifest: &str, source: String| {
        let package = root.join(kind).join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join(manifest), source).unwrap();
    };
    write(
        builtin.path(),
        "skills",
        "charts",
        "SKILL.md",
        "---\nname: charts\ndescription: Plots.\n---\nBody.\n".to_owned(),
    );
    write(
        builtin.path(),
        "plugins",
        "reporting",
        "PLUGIN.md",
        "---\nname: reporting\ndisplay-name: Reporting\ndescription: Status reporting.\n\
         category: other\nskills: [\"charts\"]\n---\n"
            .to_owned(),
    );
    // The user's own skill, and the bundle that groups it.
    write(
        dir.path(),
        "skills",
        "meeting-notes",
        "SKILL.md",
        "---\nname: meeting-notes\ndescription: How I take notes.\n---\nBody.\n".to_owned(),
    );
    let write_notes_bundle = || {
        write(
            dir.path(),
            "plugins",
            "notes",
            "PLUGIN.md",
            "---\nname: notes\ndisplay-name: My notes\ndescription: Notes the way I like them.\n\
             category: other\nskills: [\"meeting-notes\"]\n---\n"
                .to_owned(),
        );
    };
    write_notes_bundle();
    // Shadows a built-in name, and re-claims a skill a built-in bundle owns.
    // Both are skipped; neither takes the rest of the tree down with it.
    write(
        dir.path(),
        "plugins",
        "reporting",
        "PLUGIN.md",
        "---\nname: reporting\ndisplay-name: Mine\ndescription: Shadows the built-in.\n\
         category: other\nskills: [\"meeting-notes\"]\n---\n"
            .to_owned(),
    );
    write(
        dir.path(),
        "plugins",
        "poaching",
        "PLUGIN.md",
        "---\nname: poaching\ndisplay-name: Poaching\ndescription: Steals a member.\n\
         category: other\nskills: [\"charts\"]\n---\n"
            .to_owned(),
    );

    let user_plugin_app = || {
        let secrets = Arc::new(MemSecrets::default());
        let mut state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(Arc::new(FakeProvider))),
            secrets.clone(),
            Arc::new(ToolRegistry::new()),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        state.code_execution = Some(Arc::new(
            crate::code_execution::ConfiguredCodeExecutionProvider::new(
                store.clone(),
                secrets,
                dir.path().join("scratch"),
            )
            .with_skills(Some(builtin.path().join("skills")))
            .with_plugins(Some(builtin.path().join("plugins")))
            .with_user_skills(Some(dir.path().join("skills")))
            .with_user_plugins(Some(dir.path().join("plugins"))),
        ));
        let token = state.token.clone();
        (app(state), format!("Bearer {token}"))
    };
    let listed = |catalog: &serde_json::Value| {
        catalog["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|plugin| {
                (
                    plugin["name"].as_str().unwrap().to_owned(),
                    plugin["origin"].as_str().unwrap().to_owned(),
                    plugin["enabled"].as_bool().unwrap(),
                )
            })
            .collect::<Vec<_>>()
    };

    let (router, bearer) = user_plugin_app();
    let catalog = get_plugins(&router, &bearer).await;
    assert_eq!(
        listed(&catalog),
        [
            ("notes".to_owned(), "user".to_owned(), true),
            ("reporting".to_owned(), "builtin".to_owned(), true),
        ],
        "the user bundle is listed as theirs; neither collision shadows the built-in one"
    );
    // Its member is grouped under it rather than left standalone, and the
    // built-in bundle kept the skill the user tried to re-claim.
    let notes = &catalog["plugins"].as_array().unwrap()[0];
    assert_eq!(notes["skills"][0]["name"], "meeting-notes");
    assert!(catalog["skills"].as_array().unwrap().is_empty());

    // Switch the user bundle off, then uninstall it.
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"notes": false}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    std::fs::remove_dir_all(dir.path().join("plugins/notes")).unwrap();

    let (router, bearer) = user_plugin_app();
    let catalog = get_plugins(&router, &bearer).await;
    assert_eq!(
        listed(&catalog),
        [("reporting".to_owned(), "builtin".to_owned(), true)],
        "an uninstalled user bundle is gone from the catalog"
    );
    // The skill it used to claim is standalone again rather than lost.
    assert_eq!(catalog["skills"][0]["name"], "meeting-notes");

    // Reinstalling it does not silently re-enable it: the flag was recorded
    // against the name, not against the package that happened to be present.
    write_notes_bundle();
    let (router, bearer) = user_plugin_app();
    let catalog = get_plugins(&router, &bearer).await;
    assert_eq!(
        listed(&catalog),
        [
            ("notes".to_owned(), "user".to_owned(), false),
            ("reporting".to_owned(), "builtin".to_owned(), true),
        ]
    );
}

/// Contract: reusable prompts reach the client as one flat, attributed list
/// whose entries a bundle's flag gates, and their bodies come from their own
/// route. Both halves are wire shape a composer depends on, and the gating is
/// the only behavior a prompt has — a bundled prompt surviving its bundle
/// being switched off would be invisible until someone read the catalog JSON.
#[tokio::test]
async fn the_catalog_lists_prompts_and_serves_their_bodies() {
    let (dir, store) = temp_db_store("prompts.db").await;
    let store: Arc<dyn Store> = Arc::new(store);

    let trees = tempfile::tempdir().unwrap();
    let write = |kind: &str, name: &str, source: String| {
        let package = trees.path().join(kind).join(name);
        std::fs::create_dir_all(&package).unwrap();
        let manifest = match kind {
            "skills" => "SKILL.md",
            "prompts" => "PROMPT.md",
            _ => "PLUGIN.md",
        };
        std::fs::write(package.join(manifest), source).unwrap();
    };
    write(
        "skills",
        "charts",
        "---\nname: charts\ndescription: Plots.\n---\nBody.\n".to_owned(),
    );
    write(
        "prompts",
        "weekly-update",
        "---\nname: weekly-update\ndescription: Draft this week's update.\n---\n\
         Cover what shipped, what slipped, and what is next.\n"
            .to_owned(),
    );
    write(
        "plugins",
        "reporting",
        "---\nname: reporting\ndisplay-name: Reporting\ndescription: Status reporting.\n\
         category: other\nskills: [\"charts\"]\nprompts: [\"weekly-update\"]\n---\n"
            .to_owned(),
    );
    // A user-authored prompt, from the per-install directory rather than the
    // bundled tree, claimed by no plugin.
    std::fs::create_dir_all(dir.path().join("prompts/standup")).unwrap();
    std::fs::write(
        dir.path().join("prompts/standup/PROMPT.md"),
        "---\nname: standup\ndescription: My standup format.\n---\nYesterday, today, blockers.\n",
    )
    .unwrap();

    let secrets = Arc::new(MemSecrets::default());
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        secrets.clone(),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.code_execution = Some(Arc::new(
        crate::code_execution::ConfiguredCodeExecutionProvider::new(
            store,
            secrets,
            dir.path().join("scratch"),
        )
        .with_skills(Some(trees.path().join("skills")))
        .with_prompts(Some(trees.path().join("prompts")))
        .with_plugins(Some(trees.path().join("plugins")))
        .with_user_prompts(Some(dir.path().join("prompts"))),
    ));
    let token = state.token.clone();
    let router = app(state);
    let bearer = format!("Bearer {token}");

    let prompt = |catalog: &serde_json::Value, name: &str| {
        catalog["prompts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|prompt| prompt["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"))
            .clone()
    };
    let catalog = get_plugins(&router, &bearer).await;
    let bundled = prompt(&catalog, "weekly-update");
    assert_eq!(bundled["plugin"], "reporting");
    assert_eq!(bundled["origin"], "builtin");
    assert_eq!(bundled["enabled"], true);
    let standalone = prompt(&catalog, "standup");
    assert_eq!(standalone["plugin"], serde_json::Value::Null);
    assert_eq!(standalone["origin"], "user");
    assert_eq!(standalone["enabled"], true);

    // The bundle's flag is the only thing that gates a prompt.
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"reporting": false}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let catalog: serde_json::Value = json_body(response).await;
    assert_eq!(prompt(&catalog, "weekly-update")["enabled"], false);
    assert_eq!(prompt(&catalog, "standup")["enabled"], true);

    let body = |name: &str| {
        let router = router.clone();
        let bearer = bearer.clone();
        let uri = format!("/plugins/prompts/{name}/body");
        async move {
            router
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header(header::AUTHORIZATION, bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }
    };
    let response = body("standup").await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched: serde_json::Value = json_body(response).await;
    assert_eq!(fetched["body"], "Yesterday, today, blockers.");
    assert_eq!(body("does-not-exist").await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body("Not%20A%20Slug").await.status(),
        StatusCode::BAD_REQUEST
    );
}

/// POST a message that explicitly invokes skills by name, returning the raw
/// response so a refusal's typed kind can be read.
async fn send_message_invoking(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    invoked: &[&str],
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/messages"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "turn_id": TurnId::new(),
                        "content": "build the deck",
                        "invoked_skills": invoked,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Contract: a turn may name the skills it must use, and the names are checked
/// against the catalog the turn will actually stage. A skill the install has
/// switched off is not a live capability, so invoking it refuses the whole
/// submission rather than sending a turn told to read a manifest that staging
/// has already removed.
#[tokio::test]
async fn an_invoked_skill_must_be_enabled_or_the_turn_is_refused() {
    let (dir, store) = temp_db_store("invoked-skills.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let (router, token) = plugin_app(store.clone(), dir.path());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    let accepted = send_message_invoking(&router, &bearer, chat.id, &["presentations"]).await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let turns = store.list_turn_runs(chat.id).await.unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.invoked_skills.clone())
            .collect::<Vec<_>>(),
        [vec!["presentations".to_owned()]],
        "the invocation is captured with the turn, not just used to build a prompt"
    );
    let messages = store.list_messages(chat.id).await.unwrap();
    assert_eq!(messages[0].content, "build the deck");
    assert!(messages[0]
        .llm_content
        .as_deref()
        .is_some_and(|content| content.contains("presentations")));

    // Switching the skill off makes the same submission a refusal.
    let toggled = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"skills": {"presentations": false}}),
    )
    .await;
    assert_eq!(toggled.status(), StatusCode::OK);

    let refused = send_message_invoking(&router, &bearer, chat.id, &["presentations"]).await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(refused).await;
    assert_eq!(error.kind, "invoked_skill_unavailable");

    let unknown = send_message_invoking(&router, &bearer, chat.id, &["no-such-skill"]).await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(unknown).await;
    assert_eq!(error.kind, "invoked_skill_unavailable");

    // Refused, not partially honoured: one bad name in a list that is otherwise
    // live takes the whole turn down, and neither submission was recorded.
    let mixed =
        send_message_invoking(&router, &bearer, chat.id, &["charts", "no-such-skill"]).await;
    assert_eq!(mixed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(store.list_turn_runs(chat.id).await.unwrap().len(), 1);
}

/// POST guidance into a running turn that explicitly invokes skills by name.
async fn steer_invoking(
    router: &Router,
    bearer: &str,
    chat: ChatId,
    turn: TurnId,
    steer_id: TurnSteerId,
    invoked: &[&str],
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/chats/{chat}/steer"))
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "steer_id": steer_id,
                        "turn_id": turn,
                        "content": "use the deck skill instead",
                        "invoked_skills": invoked,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Contract: guidance sent into a running turn may name skills of its own, and
/// they are held to the same catalog check the opening message is held to.
///
/// A steer becomes a real user message, so the names it carries end up in that
/// message's model projection exactly as the turn's own do. Checking them here
/// is what stops a name the reader picked moments earlier — and the install has
/// since switched off — from directing the model at a manifest staging has
/// already removed.
#[tokio::test]
async fn steering_guidance_may_invoke_skills_and_is_held_to_the_same_catalog() {
    let (dir, store) = temp_db_store("steer-invoked-skills.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let (router, token) = plugin_app(store.clone(), dir.path());
    let bearer = format!("Bearer {token}");
    let chat = make_chat(&router, &bearer).await;

    assert_eq!(
        send_message_invoking(&router, &bearer, chat.id, &[])
            .await
            .status(),
        StatusCode::ACCEPTED
    );
    let turn = store.list_turn_runs(chat.id).await.unwrap()[0].id;

    let steer_id = TurnSteerId::new();
    let accepted = steer_invoking(
        &router,
        &bearer,
        chat.id,
        turn,
        steer_id,
        &["presentations"],
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    // The list is durably part of what was accepted, not just prompt material:
    // the same identity repeated with the same skills is the same instruction,
    // and repeated with different ones is a different one.
    assert_eq!(
        steer_invoking(
            &router,
            &bearer,
            chat.id,
            turn,
            steer_id,
            &["presentations"]
        )
        .await
        .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        steer_invoking(&router, &bearer, chat.id, turn, steer_id, &["charts"])
            .await
            .status(),
        StatusCode::CONFLICT
    );

    let unknown = steer_invoking(
        &router,
        &bearer,
        chat.id,
        turn,
        TurnSteerId::new(),
        &["no-such-skill"],
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(unknown).await;
    assert_eq!(error.kind, "invoked_skill_unavailable");

    let toggled = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"skills": {"presentations": false}}),
    )
    .await;
    assert_eq!(toggled.status(), StatusCode::OK);
    let refused = steer_invoking(
        &router,
        &bearer,
        chat.id,
        turn,
        TurnSteerId::new(),
        &["presentations"],
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let error: AgentErrorInfo = json_body(refused).await;
    assert_eq!(error.kind, "invoked_skill_unavailable");
}

/// A stdio MCP server small enough to write into a plugin package: POSIX sh
/// answering the three requests a connection makes, plus the health probe. It
/// records the environment it was launched with on startup, which is what
/// lets the test assert the two reserved variables actually reached the child.
#[cfg(unix)]
const FAKE_PLUGIN_MCP_SERVER: &str = r#"#!/bin/sh
printf '%s|%s|%s|%s' "$PLUGIN_ROOT" "$PLUGIN_DATA" "$MODE" "$(pwd)" > "$PLUGIN_DATA/started"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"PROTOCOL","capabilities":{"tools":{}},"serverInfo":{"name":"toolbox","version":"1"}}}\n' "$id"
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"lookup","description":"Look something up.","inputSchema":{"type":"object"}}]}}\n' "$id"
      ;;
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      ;;
  esac
done
"#;

/// Contract: a plugin's bundled MCP servers are live tools exactly while the
/// plugin is installed and enabled, and are managed only from there.
///
/// This is the whole lifecycle in one turn of the crank, because each half is
/// only meaningful against the others: mounting without unmounting leaves a
/// disabled plugin's subprocess serving tools, and a `PUT /mcp/servers` that
/// could edit or drop a derived server would make the plugin's own switch a
/// lie. The launch assertions cover what the Agent Plugins specification
/// makes normative for a subprocess-launching client — `PLUGIN_ROOT` and
/// `PLUGIN_DATA` set, the data directory created before launch, expansion in
/// `env` values — by reading what the child itself observed.
#[cfg(unix)]
#[tokio::test]
async fn a_plugins_bundled_mcp_server_mounts_only_while_the_plugin_is_enabled() {
    use std::os::unix::fs::PermissionsExt as _;

    let (dir, store) = temp_db_store("plugin-mcp.db").await;
    let store: Arc<dyn Store> = Arc::new(store);

    // A bundle must claim at least one member, so it ships a skill too.
    let skill = dir.path().join("skills/toolbox-notes");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: toolbox-notes\ndescription: Notes.\n---\nBody.\n",
    )
    .unwrap();

    let plugin = dir.path().join("plugins/toolbox");
    let write_plugin = || {
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("PLUGIN.md"),
            "---\nname: toolbox\ndisplay-name: Toolbox\ndescription: Bundled tools.\n\
             category: other\nskills: [\"toolbox-notes\"]\n---\n",
        )
        .unwrap();
        // Exactly what the importer retains beside the manifest: the canonical
        // form of a validated `mcp.json`.
        std::fs::write(
            plugin.join("mcp.json"),
            format!(
                "{{\"$schema\": \"{}\", \"mcpServers\": {{\"local.serve\": {{\
                   \"type\": \"stdio\", \"command\": \"./serve\", \
                   \"cwd\": \"${{PLUGIN_DATA}}/state\", \
                   \"env\": {{\"MODE\": \"${{PLUGIN_DATA}}/state\"}}}}}}}}",
                tidebreak_code_execution::AGENT_PLUGIN_MCP_SCHEMA_ID
            ),
        )
        .unwrap();
        let script = plugin.join("serve");
        std::fs::write(
            &script,
            FAKE_PLUGIN_MCP_SERVER.replace("PROTOCOL", tidebreak_mcp::PROTOCOL_VERSION),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write_plugin();

    let build = || {
        let secrets = Arc::new(MemSecrets::default());
        let mut state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(Arc::new(FakeProvider))),
            secrets.clone(),
            Arc::new(ToolRegistry::new()),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let exec = Arc::new(
            crate::code_execution::ConfiguredCodeExecutionProvider::new(
                store.clone(),
                secrets,
                dir.path().join("scratch"),
            )
            .with_user_skills(Some(dir.path().join("skills")))
            .with_user_plugins(Some(dir.path().join("plugins"))),
        );
        state.code_execution = Some(exec.clone());
        state
            .mcp
            .set_plugin_catalog(Arc::new(crate::plugin_mcp::InstalledPluginMcpCatalog::new(
                exec,
                store.clone(),
                dir.path().join("plugin-data"),
            )));
        let mcp = state.mcp.clone();
        let token = state.token.clone();
        (app(state), format!("Bearer {token}"), mcp)
    };

    let (router, bearer, mcp) = build();
    // Startup reconcile, as `serve` runs it after `initialize`.
    mcp.reconcile_plugin_servers().await;

    // The server key is `local.serve` and the plugin is `toolbox`; the runtime
    // namespace folds the `.` the mount grammar has no room for.
    let mounted = "mcp__toolbox-local-serve__lookup";
    assert!(
        mcp.snapshot().get(mounted).is_some(),
        "an enabled plugin's bundled server mounts its tools"
    );

    // What the child observed: both reserved variables, the configured `env`
    // value expanded against the data directory, and the working directory it
    // was actually started in. That last one is the whole anchored-`cwd`
    // contract in one reading — the directory is under the data tree the
    // configuration named, and the client created it, because a plugin is
    // handed that tree empty and cannot be expected to have made it first.
    // It also pins that `./serve` resolved against the *package root*: with a
    // working directory pointing somewhere else entirely, a command left for
    // the child to resolve would not have started at all.
    let data = dir.path().join("plugin-data/toolbox");
    let observed = std::fs::read_to_string(data.join("started")).unwrap();
    let root = std::fs::canonicalize(&plugin).unwrap();
    let data_path = std::fs::canonicalize(&data).unwrap();
    let state = std::fs::canonicalize(data.join("state")).unwrap();
    assert_eq!(
        observed,
        format!(
            "{}|{}|{}/state|{}",
            root.display(),
            data_path.display(),
            data_path.display(),
            state.display()
        )
    );

    // The settings surface lists it read-only, attributed to its plugin.
    let listed = get_mcp_servers(&router, &bearer).await;
    assert_eq!(listed["servers"].as_array().unwrap().len(), 1);
    assert_eq!(listed["servers"][0]["name"], "toolbox-local-serve");
    assert_eq!(listed["servers"][0]["plugin"], "toolbox");
    assert_eq!(listed["servers"][0]["health"], "healthy");

    // A body that names it is refused rather than quietly ignored, and a body
    // that omits it does not delete it — the derived slice is rebuilt either
    // way.
    let refused = put_mcp_servers(
        &router,
        &bearer,
        serde_json::json!({"servers": [{
            "name": "toolbox-local-serve", "command": "/bin/false", "plugin": "toolbox"
        }]}),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    let response = put_mcp_servers(&router, &bearer, serde_json::json!({"servers": []})).await;
    assert_eq!(response.status(), StatusCode::OK);
    let after: serde_json::Value = json_body(response).await;
    assert_eq!(after["servers"][0]["plugin"], "toolbox");
    assert!(
        mcp.snapshot().get(mounted).is_some(),
        "omitting a derived server from a settings save must not unmount it"
    );

    // Switching the plugin off disconnects its server and unmounts its tools.
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"toolbox": false}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(mcp.snapshot().get(mounted).is_none());
    assert!(get_mcp_servers(&router, &bearer).await["servers"]
        .as_array()
        .unwrap()
        .is_empty());
    // Its data survives the switch: it is off, not gone.
    assert!(data.is_dir());

    // Uninstalling — removing the package — takes the data directory with it.
    let response = put_plugins_enabled(
        &router,
        &bearer,
        serde_json::json!({"plugins": {"toolbox": true}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    std::fs::remove_dir_all(&plugin).unwrap();
    let (_router, _bearer, mcp) = build();
    mcp.reconcile_plugin_servers().await;
    assert!(mcp.snapshot().get(mounted).is_none());
    assert!(
        !data.exists(),
        "uninstalling a plugin deletes the writable directory it was given"
    );
}
