//! `POST /apps/{id}/invoke`: server-side enforcement and MCP dispatch.

use super::*;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{
    AppBinding, AppManifest, AppToolsBinding, CreateApp, NewAppRevision,
};
use serde_json::json;

/// The MCP client's call-result bound and the marker its clamp leaves.
const CALL_RESULT_CLAMP_BYTES: usize = 1024 * 1024;
const CLAMP_MARKER: &str = "[truncated: MCP result exceeded 1 MiB]";

/// What one invoke against the fake external server observed.
#[derive(Default)]
struct ToolServerLog {
    calls: AtomicUsize,
    last_arguments: Mutex<Option<serde_json::Value>>,
}

/// A loopback Streamable-HTTP MCP server mounting `viewer` and `unpinned`,
/// whose `tools/call` answers with the given content and structured payload.
async fn spawn_tool_server(
    log: Arc<ToolServerLog>,
    content: String,
    structured: serde_json::Value,
) -> std::net::SocketAddr {
    use axum::routing::post as axum_post;

    let handler = move |body: String| {
        let log = log.clone();
        let content = content.clone();
        let structured = structured.clone();
        async move {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let id = request.get("id").cloned().unwrap_or_default();
            let result = match request["method"].as_str().unwrap_or_default() {
                "initialize" => json!({
                    "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "invoke-fixture", "version": "1"}
                }),
                "tools/list" => json!({
                    "tools": [
                        {
                            "name": "viewer",
                            "description": "The pinned tool",
                            "inputSchema": {"type": "object"}
                        },
                        {
                            "name": "unpinned",
                            "description": "Mounted but never pinned",
                            "inputSchema": {"type": "object"}
                        }
                    ]
                }),
                "tools/call" => {
                    log.calls.fetch_add(1, Ordering::SeqCst);
                    *log.last_arguments.lock().unwrap() =
                        Some(request["params"]["arguments"].clone());
                    json!({
                        "content": [{"type": "text", "text": content}],
                        "structuredContent": structured,
                        "isError": false
                    })
                }
                _ => json!({}),
            };
            (
                [("content-type", "application/json")],
                json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            )
        }
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/mcp", axum_post(handler)))
            .await
            .unwrap();
    });
    address
}

fn tool_server_config(address: std::net::SocketAddr) -> crate::mcp_config::McpServersConfig {
    serde_json::from_value(json!({
        "servers": [{
            "name": "srv",
            "url": format!("http://{address}/mcp"),
            "request_timeout_ms": 30_000,
            "enabled": true
        }]
    }))
    .unwrap()
}

/// Create an app whose current manifest pins exactly `tools` under the
/// connected app the configured `srv` server became.
async fn create_pinned_app(store: &Arc<dyn Store>, tools: &[&str]) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Invoke fixture".into(),
                    bindings: vec![AppBinding::Tools(AppToolsBinding {
                        app: connected_app_id(store, "srv").await,
                        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
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

/// Configure the mounted server set to one `srv` at `address` over the API.
async fn put_srv(router: &Router, bearer: &str, address: std::net::SocketAddr) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header("authorization", bearer)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"servers": [{
                        "name": "srv",
                        "url": format!("http://{address}/mcp"),
                        "request_timeout_ms": 30_000,
                        "enabled": true
                    }]})
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

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

/// The route's whole enforcement ladder, fail-closed at every rung: a missing
/// or deleted app is 404, an unpinned or non-MCP name is refused before the
/// gate, and — the fail-closed default — even a perfectly pinned tool on a
/// mounted, healthy server is refused with `consent_required` until the user
/// consents, and the external server never sees a `tools/call`.
#[tokio::test]
async fn every_invoke_is_refused_before_dispatch_without_a_grant() {
    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "ok".into(), json!({})).await;
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    put_srv(&router, &bearer, address).await;
    let app_id = create_pinned_app(&store, &["mcp__srv__viewer"]).await;

    let refusal = |status: StatusCode, kind: &str| (status, kind.to_owned());
    let cases = [
        // No such app: refused before anything is inspected.
        (
            AppId::new(),
            json!({"tool": "mcp__srv__viewer"}),
            refusal(StatusCode::NOT_FOUND, "app_not_found"),
        ),
        // Mounted and healthy, but the manifest never pinned it.
        (
            app_id,
            json!({"tool": "mcp__srv__unpinned"}),
            refusal(StatusCode::FORBIDDEN, "not_pinned"),
        ),
        // A non-MCP name can never be pinned by a validated manifest.
        (
            app_id,
            json!({"tool": "read_file"}),
            refusal(StatusCode::FORBIDDEN, "not_pinned"),
        ),
        // Pinned, mounted, healthy — still refused, because the user never
        // consented. No grant, no call.
        (
            app_id,
            json!({"tool": "mcp__srv__viewer", "arguments": {"q": "open"}}),
            refusal(StatusCode::FORBIDDEN, "consent_required"),
        ),
    ];
    for (target, body, (status, kind)) in cases {
        let response = invoke(&router, &bearer, target, body.clone()).await;
        assert_eq!(response.status(), status, "{body}");
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, kind, "{body}");
    }

    // A soft-deleted app refuses identically to a missing one.
    store.delete_app(app_id, chrono::Utc::now()).await.unwrap();
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    assert_eq!(
        log.calls.load(Ordering::SeqCst),
        0,
        "no refusal path may reach the external server"
    );
}

/// The grant lifecycle end to end over the API: consent opens the gate and a
/// granted invoke round-trips to the external server; revocation closes it on
/// the very next invoke, and the refusal is `consent_required` — a re-prompt,
/// not an error.
#[tokio::test]
async fn consent_opens_the_gate_and_revocation_closes_it() {
    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "ok".into(), json!({})).await;
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    put_srv(&router, &bearer, address).await;
    let app_id = create_pinned_app(&store, &["mcp__srv__viewer"]).await;

    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer", "arguments": {"q": "open"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let result: serde_json::Value = json_body(response).await;
    assert_eq!(result["content"], json!("ok"));
    assert_eq!(log.calls.load(Ordering::SeqCst), 1);

    let revoked = grant_request(&router, &bearer, "DELETE", app_id).await;
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
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

/// The property that makes MCP approvals per-call in chats, preserved here
/// without per-call friction: a Settings edit can swap the process behind a
/// stable server name, so a reconfigured definition invalidates the grant. The
/// granted invoke works; after the definition changes the same invoke refuses
/// with `consent_required` and the state projection flags the server as
/// changed; a fresh consent re-pins to the new definition.
#[tokio::test]
async fn reconfiguring_a_bound_server_invalidates_the_grant() {
    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "first".into(), json!({})).await;
    let swapped_log = Arc::new(ToolServerLog::default());
    let swapped = spawn_tool_server(swapped_log.clone(), "second".into(), json!({})).await;
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    put_srv(&router, &bearer, address).await;
    let app_id = create_pinned_app(&store, &["mcp__srv__viewer"]).await;

    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The same name now resolves to a different process.
    put_srv(&router, &bearer, swapped).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    assert_eq!(
        swapped_log.calls.load(Ordering::SeqCst),
        0,
        "the swapped-in server must not be reached under the stale grant"
    );
    let state = grant_request(&router, &bearer, "GET", app_id).await;
    let state: serde_json::Value = json_body(state).await;
    assert_eq!(state["granted"], json!(false));
    assert_eq!(state["bindings"][0]["definition_changed"], json!(true));

    consent(&router, &bearer, app_id).await;
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(swapped_log.calls.load(Ordering::SeqCst), 1);
}

/// A revision that widens the manifest exceeds the grant by construction: the
/// new tool refuses with `consent_required` while the already-granted tool
/// keeps working, with no widening-specific mechanism anywhere. Re-consent is
/// computed from the server's current state — the renderer's bare "yes"
/// cannot name tools — so afterwards the new tool is invokable.
#[tokio::test]
async fn a_widened_manifest_requires_fresh_consent_for_the_new_tools() {
    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "ok".into(), json!({})).await;
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    put_srv(&router, &bearer, address).await;
    let app_id = create_pinned_app(&store, &["mcp__srv__viewer"]).await;
    consent(&router, &bearer, app_id).await;

    store
        .append_app_revision(
            app_id,
            &NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Invoke fixture".into(),
                    bindings: vec![AppBinding::Tools(AppToolsBinding {
                        app: connected_app_id(&store, "srv").await,
                        tools: vec!["mcp__srv__viewer".into(), "mcp__srv__unpinned".into()],
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
        json!({"tool": "mcp__srv__unpinned"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let info: AgentErrorInfo = json_body(response).await;
    assert_eq!(info.kind, "consent_required");
    let response = invoke(
        &router,
        &bearer,
        app_id,
        json!({"tool": "mcp__srv__viewer"}),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "tools within the granted set stay invokable"
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
        json!({"tool": "mcp__srv__unpinned"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn dispatch_state(
    base: ToolRegistry,
    address: std::net::SocketAddr,
) -> (AppState, tempfile::TempDir) {
    let (dir, store) = temp_db_store("app-invoke.db").await;
    let store: Arc<dyn Store> = Arc::new(store);
    let state = AppState::new(
        Config::desktop(dir.path()),
        store,
        Arc::new(FixedResolver(Arc::new(FakeProvider))),
        Arc::new(MemSecrets::default()),
        Arc::new(base),
        AgentConfig {
            model: "fake".into(),
            ..AgentConfig::default()
        },
    );
    state
        .mcp
        .replace(tool_server_config(address))
        .await
        .unwrap();
    (state, dir)
}

/// With the consent gate out of the way (dispatch driven directly, which is
/// exactly what a granted invoke will do), a pinned name round-trips to the
/// external server: arguments pass through verbatim, the oversized text comes
/// back clamped with the client's marker, and the structured payload crosses
/// as untouched passthrough JSON.
#[tokio::test]
async fn gate_opened_dispatch_round_trips_clamped_opaque_results() {
    let log = Arc::new(ToolServerLog::default());
    let structured = json!({"rows": [{"id": 7, "status": "open"}]});
    let address = spawn_tool_server(
        log.clone(),
        "x".repeat(CALL_RESULT_CLAMP_BYTES + 64),
        structured.clone(),
    )
    .await;
    let (state, _dir) = dispatch_state(ToolRegistry::new(), address).await;

    let arguments = json!({"q": "open", "nested": {"limit": 3}});
    let result =
        crate::routes::dispatch_mounted_tool(&state, "mcp__srv__viewer", arguments.clone())
            .await
            .unwrap();

    assert_eq!(log.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        log.last_arguments.lock().unwrap().clone(),
        Some(arguments),
        "arguments must cross to the server verbatim"
    );
    assert!(result.content.len() <= CALL_RESULT_CLAMP_BYTES);
    assert!(
        result.content.ends_with(CLAMP_MARKER),
        "the clamp marker must survive to the invoke result"
    );
    assert_eq!(
        result.structured_content,
        Some(structured),
        "structured content is opaque passthrough"
    );
    assert!(!result.is_error);
}

/// Even driven with the gate open, dispatch executes mounted MCP tools only:
/// a built-in server tool, a client-executed contract (including one hiding
/// under a mounted-looking name), and an unmounted name are all refused.
#[tokio::test]
async fn gate_opened_dispatch_refuses_every_non_mcp_surface() {
    struct BaseTool;

    #[async_trait]
    impl Tool for BaseTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "innocent_tool".into(),
                description: "a built-in server tool".into(),
                input_schema: json!({"type": "object"}),
            }
        }
        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::ReadOnly
        }
        async fn execute(
            &self,
            _ctx: &ToolCtx,
            _args: serde_json::Value,
        ) -> openwave_core::Result<ToolOutput> {
            panic!("the invoke route must never execute a built-in tool");
        }
    }

    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "ok".into(), json!({})).await;
    let mut base = ToolRegistry::new();
    base.register(Box::new(BaseTool));
    base.register_client(
        ToolSpec {
            name: "mcp__srv__client_owned".into(),
            description: "client-executed contract".into(),
            input_schema: json!({"type": "object"}),
        },
        ApprovalClass::ReadOnly,
    );
    let (state, _dir) = dispatch_state(base, address).await;

    for name in [
        "innocent_tool",
        "mcp__srv__client_owned",
        "mcp__srv__missing",
        "mcp____tool",
        "mcp__srv__",
    ] {
        let error = crate::routes::dispatch_mounted_tool(&state, name, json!({}))
            .await
            .expect_err(name);
        match error {
            crate::routes::AppInvokeError::Refused(refusal) => assert_eq!(
                refusal.kind,
                crate::routes::AppInvokeRefusalKind::UnknownTool,
                "{name}"
            ),
            other => panic!("{name}: expected a typed refusal, got {other:?}"),
        }
    }
    assert_eq!(log.calls.load(Ordering::SeqCst), 0);
}

// --- REST operation bindings ---

use std::net::IpAddr;

use base64::Engine as _;
use openwave_core::connected_app::{ConnectedApp, ConnectedAppKind};
use openwave_core::id::ConnectedAppId;
use openwave_core::local_app::AppOperationsBinding;

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
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
        (
            json!({}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_invoke_request",
        ),
        (
            json!({"operation_id": "listIssues", "arguments": {"q": "open"}}),
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
    assert_eq!(log.calls.load(Ordering::SeqCst), 0);

    // Consent projects the operation vocabulary under the app's display name.
    let response = grant_request(&router, &bearer, "POST", app_id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let state: serde_json::Value = json_body(response).await;
    assert_eq!(state["granted"], json!(true));
    assert_eq!(state["bindings"][0]["name"], json!("issues"));
    assert_eq!(state["bindings"][0]["operation_ids"], json!(["listIssues"]));
    assert_eq!(state["bindings"][0]["tools"], json!(null));

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
