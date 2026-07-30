//! `POST /apps/{id}/invoke`: server-side enforcement and MCP dispatch.

use super::*;

use openwave_core::id::{AppId, AppRevisionId};
use openwave_core::local_app::{AppBinding, AppManifest, CreateApp, NewAppRevision};
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

/// Create an app whose current manifest pins exactly `tools` under `srv`.
async fn create_pinned_app(store: &Arc<dyn Store>, tools: &[&str]) -> AppId {
    let app_id = AppId::new();
    store
        .create_app(&CreateApp {
            id: app_id,
            revision: NewAppRevision {
                id: AppRevisionId::new(),
                manifest: AppManifest {
                    name: "Invoke fixture".into(),
                    bindings: vec![AppBinding {
                        server: "srv".into(),
                        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
                    }],
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
/// gate, and — the dark-ship property — even a perfectly pinned tool on a
/// mounted, healthy server is refused with `consent_required` and the external
/// server never sees a `tools/call`.
#[tokio::test]
async fn every_invoke_is_refused_before_dispatch_until_grants_exist() {
    let log = Arc::new(ToolServerLog::default());
    let address = spawn_tool_server(log.clone(), "ok".into(), json!({})).await;
    let (router, token, store, _dir) = test_app().await;
    let bearer = format!("Bearer {token}");
    let put = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/mcp/servers")
                .header("authorization", &bearer)
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
    assert_eq!(put.status(), StatusCode::OK);
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
        // The dark ship: pinned, mounted, healthy — still refused, because no
        // grant store exists yet.
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
