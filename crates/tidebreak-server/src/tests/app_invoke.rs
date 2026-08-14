//! `POST /apps/{id}/invoke`: server-side enforcement and governed REST
//! dispatch.

use super::*;

use serde_json::json;
use tidebreak_core::id::{AppId, AppRevisionId};
use tidebreak_core::local_app::{AppBinding, AppManifest, CreateApp, NewAppRevision};

/// Send one of the body-less grant requests (`GET`, `POST` consent, `DELETE`).
async fn grant_request(
    router: &Router,
    bearer: &str,
    method: &str,
    app_id: AppId,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/apps/{app_id}/grant"))
                .header("authorization", bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Record consent for the app and assert the resulting state is fully granted.
async fn consent(router: &Router, bearer: &str, app_id: AppId) {
    let response = grant_request(router, bearer, "POST", app_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let state: serde_json::Value = json_body(response).await;
    assert_eq!(state["granted"], json!(true));
}

async fn invoke(
    router: &Router,
    bearer: &str,
    app_id: AppId,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/apps/{app_id}/invoke"))
                .header("authorization", bearer)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

// --- REST operation bindings ---

use std::net::IpAddr;

use base64::Engine as _;
use tidebreak_core::connected_app::{ConnectedApp, ConnectedAppKind};
use tidebreak_core::id::ConnectedAppId;
use tidebreak_core::local_app::AppOperationsBinding;

use crate::rest_executor::{
    RestExecuteError, RestExecutor, RestHostResolver, RestOperationResponse, RestTransport,
    RestTransportRequest,
};

/// What the fake REST transport observed and how it should answer next.
#[derive(Default)]
struct RestCallLog {
    calls: AtomicUsize,
    last_url: Mutex<Option<String>>,
    last_headers: Mutex<Vec<(String, String)>>,
    fail_next: AtomicBool,
}

/// A transport seam standing in for the network: the real [`RestExecutor`]
/// still validates against the catalog, admits the base URL, vets the
/// resolved addresses, and injects the credential — only the wire is fake.
struct FakeRestTransport(Arc<RestCallLog>);

#[async_trait]
impl RestTransport for FakeRestTransport {
    async fn execute(
        &self,
        request: &RestTransportRequest,
    ) -> std::result::Result<RestOperationResponse, RestExecuteError> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        *self.0.last_url.lock().unwrap() = Some(request.url.to_string());
        *self.0.last_headers.lock().unwrap() = request.headers.clone();
        if self.0.fail_next.swap(false, Ordering::SeqCst) {
            return Err(RestExecuteError::Timeout);
        }
        Ok(RestOperationResponse {
            status: 200,
            content_type: Some("application/json".into()),
            body: br#"{"issues":[]}"#.to_vec(),
        })
    }
}

/// Resolves every host to a public address so admission always vets cleanly.
struct PublicResolver;

#[async_trait]
impl RestHostResolver for PublicResolver {
    async fn resolve(&self, _host: &str) -> std::result::Result<Vec<IpAddr>, RestExecuteError> {
        Ok(vec![IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34))])
    }
}

/// An authenticated API whose REST dispatch runs the real governed executor
/// over the fake transport, with the credential value seeded in the secret
/// store.
async fn rest_test_app(
    log: Arc<RestCallLog>,
) -> (Router, String, Arc<dyn Store>, tempfile::TempDir) {
    let (dir, store) = temp_db_store("rest-invoke.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state
        .secrets
        .set_secret("issues-token", "sk-test-rest")
        .await
        .unwrap();
    state.rest_dispatch = Arc::new(RestExecutor::new(
        FakeRestTransport(log),
        PublicResolver,
        state.secrets.clone(),
    ));
    let bearer = format!("Bearer {}", state.token);
    (app(state), bearer, store, dir)
}

/// Replace the profile's `rest_api` records with one record for `id`,
/// pointing at `base_url` with a catalog ingested from an inline spec.
async fn seed_rest_record(store: &Arc<dyn Store>, id: ConnectedAppId, base_url: &str) {
    let spec = json!({
        "openapi": "3.0.3",
        "info": { "title": "Issues", "version": "1" },
        "paths": {
            "/issues": {
                "get": {
                    "operationId": "listIssues",
                    "parameters": [
                        { "name": "q", "in": "query", "schema": { "type": "string" } }
                    ]
                }
            },
            "/issues/{id}": {
                "get": {
                    "operationId": "getIssue",
                    "parameters": [
                        { "name": "id", "in": "path", "required": true,
                          "schema": { "type": "string" } }
                    ]
                }
            }
        }
    });
    let catalog =
        crate::openapi_catalog::ingest_openapi_document(spec.to_string().as_bytes(), None).unwrap();
    store
        .replace_connected_apps(
            ConnectedAppKind::RestApi,
            &[ConnectedApp {
                id,
                name: "issues".into(),
                kind: ConnectedAppKind::RestApi,
                definition: json!({
                    "base_url": base_url,
                    "catalog": catalog,
                    "credential": {
                        "secret_name": "issues-token",
                        "placement": "bearer"
                    },
                }),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
        )
        .await
        .unwrap();
}

/// Create an app whose current manifest pins exactly `operation_ids` under
/// the given `rest_api` connected app.
async fn create_operation_app(
    store: &Arc<dyn Store>,
    connected: ConnectedAppId,
    operation_ids: &[&str],
) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "REST fixture".into(),
                    bindings: vec![AppBinding::Operations(AppOperationsBinding {
                        app: connected,
                        operation_ids: operation_ids.iter().map(|id| (*id).to_owned()).collect(),
                    })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    app_id
}

/// The whole `rest_api` ladder over the API: an ungranted or malformed invoke
/// never reaches the transport; consent projects the operation vocabulary and
/// opens the gate; a granted invoke runs the real governed executor — catalog
/// validation, URL assembly, credential injection — over the fake wire and
/// crosses back as opaque base64 passthrough; an executor failure is an
/// `is_error` result, not a 500; and editing the record's definition
/// invalidates the grant on the very next invoke.
#[tokio::test]
async fn rest_invokes_walk_the_ladder_and_round_trip_through_the_governed_executor() {
    let log = Arc::new(RestCallLog::default());
    let (router, bearer, store, _dir) = rest_test_app(log.clone()).await;
    let connected = ConnectedAppId::new();
    seed_rest_record(&store, connected, "https://api.example.com/v2").await;
    let app_id = create_operation_app(&store, connected, &["listIssues"]).await;

    // Fail-closed before consent, and on every malformed or unpinned shape.
    // A body carrying the removed `tools` vocabulary's `tool` field no longer
    // parses at all (#1332, #1589).
    let refusals = [
        (
            json!({"operation_id": "listIssues"}),
            StatusCode::FORBIDDEN,
            "consent_required",
        ),
        (
            json!({"operation_id": "getIssue"}),
            StatusCode::FORBIDDEN,
            "not_pinned",
        ),
        (
            json!({"operation_id": "listIssues", "tool": "mcp__srv__viewer"}),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            json!({}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
        (
            json!({"operation_id": "listIssues", "op": "list"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
    ];
    for (body, status, kind) in refusals {
        let response = invoke(&router, &bearer, app_id, body.clone()).await;
        assert_eq!(response.status(), status, "{body}");
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, kind, "{body}");
    }
    // No such app: refused before anything is inspected.
    let response = invoke(
        &router,
        &bearer,
        AppId::new(),
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "app_not_found");
    assert_eq!(log.calls.load(Ordering::SeqCst), 0);

    // Consent projects the operation vocabulary under the app's display name.
    let response = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let state: serde_json::Value = json_body(response).await;
    assert_eq!(state["granted"], json!(true));
    assert_eq!(state["bindings"][0]["name"], json!("issues"));
    assert_eq!(state["bindings"][0]["operation_ids"], json!(["listIssues"]));
    assert_eq!(
        state["bindings"][0].get("tools"),
        None,
        "the removed tools vocabulary must not reappear in the projection"
    );

    // A granted invoke round-trips: the executor assembled the declared URL,
    // injected the referenced credential, and the response crosses as opaque
    // base64 passthrough.
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues", "parameters": {"q": "open"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["status"], json!(200));
    assert_eq!(result["content_type"], json!("application/json"));
    assert_eq!(result["is_error"], json!(false));
    let body = base64::engine::general_purpose::STANDARD
        .decode(result["body_base64"].as_str().unwrap())
        .unwrap();
    assert_eq!(body, br#"{"issues":[]}"#);
    assert_eq!(log.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        log.last_url.lock().unwrap().as_deref(),
        Some("https://api.example.com/v2/issues?q=open")
    );
    assert!(log
        .last_headers
        .lock()
        .unwrap()
        .iter()
        .any(|(name, value)| name == "authorization" && value == "Bearer sk-test-rest"));

    // An executor failure past the gate is the app's to present: an is_error
    // result with the closed refusal text, never a 500.
    log.fail_next.store(true, Ordering::SeqCst);
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["is_error"], json!(true));
    assert_eq!(result["error"], json!("request exceeded its time budget"));
    assert_eq!(result.get("status"), None);

    // Editing the record's definition (a moved base URL) invalidates the
    // grant on the next invoke, and the projection says why.
    seed_rest_record(&store, connected, "https://api.elsewhere.example/v2").await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    let response = grant_request(&router, &bearer, "GET", app_id).await;
    let state: serde_json::Value = json_body(response).await;
    assert_eq!(state["granted"], json!(false));
    assert_eq!(state["bindings"][0]["definition_changed"], json!(true));

    // Fresh consent re-pins to the moved definition and reopens the gate.
    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(log
        .last_url
        .lock()
        .unwrap()
        .as_deref()
        .unwrap()
        .starts_with("https://api.elsewhere.example/v2/issues"));

    // A soft-deleted app refuses identically to a missing one.
    store.delete_app(app_id, chrono::Utc::now()).await.unwrap();
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Consent has nothing coherent to record when a pinned operation is not in
/// the record's current catalog: the sheet conflicts instead of granting a
/// pin that could never execute.
#[tokio::test]
async fn consent_conflicts_when_a_pinned_operation_left_the_catalog() {
    let log = Arc::new(RestCallLog::default());
    let (router, bearer, store, _dir) = rest_test_app(log.clone()).await;
    let connected = ConnectedAppId::new();
    seed_rest_record(&store, connected, "https://api.example.com/v2").await;
    let app_id = create_operation_app(&store, connected, &["retiredOp"]).await;

    let response = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let info: AgentErrorInfo = json_body(response).await;
    assert!(info.message.contains("retiredOp"), "{}", info.message);
}

/// Revocation closes the gate on the very next invoke, and the refusal is
/// `consent_required` — a re-prompt, not an error.
#[tokio::test]
async fn revocation_closes_the_gate_on_the_next_invoke() {
    let log = Arc::new(RestCallLog::default());
    let (router, bearer, store, _dir) = rest_test_app(log.clone()).await;
    let connected = ConnectedAppId::new();
    seed_rest_record(&store, connected, "https://api.example.com/v2").await;
    let app_id = create_operation_app(&store, connected, &["listIssues"]).await;

    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(log.calls.load(Ordering::SeqCst), 1);

    let revoked = grant_request(&router, &bearer, "DELETE", app_id).await;
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    assert_eq!(
        log.calls.load(Ordering::SeqCst),
        1,
        "a revoked grant must stop the next call before dispatch"
    );
}

/// A revision that widens the manifest exceeds the grant by construction: the
/// new operation refuses with `consent_required` while the already-granted
/// one keeps working, with no widening-specific mechanism anywhere.
/// Re-consent is computed from the server's current state — the renderer's
/// bare "yes" cannot name operations — so afterwards the new operation is
/// invokable.
#[tokio::test]
async fn a_widened_manifest_requires_fresh_consent_for_the_new_operations() {
    let log = Arc::new(RestCallLog::default());
    let (router, bearer, store, _dir) = rest_test_app(log.clone()).await;
    let connected = ConnectedAppId::new();
    seed_rest_record(&store, connected, "https://api.example.com/v2").await;
    let app_id = create_operation_app(&store, connected, &["listIssues"]).await;
    consent(&router, &bearer, app_id).await;

    store
        .append_app_revision(
            app_id,
            &NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "REST fixture".into(),
                    bindings: vec![AppBinding::Operations(AppOperationsBinding {
                        app: connected,
                        operation_ids: vec!["listIssues".into(), "getIssue".into()],
                    })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();

    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "getIssue", "parameters": {"id": "7"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "operations within the granted set stay invokable"
    );
    let state = grant_request(&router, &bearer, "GET", app_id).await;
    let state: serde_json::Value = json_body(state).await;
    assert_eq!(
        state["granted"],
        json!(false),
        "the widened manifest must re-prompt on next open"
    );

    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"operation_id": "getIssue", "parameters": {"id": "7"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

// --- Gateway operation bindings ---

use tidebreak_core::local_app::AppGatewayOperationsBinding;

use crate::connected_apps::{
    GatewayDispatchError, GatewayInvokeDispatcher, GatewayOperationRequest,
};
use crate::connectors::GatewayInvokeOutcome;
use crate::tests::app_grant::FakeGatewayCatalogs;

/// What the fake relay should answer next.
enum GatewayAnswer {
    Executed,
    AuthorizationRequired,
    NoSession,
    NotRegistered,
}

/// A relay seam standing in for the gateway's shared-app invoke route,
/// recording exactly what crossed it.
struct FakeGatewayDispatch {
    /// One entry per dispatched call: the invoking app and the five fields of
    /// the gateway's own invoke vocabulary.
    calls: StdMutex<Vec<serde_json::Value>>,
    answer: StdMutex<GatewayAnswer>,
}

impl FakeGatewayDispatch {
    fn new() -> Self {
        Self {
            calls: StdMutex::new(Vec::new()),
            answer: StdMutex::new(GatewayAnswer::Executed),
        }
    }

    fn answers(&self, answer: GatewayAnswer) {
        *self.answer.lock().unwrap() = answer;
    }
}

#[async_trait]
impl GatewayInvokeDispatcher for FakeGatewayDispatch {
    async fn dispatch(
        &self,
        _owner: &tidebreak_core::OwnerId,
        app: AppId,
        request: &GatewayOperationRequest,
    ) -> Result<GatewayInvokeOutcome, GatewayDispatchError> {
        self.calls.lock().unwrap().push(json!({
            "app": app,
            "gateway_app": request.gateway_app,
            "operation_id": request.operation_id,
            "path_parameters": request.path_parameters,
            "query": request.query,
            "body": request.body,
        }));
        match *self.answer.lock().unwrap() {
            GatewayAnswer::Executed => Ok(GatewayInvokeOutcome::Executed {
                status: 201,
                content_type: Some("application/json".into()),
                body_base64: base64::engine::general_purpose::STANDARD.encode(br#"{"ok":true}"#),
            }),
            GatewayAnswer::AuthorizationRequired => {
                Ok(GatewayInvokeOutcome::AuthorizationRequired {
                    message: "sign in to Issues to continue".into(),
                })
            }
            GatewayAnswer::NoSession => Err(GatewayDispatchError::NoSession),
            GatewayAnswer::NotRegistered => Err(GatewayDispatchError::NotRegistered),
        }
    }
}

/// A profile whose gateway catalog reads and relay both come from fakes,
/// holding one app that pins `operation_ids` of the gateway app `gateway_app`.
async fn gateway_test_app(
    catalogs: Arc<FakeGatewayCatalogs>,
    relay: Arc<FakeGatewayDispatch>,
    gateway_app: &str,
    operation_ids: &[&str],
) -> (Router, String, AppId, tempfile::TempDir) {
    let (dir, store) = temp_db_store("gateway-invoke.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state.gateway_catalogs = catalogs;
    state.gateway_dispatch = relay;
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Gateway fixture".into(),
                    bindings: vec![AppBinding::GatewayOperations(AppGatewayOperationsBinding {
                        gateway_app: gateway_app.to_owned(),
                        operation_ids: operation_ids.iter().map(|id| (*id).to_owned()).collect(),
                    })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    (router, bearer, app_id, dir)
}

/// The gateway ladder end to end over the API: the local gates all run before
/// anything leaves the machine, the relay receives exactly the gateway's own
/// five-field invoke vocabulary, an executed call crosses back as opaque
/// passthrough, and the two ways nothing can answer are distinguishable —
/// `gateway_authorization_required` is the viewer's to fix at the gateway,
/// `gateway_unavailable` is a pin nothing answers. A moved catalog closes the
/// gate again without a network call.
#[tokio::test]
async fn gateway_invokes_walk_the_ladder_before_anything_leaves_the_machine() {
    let catalogs = Arc::new(FakeGatewayCatalogs::signed_in(
        "https://gateway.internal.example.com",
        &[(
            "gw-issues",
            "Issues (gateway)",
            &["listIssues", "createIssue"],
        )],
    ));
    let relay = Arc::new(FakeGatewayDispatch::new());
    let (router, bearer, app_id, _dir) = gateway_test_app(
        catalogs.clone(),
        relay.clone(),
        "gw-issues",
        &["listIssues"],
    )
    .await;
    let calls = || relay.calls.lock().unwrap().len();

    // Fail closed before consent, and on every unpinned or malformed shape.
    // The local and gateway surfaces stay disjoint: `parameters` is the local
    // operation vocabulary and `query` the gateway's, and neither is quietly
    // dropped when it lands on the other.
    let refusals = [
        (
            json!({"gateway_app": "gw-issues", "operation_id": "createIssue"}),
            StatusCode::FORBIDDEN,
            "not_pinned",
        ),
        (
            json!({"gateway_app": "gw-elsewhere", "operation_id": "listIssues"}),
            StatusCode::FORBIDDEN,
            "not_pinned",
        ),
        (
            json!({"gateway_app": "gw-issues", "operation_id": "listIssues"}),
            StatusCode::FORBIDDEN,
            "consent_required",
        ),
        (
            json!({"gateway_app": "gw-issues"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
        (
            json!({"gateway_app": "gw-issues", "operation_id": "listIssues",
                   "parameters": {"q": "open"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
        (
            json!({"operation_id": "listIssues", "query": {"q": "open"}}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
    ];
    for (body, status, kind) in refusals {
        let response = invoke(&router, &bearer, app_id, body.clone()).await;
        assert_eq!(response.status(), status, "{body}");
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, kind, "{body}");
    }
    assert_eq!(calls(), 0, "no local refusal may reach the gateway");

    // Granted and current: the relay receives the gateway's own vocabulary
    // verbatim — the bound app id, the operation, and the three passthrough
    // halves — attributed to the invoking app, and the answer crosses back as
    // opaque passthrough.
    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"gateway_app": "gw-issues", "operation_id": "listIssues",
               "path_parameters": {"id": "7"}, "query": {"state": "open"},
               "body": {"note": "hi"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["status"], json!(201));
    assert_eq!(result["content_type"], json!("application/json"));
    assert_eq!(result["is_error"], json!(false));
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(result["body_base64"].as_str().unwrap())
            .unwrap(),
        br#"{"ok":true}"#
    );
    assert_eq!(
        relay.calls.lock().unwrap().as_slice(),
        [json!({
            "app": app_id,
            "gateway_app": "gw-issues",
            "operation_id": "listIssues",
            "path_parameters": {"id": "7"},
            "query": {"state": "open"},
            "body": {"note": "hi"},
        })]
    );

    // The gateway's typed `authorization_required` crosses machine-readably
    // and names the app the viewer would recognize — never the gateway's id
    // alone, and never as prose the frame would have to match on.
    relay.answers(GatewayAnswer::AuthorizationRequired);
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"gateway_app": "gw-issues", "operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "gateway_authorization_required");
    assert!(
        info.message.contains("Issues (gateway)"),
        "{}",
        info.message
    );

    // A pin nothing can answer is a conflict with current state, and the
    // message says which of the two reasons it was.
    for (answer, expected) in [
        (GatewayAnswer::NoSession, "no gateway session"),
        (GatewayAnswer::NotRegistered, "not registered"),
    ] {
        relay.answers(answer);
        let response = invoke(
            &router,
            &bearer,
            app_id,
            json!({"gateway_app": "gw-issues", "operation_id": "listIssues"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "gateway_unavailable");
        assert!(info.message.contains(expected), "{}", info.message);
    }

    // A re-ingested catalog moves the fingerprint, so consent is stale and the
    // call stops at the grant gate — before the relay, not after it.
    let before = calls();
    *catalogs.0.lock().unwrap() = Some((
        "https://gateway.internal.example.com".to_owned(),
        FakeGatewayCatalogs::roster(&[("gw-issues", "Issues (gateway)", &["listIssues"])]),
    ));
    relay.answers(GatewayAnswer::Executed);
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"gateway_app": "gw-issues", "operation_id": "listIssues"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    assert_eq!(
        calls(),
        before,
        "a stale grant must stop the call before the relay"
    );
}

// --- Folder bindings ---

use std::collections::BTreeMap as StdBTreeMap;
use std::sync::Mutex as StdMutex;

use tidebreak_core::id::HostRootId;
use tidebreak_core::local_app::{AppFolderBinding, FolderAccess};

use crate::host_folders::{
    ApprovedFolder, FolderEntry, FolderOpError, FolderWriteReceipt, HostFolders,
};

/// An in-memory host-folder seam: one approved root over a flat file map,
/// honoring the seam's contract (live-registration check, create-vs-replace
/// modes, closed errors) without a broker.
struct FakeFolderHost {
    roots: StdMutex<Vec<ApprovedFolder>>,
    files: StdMutex<StdBTreeMap<String, Vec<u8>>>,
    /// The app identity each dispatched operation carried, in order — the
    /// value the host's audit trail will attribute the I/O to.
    seen_apps: StdMutex<Vec<tidebreak_core::id::AppId>>,
}

impl FakeFolderHost {
    fn live(&self, root: HostRootId) -> bool {
        self.roots
            .lock()
            .unwrap()
            .iter()
            .any(|approved| approved.root_id == root)
    }
}

#[async_trait]
impl HostFolders for FakeFolderHost {
    async fn approved_roots(&self) -> tidebreak_core::Result<Vec<ApprovedFolder>> {
        Ok(self.roots.lock().unwrap().clone())
    }

    async fn list_folder(
        &self,
        app: tidebreak_core::id::AppId,
        root: HostRootId,
        _path: &str,
    ) -> Result<Vec<FolderEntry>, FolderOpError> {
        self.seen_apps.lock().unwrap().push(app);
        if !self.live(root) {
            return Err(FolderOpError::NotConnected);
        }
        Ok(self
            .files
            .lock()
            .unwrap()
            .keys()
            .map(|name| FolderEntry {
                name: name.clone(),
                directory: false,
            })
            .collect())
    }

    async fn read_file(
        &self,
        app: tidebreak_core::id::AppId,
        root: HostRootId,
        path: &str,
    ) -> Result<Vec<u8>, FolderOpError> {
        self.seen_apps.lock().unwrap().push(app);
        if !self.live(root) {
            return Err(FolderOpError::NotConnected);
        }
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or(FolderOpError::NotFound)
    }

    async fn write_file(
        &self,
        app: tidebreak_core::id::AppId,
        root: HostRootId,
        path: &str,
        content: &[u8],
        replace: bool,
    ) -> Result<FolderWriteReceipt, FolderOpError> {
        self.seen_apps.lock().unwrap().push(app);
        if !self.live(root) {
            return Err(FolderOpError::NotConnected);
        }
        let mut files = self.files.lock().unwrap();
        let existed = files.contains_key(path);
        if existed && !replace {
            return Err(FolderOpError::WrongMode);
        }
        if !existed && replace {
            return Err(FolderOpError::WrongMode);
        }
        files.insert(path.to_owned(), content.to_vec());
        Ok(FolderWriteReceipt {
            bytes: content.len(),
            replaced: existed,
        })
    }
}

/// Create an app whose current manifest pins exactly one folder at `access`.
async fn create_folder_app(
    store: &Arc<dyn Store>,
    folder: HostRootId,
    access: FolderAccess,
) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Folder fixture".into(),
                    bindings: vec![AppBinding::Folder(AppFolderBinding { folder, access })],
                },
                byte_len: 1,
                sha256: [0; 32],
                turn_id: None,
                producing_run_id: None,
                chat_id: None,
                created_at: chrono::Utc::now(),
            },
        })
        .await
        .unwrap();
    app_id
}

/// The folder ladder end to end over the API: the pin gates the access level
/// before the grant is read, an ungranted invoke re-prompts, a granted app
/// round-trips list/read/write through the seam as opaque base64, the mode
/// flag is honored, and disconnecting the folder invalidates the grant on
/// the very next invoke.
#[tokio::test]
async fn folder_invokes_walk_the_ladder_and_round_trip_through_the_seam() {
    use base64::Engine as _;

    let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let (dir, store) = temp_db_store("folder-invoke.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let mut state = AppState::new(
        Config::desktop(dir.path()),
        store.clone(),
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(ToolRegistry::new()),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    let root = HostRootId::from_uuid(uuid::Uuid::new_v4()).unwrap();
    let host = Arc::new(FakeFolderHost {
        roots: StdMutex::new(vec![ApprovedFolder {
            root_id: root,
            display_name: "Notes".into(),
        }]),
        files: StdMutex::new(StdBTreeMap::from([(
            "note.txt".to_owned(),
            b"hello".to_vec(),
        )])),
        seen_apps: StdMutex::new(Vec::new()),
    });
    state.host_folders = Some(host.clone());
    let bearer = format!("Bearer {}", state.token);
    let router = app(state);

    // A read-pinned app: reads and listings work after consent, and a write
    // refuses at the pin — before the grant is even consulted.
    let reader = create_folder_app(&store, root, FolderAccess::Read).await;
    consent(&router, &bearer, reader).await;
    let response = invoke(
        &router,
        &bearer,
        reader,
        json!({"folder": root, "op": "read", "path": "note.txt"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["content_base64"], json!(encode(b"hello")));
    assert_eq!(result["is_error"], json!(false));
    let response = invoke(
        &router,
        &bearer,
        reader,
        json!({"folder": root, "op": "list"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(
        result["entries"],
        json!([{ "name": "note.txt", "directory": false }])
    );
    let response = invoke(
        &router,
        &bearer,
        reader,
        json!({"folder": root, "op": "write", "path": "new.txt", "content_base64": encode(b"x")}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "not_pinned");

    // A read_write app: ungranted refuses to a re-prompt; granted writes
    // land with the mode honored; a malformed op never reaches dispatch.
    let writer = create_folder_app(&store, root, FolderAccess::ReadWrite).await;
    let response = invoke(
        &router,
        &bearer,
        writer,
        json!({"folder": root, "op": "write", "path": "state.json", "content_base64": encode(b"{}")}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    consent(&router, &bearer, writer).await;
    let response = invoke(
        &router,
        &bearer,
        writer,
        json!({"folder": root, "op": "write", "path": "state.json", "content_base64": encode(b"{}")}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["replaced"], json!(false));
    let response = invoke(
        &router,
        &bearer,
        writer,
        json!({"folder": root, "op": "write", "path": "state.json",
               "content_base64": encode(b"{\"v\":2}"), "replace": true}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["replaced"], json!(true));
    assert_eq!(
        host.files.lock().unwrap().get("state.json"),
        Some(&b"{\"v\":2}".to_vec())
    );
    let response = invoke(
        &router,
        &bearer,
        writer,
        json!({"folder": root, "op": "chmod", "path": "state.json"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Disconnecting the folder invalidates every grant naming it on the
    // next invoke — consent never outlives the registration.
    host.roots.lock().unwrap().clear();
    let response = invoke(
        &router,
        &bearer,
        writer,
        json!({"folder": root, "op": "read", "path": "note.txt"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");

    // Every dispatched operation carried the invoking app's identity — what
    // the host's audit trail attributes the I/O to. Refused invokes (pin,
    // consent, malformed, disconnected) never reached the seam at all.
    assert_eq!(
        host.seen_apps.lock().unwrap().as_slice(),
        [reader, reader, writer, writer]
    );
}
