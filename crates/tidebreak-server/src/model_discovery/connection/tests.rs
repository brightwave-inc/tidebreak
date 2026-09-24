use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use tidebreak_core::DbStore;

use super::*;
use crate::providers::{ProviderConfig, ProviderUpdate};

/// A stand-in credential, built at run time so no literal in this file reads
/// like a real key.
fn stand_in_key(label: &str) -> String {
    ["stand-in", label, "credential"].join("-")
}

/// Each request a stand-in saw: its path with query, and its headers.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(String, HeaderMap)>>>);

impl Seen {
    fn record(&self, path: String, headers: &HeaderMap) {
        self.0.lock().unwrap().push((path, headers.clone()));
    }

    fn requests(&self) -> Vec<(String, HeaderMap)> {
        self.0.lock().unwrap().clone()
    }
}

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("http://{address}")
}

/// A stand-in that answers every request with `status` and `body`.
async fn answering(status: StatusCode, body: &'static str) -> String {
    serve(Router::new().fallback(move || async move { (status, body) })).await
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[tokio::test]
async fn a_local_server_that_lists_models_is_connected_without_a_key() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/v1/models",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/v1/models".into(), &headers);
                Json(serde_json::json!({
                    "object": "list",
                    "data": [
                        { "id": "qwen3-8b", "object": "model" },
                        { "id": "llama-4-scout", "object": "model" }
                    ]
                }))
            }),
        )
        .with_state(seen.clone());
    let base = format!("{}/v1", serve(router).await);

    let result = test_at(ProviderKind::OpenaiCompatible, &base, None).await;

    assert_eq!(result.outcome, ProviderTestOutcome::Connected);
    assert_eq!(result.model_count, Some(2));
    assert_eq!(result.status, None);
    assert_eq!(
        result.message,
        "Tidebreak reached the server. It lists 2 models."
    );
    let requests = seen.requests();
    assert_eq!(requests.len(), 1, "one cheap request");
    assert_eq!(header(&requests[0].1, "authorization"), None);
}

#[tokio::test]
async fn a_rejected_key_is_named_without_repeating_it() {
    let key = stand_in_key("rejected");
    let echoed = key.clone();
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/v1/models",
            get(
                move |State(seen): State<Seen>,
                      Query(query): Query<HashMap<String, String>>,
                      headers: HeaderMap| {
                    let echoed = echoed.clone();
                    async move {
                        seen.record(format!("limit={:?}", query.get("limit")), &headers);
                        (
                            StatusCode::UNAUTHORIZED,
                            format!(
                                "{{\"type\":\"error\",\"error\":{{\"type\":\"authentication_error\",\"message\":\"invalid x-api-key {echoed}\"}}}}"
                            ),
                        )
                    }
                },
            ),
        )
        .with_state(seen.clone());
    let base = serve(router).await;

    let result = test_at(ProviderKind::Anthropic, &base, Some(&key)).await;

    assert_eq!(result.outcome, ProviderTestOutcome::KeyRejected);
    assert_eq!(result.status, Some(401));
    assert!(
        result
            .message
            .starts_with("Anthropic rejected the saved API key (HTTP 401)"),
        "{}",
        result.message
    );
    assert!(!result.message.contains(&key));
    let requests = seen.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "limit=Some(\"1\")", "the smallest page");
    assert_eq!(header(&requests[0].1, "x-api-key"), Some(key.as_str()));
    assert_eq!(
        header(&requests[0].1, "anthropic-version"),
        Some("2023-06-01")
    );
}

#[tokio::test]
async fn a_keyless_server_that_wants_a_key_says_so() {
    let base = answering(StatusCode::UNAUTHORIZED, "{}").await;

    let result = test_at(ProviderKind::OpenaiCompatible, &base, None).await;

    assert_eq!(result.outcome, ProviderTestOutcome::KeyRejected);
    assert_eq!(
        result.message,
        "The server asked for an API key (HTTP 401). Save one, then test again."
    );
}

/// Gemini and xAI answer a bad key with a 400. The body tells it apart from
/// any other bad request; the body itself never reaches the result.
#[tokio::test]
async fn providers_that_answer_a_bad_key_with_400_are_read_as_rejected() {
    let gemini = answering(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.rpc.ErrorInfo","reason":"API_KEY_INVALID","domain":"googleapis.com"}]}}"#,
    )
    .await;
    let result = test_at(ProviderKind::Gemini, &gemini, Some("gemini-key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::KeyRejected);
    assert_eq!(result.status, Some(400));

    let xai = answering(
        StatusCode::BAD_REQUEST,
        r#"{"code":"Client specified an invalid argument","error":"Incorrect API key provided: xa***yz. You can obtain an API key from https://console.x.ai."}"#,
    )
    .await;
    let result = test_at(ProviderKind::Xai, &xai, Some("xai-key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::KeyRejected);
    assert!(!result.message.contains("console.x.ai"));

    let other = answering(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"unsupported parameter"}}"#,
    )
    .await;
    let result = test_at(ProviderKind::Gemini, &other, Some("gemini-key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::UnexpectedAnswer);
}

#[tokio::test]
async fn access_rate_limits_and_other_answers_are_classified() {
    let forbidden = answering(StatusCode::FORBIDDEN, "{}").await;
    let result = test_at(ProviderKind::Openai, &forbidden, Some("openai-key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::AccessDenied);
    assert_eq!(result.status, Some(403));
    assert!(
        !result.message.contains("rejected"),
        "a bare 403 does not call the key invalid: {}",
        result.message
    );

    let limited = answering(StatusCode::TOO_MANY_REQUESTS, "{}").await;
    let result = test_at(ProviderKind::Together, &limited, Some("together-key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::RateLimited);
    assert_eq!(result.status, Some(429));

    let missing = answering(StatusCode::NOT_FOUND, "<html>not here</html>").await;
    let result = test_at(ProviderKind::OpenaiCompatible, &missing, Some("key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::UnexpectedAnswer);
    assert_eq!(result.status, Some(404));
    assert!(result.message.contains("Check the base URL"));

    let broken = answering(StatusCode::SERVICE_UNAVAILABLE, "down for maintenance").await;
    let result = test_at(ProviderKind::OpenaiCompatible, &broken, Some("key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::UnexpectedAnswer);
    assert!(result.message.contains("HTTP 503"), "{}", result.message);
    assert!(!result.message.contains("maintenance"));

    let page = answering(StatusCode::OK, "<html>a login page</html>").await;
    let result = test_at(ProviderKind::OpenaiCompatible, &page, None).await;
    assert_eq!(result.outcome, ProviderTestOutcome::UnexpectedAnswer);
    assert_eq!(result.status, None);

    let redirect = serve(Router::new().fallback(|| async {
        (
            StatusCode::MOVED_PERMANENTLY,
            [("location", "https://elsewhere.example/v1/models")],
        )
    }))
    .await;
    let result = test_at(ProviderKind::OpenaiCompatible, &redirect, Some("key")).await;
    assert_eq!(result.outcome, ProviderTestOutcome::UnexpectedAnswer);
    assert!(result.message.contains("redirected"), "{}", result.message);
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    // Bind and drop a listener so the port is known to be closed.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{address}/v1");

    let result = test_at(ProviderKind::Ollama, &base, None).await;

    assert_eq!(result.outcome, ProviderTestOutcome::Unreachable);
    assert_eq!(
        result.message,
        format!("Tidebreak could not reach Ollama at {base}. Check that it is running.")
    );
}

#[tokio::test]
async fn ollama_is_asked_for_its_pulled_models() {
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/api/tags",
            get(|State(seen): State<Seen>, headers: HeaderMap| async move {
                seen.record("/api/tags".into(), &headers);
                Json(serde_json::json!({ "models": [] }))
            }),
        )
        .with_state(seen.clone());
    let base = format!("{}/v1", serve(router).await);

    let result = test_at(ProviderKind::Ollama, &base, None).await;

    assert_eq!(result.outcome, ProviderTestOutcome::Connected);
    assert_eq!(result.model_count, Some(0));
    assert!(
        result.message.contains("No models are pulled yet"),
        "{}",
        result.message
    );
    assert_eq!(seen.requests().len(), 1);
}

/// OpenRouter lists its models to anyone, so a model list would pass a bad
/// key. The test asks about the key instead.
#[tokio::test]
async fn openrouter_is_asked_about_the_key_itself() {
    let key = stand_in_key("openrouter");
    // OpenRouter labels a key with a masked copy of it. Built at run time so
    // no key-shaped literal sits in the source.
    let label = ["sk", "or", "v1", "abc...xyz"].join("-");
    let seen = Seen::default();
    let router = Router::new()
        .route(
            "/api/v1/key",
            get({
                let label = label.clone();
                move |State(seen): State<Seen>, headers: HeaderMap| {
                    let label = label.clone();
                    async move {
                        seen.record("/api/v1/key".into(), &headers);
                        Json(serde_json::json!({ "data": { "label": label, "usage": 0 } }))
                    }
                }
            }),
        )
        .with_state(seen.clone());
    let base = format!("{}/api/v1", serve(router).await);

    let result = test_at(ProviderKind::Openrouter, &base, Some(&key)).await;

    assert_eq!(result.outcome, ProviderTestOutcome::Connected);
    assert_eq!(result.message, "OpenRouter accepted the saved key.");
    assert!(
        !result.message.contains(&label),
        "the masked key never comes back"
    );
    let requests = seen.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        header(&requests[0].1, "authorization"),
        Some(format!("Bearer {key}").as_str())
    );
}

// ---------------------------------------------------------------------------
// The guards in front of the request, and the recorded result.

#[derive(Default)]
struct TestSecrets(Mutex<HashMap<String, String>>);

#[async_trait::async_trait]
impl SecretProvider for TestSecrets {
    async fn get_secret(&self, key: &str) -> tidebreak_core::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    async fn set_secret(&self, key: &str, value: &str) -> tidebreak_core::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> tidebreak_core::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

async fn test_store() -> (DbStore, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = DbStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("connection-test.db").display()
    ))
    .await
    .unwrap();
    (store, directory)
}

fn unmanaged() -> ManagedPolicy {
    crate::managed_policy::resolve(
        &*crate::managed_policy::MemoryProvisionedPolicy::new(),
        &crate::managed_policy::NoOsPolicy,
    )
    .unwrap()
}

#[tokio::test]
async fn a_test_refuses_before_any_request_when_there_is_nothing_to_test() {
    let (store, _directory) = test_store().await;
    let secrets = TestSecrets::default();

    let error = test_provider(&store, &secrets, ProviderKind::ModelGateway, &unmanaged())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "provider_test_unsupported");

    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    crate::managed_policy::provision(&*provisioned, "https://gateway.example").unwrap();
    let managed =
        crate::managed_policy::resolve(&*provisioned, &crate::managed_policy::NoOsPolicy).unwrap();
    let error = test_provider(&store, &secrets, ProviderKind::Anthropic, &managed)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "managed_profile");

    // Every kind that needs a key also reads one from the environment, so the
    // missing-key check only holds where that variable is unset.
    if std::env::var_os("TOGETHER_API_KEY").is_none() {
        let error = test_provider(&store, &secrets, ProviderKind::Together, &unmanaged())
            .await
            .unwrap_err();
        assert_eq!(error.kind(), "provider_credential_missing");
    }

    // A keyless OpenAI-compatible endpoint is fine; one with no address is not.
    let error = test_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_endpoint_missing");

    // A saved key does not travel over HTTP without consent.
    providers::write_credential(
        &secrets,
        ProviderKind::OpenaiCompatible,
        &ProviderCredential::api_key(stand_in_key("compat")),
    )
    .await
    .unwrap();
    providers::write_config(
        &store,
        ProviderKind::OpenaiCompatible,
        &ProviderConfig {
            enabled: true,
            base_url: Some("http://127.0.0.1:9/v1".into()),
            ..ProviderConfig::disabled()
        },
    )
    .await
    .unwrap();
    let error = test_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind(), "provider_endpoint_insecure");

    providers::write_credential(
        &secrets,
        ProviderKind::Openai,
        &ProviderCredential::Oauth {},
    )
    .await
    .unwrap();
    let error = test_provider(&store, &secrets, ProviderKind::Openai, &unmanaged())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), "provider_test_unsupported");

    for kind in [
        ProviderKind::ModelGateway,
        ProviderKind::Anthropic,
        ProviderKind::Together,
        ProviderKind::OpenaiCompatible,
        ProviderKind::Openai,
    ] {
        assert_eq!(
            providers::read_last_test(&store, &secrets, kind)
                .await
                .unwrap(),
            None,
            "a refusal is not a test result for {kind}"
        );
    }
}

/// The last test rides the provider list until the key or the endpoint
/// changes, and then it is gone rather than vouching for the new setup.
#[tokio::test]
async fn the_last_test_is_recorded_and_dropped_when_the_setup_changes() {
    let (store, _directory) = test_store().await;
    let secrets = TestSecrets::default();
    let router = Router::new().route(
        "/v1/models",
        get(|| async { Json(serde_json::json!({ "object": "list", "data": [] })) }),
    );
    let base = format!("{}/v1", serve(router).await);
    let provisioned = crate::managed_policy::MemoryProvisionedPolicy::new();
    let update = |base_url: Option<String>, key: Option<String>| ProviderUpdate {
        enabled: Some(true),
        base_url: base_url.map(Some),
        credential: key.map(ProviderCredential::api_key),
        models: None,
        allow_loopback_http: None,
    };

    providers::update_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        update(Some(base.clone()), None),
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .unwrap();
    let result = test_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap();
    assert_eq!(result.outcome, ProviderTestOutcome::Connected);
    let listed = providers::list_providers(&store, &secrets, &unmanaged(), None)
        .await
        .unwrap();
    let compatible = listed
        .iter()
        .find(|info| info.kind == ProviderKind::OpenaiCompatible)
        .unwrap();
    assert_eq!(compatible.last_test.as_ref(), Some(&result));

    // Saving the same endpoint again keeps the verdict.
    providers::update_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        update(Some(base.clone()), None),
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .unwrap();
    assert!(
        providers::read_last_test(&store, &secrets, ProviderKind::OpenaiCompatible)
            .await
            .unwrap()
            .is_some()
    );

    // A new endpoint does not.
    let info = providers::update_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        update(Some(format!("{base}/")), None),
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .unwrap();
    assert_eq!(info.last_test, None);

    // Nor does a new key, even one the reader agreed to send to this computer.
    test_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap();
    let info = providers::update_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        ProviderUpdate {
            allow_loopback_http: Some(true),
            ..update(None, Some(stand_in_key("compat")))
        },
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .unwrap();
    assert!(info.allow_loopback_http);
    assert_eq!(info.last_test, None);

    // Another provider's change leaves this one's verdict alone.
    test_provider(
        &store,
        &secrets,
        ProviderKind::OpenaiCompatible,
        &unmanaged(),
    )
    .await
    .unwrap();
    providers::update_provider(
        &store,
        &secrets,
        ProviderKind::Together,
        update(None, Some(stand_in_key("together"))),
        &*provisioned,
        &crate::managed_policy::NoOsPolicy,
    )
    .await
    .unwrap();
    assert!(
        providers::read_last_test(&store, &secrets, ProviderKind::OpenaiCompatible)
            .await
            .unwrap()
            .is_some()
    );
}
