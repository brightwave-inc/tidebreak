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
                    serde_json::json!({"model": "claude-x"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: serde_json::Value = json_body(response).await;
    assert_eq!(settings["model"], "claude-x");

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
                "env": {"LOG_LEVEL": "info"},
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
                {"provider": "tavily", "has_credential": false}
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
    assert_eq!(initial["provider"], "local");
    assert_eq!(
        initial["timeout_ms"],
        crate::code_execution::DEFAULT_TIMEOUT_MS
    );
    assert!(initial["available"].is_boolean());
    assert_eq!(initial["has_credential"], false);

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
    // enforcement status: E2B confirmed, Daytona pending live confirmation.
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
    // E2B is applied but honestly not a full boundary; Daytona is unconfirmed.
    // Neither is ever shown as a plain boundary, and the gaps are surfaced.
    assert_eq!(row("e2b")["status"], "applied_with_gaps");
    assert!(!row("e2b")["gaps"].as_array().unwrap().is_empty());
    assert_eq!(
        row("daytona")["status"],
        "unconfirmed",
        "Daytona egress is applied but not yet a confirmed boundary"
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
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(resolver::ConfiguredResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(store.clone(), secrets.clone()),
        )),
        secrets,
        Arc::new(ToolRegistry::new()),
        retrieval,
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
/// them, and enabling a provider changes the answer without a restart.
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
    let (retrieval, _search) = build_retrieval(
        Arc::new(HashEmbedder::default()),
        Arc::new(InMemoryVectorStore::new(HashEmbedder::DEFAULT_DIMS)),
    );
    let state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(resolver::ConfiguredResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(store.clone(), secrets.clone()),
        )),
        secrets,
        Arc::new(ToolRegistry::new()),
        retrieval,
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
        crate::gateway_runtime::GatewayRuntime::new(store.clone(), secrets.clone()),
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

        let routes = providers::collect_routes(&*store, &*secrets, None).await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, route_kind);

        let resolver = resolver::KeyedResolver::new(
            store.clone(),
            secrets.clone(),
            crate::gateway_runtime::GatewayRuntime::new(store.clone(), secrets.clone()),
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

    assert!(
        !providers::provider_is_usable(&*store, &*secrets, providers::ProviderKind::Gemini,)
            .await
            .unwrap()
    );
    assert!(providers::collect_routes(&*store, &*secrets, None)
        .await
        .is_empty());
    assert!(providers::catalog_models(&*store, &*secrets)
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

    let routes = providers::collect_routes(&*store, &*secrets, None).await;
    let router = openwave_router::Router::build(routes);
    assert_eq!(
        router.select("llama-3-local"),
        Some(openwave_router::RouteKind::OpenaiCompatible)
    );
}
