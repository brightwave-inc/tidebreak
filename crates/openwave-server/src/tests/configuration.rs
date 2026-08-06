use super::*;

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
    assert_eq!(settings["max_active_background_agents"], 5);

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
                        "max_active_background_agents": 7
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
async fn failed_mcp_candidate_does_not_replace_the_active_configuration() {
    const MISSING: &str = "OPENWAVE_TEST_MCP_ROUTE_MISSING_ENV_65B7A2";
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

    let failed = put_mcp_servers(
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
    if openwave_code_execution::LocalExecutionProvider::availability().is_ok() {
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
        ["local", "e2b", "daytona"]
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
            .get_secret(openwave_code_execution::E2B_CREDENTIAL_KEY)
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
            .get_secret(openwave_code_execution::DAYTONA_CREDENTIAL_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some(daytona_key)
    );
    assert_eq!(
        secrets
            .get_secret(openwave_code_execution::E2B_CREDENTIAL_KEY)
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

#[tokio::test]
async fn providers_never_echo_an_unsupported_service_account() {
    let (router, token, _store, _dir) = test_app().await;
    let secret = "test-private-key-material";
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/openai")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "credential": {
                            "type": "service_account",
                            "json": secret,
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = json_body(response).await;
    assert_eq!(body["kind"], "bad_request");
    assert!(!body.to_string().contains(secret));
}

#[tokio::test]
async fn gemini_vertex_location_is_validated_and_public_info_never_grows_a_project_field() {
    let (router, token, _store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/gemini")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "enabled": true,
                        "vertex_location": "us-central1",
                        "credential": {"type": "api_key", "key": "gemini-secret"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let body: serde_json::Value = json_body(put).await;
    assert_eq!(body["vertex_location"], "us-central1");
    assert_eq!(body["has_credential"], true);
    assert!(body.get("credential").is_none());
    assert!(body.get("project_id").is_none());
    assert!(!body.to_string().contains("gemini-secret"));

    let rejected = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/providers/gemini")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"vertex_location": "../global"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
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
            vertex_location: None,
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
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
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
                        "base_url": "http://127.0.0.1:1234/v1",
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
/// profile flips to managed, both roles' reads re-route to entitled gateway
/// models rather than reporting selections no turn could serve — and a message
/// sent against the stale default runs on the re-routed model.
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
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
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

    // A managed flip with a BYOK chat pin carried in from before it: the pin
    // stays stored — a profile returned to the open experience gets it back —
    // but both roles' effective resolutions move to entitled gateway models.
    // Chat lands on the first model the gateway lists (the row the composer
    // picker offers first); utility keeps #901's cheapest-first walk.
    configure_provider("openai", serde_json::json!({"enabled": true})).await;
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

    crate::managed_policy::provision(&*store, "https://corp.gateway")
        .await
        .unwrap();
    let credentials: openwave_connectors::GatewayCredentials =
        serde_json::from_value(serde_json::json!({
            "base_url": "https://corp.gateway/",
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_seed",
            "access_tokens": {}
        }))
        .unwrap();
    openwave_connectors::CredentialVault::new(secrets)
        .save(&credentials)
        .await
        .unwrap();
    providers::write_gateway_snapshot(
        &*store,
        &providers::GatewayModelSnapshot {
            gateway_url: "https://corp.gateway/".to_string(),
            // Flagship-first, as a gateway well might list them: chat takes
            // the gateway's first pick, utility must not.
            models: vec![
                providers::CustomModelConfig {
                    id: "gateway-flagship".to_string(),
                    upstream_id: None,
                    display_name: Some("Gateway Flagship".to_string()),
                    context_window: 1_000_000,
                    max_output_tokens: 64_000,
                },
                providers::CustomModelConfig {
                    id: "gateway-haiku".to_string(),
                    upstream_id: None,
                    display_name: Some("Gateway Haiku".to_string()),
                    context_window: 200_000,
                    max_output_tokens: 8_192,
                },
            ],
        },
    )
    .await
    .unwrap();

    let roles = role_rows().await;
    let chat = role_row(&roles, "chat");
    assert_eq!(chat["selection"], "openai::gpt-5.6-sol");
    assert_eq!(chat["resolved_key"], "model_gateway::gateway-flagship");
    assert_eq!(
        role_row(&roles, "utility")["resolved_key"],
        "model_gateway::gateway-haiku"
    );

    // The label is what the turn gets: a message sent against that stale BYOK
    // default is accepted and frozen on the gateway model the roles read
    // named, not refused with `model_provider_unavailable`. This is the
    // field-reported failure — remove the re-route from the accept path and
    // this send 409s.
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
    let turn_id = TurnId::new();
    assert_eq!(
        send_message_with_id(&router, &bearer, chat.id, turn_id, "hello").await,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        store.get_turn_run(turn_id).await.unwrap().unwrap().model,
        "model_gateway::gateway-flagship"
    );

    // The re-route covers exactly what the roles read labels: the default. A
    // per-chat override is the user's explicit pick, and its pill still names
    // it — a dead one is refused honestly rather than silently swapped, and
    // the picker offers only gateway models to fix it.
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
                        "base_url": "http://127.0.0.1:1234/v1",
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
            vertex_location: None,
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
            Arc::new(crate::managed_policy::NoOsPolicy),
        ),
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
            vertex_location: None,
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
            openwave_router::RouteKind::Openai,
        ),
        (
            providers::ProviderKind::Gemini,
            "gemini-3.6-flash",
            openwave_router::RouteKind::Gemini,
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
                vertex_location: None,
                models: Vec::new(),
            },
        )
        .await
        .unwrap();

        let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
            .await
            .unwrap();
        let routes = providers::collect_routes(&*store, &*secrets, None, &policy).await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, route_kind);

        let resolver = resolver::KeyedResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(
                store.clone(),
                secrets.clone(),
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
            Arc::new(crate::managed_policy::NoOsPolicy),
        );
        let provider = resolver.resolve().await;
        assert_eq!(provider.id().0, "router");

        // Curated models stay on their explicitly configured native route;
        // the absence of a compatibility fallback must not cross providers.
        let router = openwave_router::Router::build(routes);
        assert_eq!(router.select(model), Some(route_kind));
        assert_eq!(router.select("claude-opus-4-8"), None);
    }
}

#[tokio::test]
async fn malformed_gemini_service_account_never_advertises_or_builds_a_route() {
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
        providers::ProviderKind::Gemini,
        &providers::ProviderCredential::ServiceAccount {
            json: r#"{"type":"service_account","client_email":"service@example.test","private_key":"not-a-private-key","project_id":"test-project"}"#.into(),
        },
    )
    .await
    .unwrap();
    providers::write_config(
        &*store,
        providers::ProviderKind::Gemini,
        &providers::ProviderConfig {
            enabled: true,
            base_url: None,
            vertex_location: Some("global".into()),
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
        .await
        .unwrap();
    assert!(!providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::Gemini,
        &policy
    )
    .await
    .unwrap());
    assert!(providers::collect_routes(&*store, &*secrets, None, &policy)
        .await
        .is_empty());
    assert!(providers::catalog_models(&*store, &*secrets, &policy)
        .await
        .unwrap()
        .into_iter()
        .filter(|model| model.policy.provider == providers::ProviderKind::Gemini)
        .all(|model| !model.available));
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
            base_url: Some("http://127.0.0.1:1234/v1".into()),
            vertex_location: None,
            models: Vec::new(),
        },
    )
    .await
    .unwrap();

    let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
        .await
        .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, None, &policy).await;
    let router = openwave_router::Router::build(routes);
    assert_eq!(
        router.select("llama-3-local"),
        Some(openwave_router::RouteKind::OpenaiCompatible)
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
    impl openwave_router::BearerTokenSource for StaticTokens {
        async fn bearer_token(&self) -> openwave_core::Result<String> {
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
    let tokens: Arc<dyn openwave_router::BearerTokenSource> = Arc::new(StaticTokens);

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
                vertex_location: None,
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
    let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
        .await
        .unwrap();
    let kinds: Vec<_> =
        providers::collect_routes(&*store, &*secrets, Some(tokens.clone()), &policy)
            .await
            .into_iter()
            .map(|route| route.kind)
            .collect();
    assert!(kinds.contains(&openwave_router::RouteKind::Anthropic));
    assert!(kinds.contains(&openwave_router::RouteKind::Gemini));
    assert!(!kinds.contains(&openwave_router::RouteKind::ModelGateway));

    // Provisioned: only the gateway remains, aimed at the policy URL. The
    // stale stored row and every BYOK credential — stored or env — are inert.
    crate::managed_policy::provision(&*store, "https://corp.gateway")
        .await
        .unwrap();
    let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
        .await
        .unwrap();
    let routes = providers::collect_routes(&*store, &*secrets, Some(tokens.clone()), &policy).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, openwave_router::RouteKind::ModelGateway);
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
    let routes = providers::collect_routes(&*store, &*secrets, Some(tokens), &policy).await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].kind, openwave_router::RouteKind::ModelGateway);
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
            vertex_location: None,
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
            Arc::new(crate::managed_policy::NoOsPolicy),
        ),
        Arc::new(crate::managed_policy::NoOsPolicy),
    );
    assert_eq!(resolver.resolve().await.id().0, "router");

    // Corrupt the persisted policy record (the key is a persisted contract).
    store
        .set_setting("managed_policy_v1", &serde_json::json!({"gateway_url": 42}))
        .await
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
            vertex_location: None,
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
                Arc::new(crate::managed_policy::NoOsPolicy),
            ),
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
    store
        .set_setting("managed_policy_v1", &serde_json::json!({"gateway_url": 42}))
        .await
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

async fn put_json(
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
    let (router, token, store, _dir) = test_app().await;
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

    crate::managed_policy::provision(&*store, "https://corp.gateway")
        .await
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
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let legacy = serde_json::json!({
        "name": "legacy_docs",
        "command": "/not/started/while/disabled",
        "enabled": false
    });

    // Configured before the profile was managed.
    let saved = put_mcp_servers(&router, &bearer, serde_json::json!({"servers": [legacy]})).await;
    assert_eq!(saved.status(), StatusCode::OK);
    crate::managed_policy::provision(&*store, "https://corp.gateway")
        .await
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
            put_mcp_servers(&router, &bearer, serde_json::json!({"servers": candidate})).await;
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
        let credentials: openwave_connectors::GatewayCredentials =
            serde_json::from_value(serde_json::json!({
                "base_url": base_url,
                "installation_id": "install-1",
                "user_id": "user-1",
                "refresh_token": "mg_rt_seed",
                "access_tokens": {}
            }))
            .unwrap();
        openwave_connectors::CredentialVault::new(secrets)
            .save(&credentials)
            .await
            .unwrap();
    };
    seed(secrets.clone(), "https://old.gateway").await;
    crate::managed_policy::provision(&*store, "https://corp.gateway")
        .await
        .unwrap();
    let policy = crate::managed_policy::resolve(&*store, &crate::managed_policy::NoOsPolicy)
        .await
        .unwrap();

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

    // A session for the policy's own deployment — trailing slash included,
    // per the shared normalization — reads usable again.
    seed(secrets.clone(), "https://corp.gateway/").await;
    assert!(providers::provider_is_usable(
        &*store,
        &*secrets,
        providers::ProviderKind::ModelGateway,
        &policy
    )
    .await
    .unwrap());
}

/// A router whose state carries a code-execution provider loaded from the
/// repository's own bundled skills and plugins, over `store`.
///
/// The catalog routes are only interesting against a real load: what they
/// project is exactly what staging and prompt composition read.
fn plugin_app(store: Arc<dyn Store>, data_dir: &std::path::Path) -> (Router, Arc<str>) {
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
        .with_plugins(Some(root.join("plugins"))),
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
            b"---\nname: deploy-reports\ndescription: Publish a generated report.\ndeps: { python: [\"not-in-openwave-image==1.2.3\"] }\n---\nRun the helper.\n",
        ),
        (
            "deploy-skill/scripts/deploy.py",
            b"print('requires external deployment tooling')\n",
        ),
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
        installed["compatibility"]["issues"],
        serde_json::json!([
            {
                "kind": "missing_sandbox_dependency",
                "skill": "deploy-reports",
                "dependency": "not-in-openwave-image==1.2.3"
            },
            {"kind": "scripts_present", "skill": "deploy-reports"}
        ])
    );

    let stamp: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            dir.path()
                .join("plugins/deploy-reports/.openwave-install.json"),
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
