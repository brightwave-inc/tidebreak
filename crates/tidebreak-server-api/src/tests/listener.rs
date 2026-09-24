use crate::bind;
use tidebreak_core::{Chat, Config, KeychainSecretProvider};

async fn serve_test_server() -> (std::net::SocketAddr, String, tempfile::TempDir) {
    KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    let addr = server.local_addr();
    let token = server.token().to_string();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });
    (addr, token, dir)
}

#[tokio::test]
async fn bind_yields_a_loopback_addr_and_token() {
    KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();

    assert!(server.local_addr().ip().is_loopback());
    assert!(!server.token().is_empty());
    assert!(server.store().list_chats().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_answers_over_a_real_socket() {
    let (addr, token, _dir) = serve_test_server().await;

    let client = reqwest::Client::new();
    let health = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    let unauthed = client
        .get(format!("http://{addr}/chats"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

    let authed = client
        .get(format!("http://{addr}/chats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status(), reqwest::StatusCode::OK);
    assert_eq!(authed.json::<Vec<Chat>>().await.unwrap(), vec![]);
}

/// Delete all data stops the server before it deletes the folders it writes
/// into. A stop closes the listener, and a connection the client kept alive
/// accepts no more requests, so nothing can start new work over it.
#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_server_takes_no_more_requests() {
    KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    let addr = server.local_addr();
    let stop = server.stop_handle();
    let serving = tokio::spawn(async move { server.serve().await });

    let client = reqwest::Client::new();
    let health = client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);

    tokio::time::timeout(std::time::Duration::from_secs(20), stop.stop())
        .await
        .expect("the server and its workers stop");
    tokio::time::timeout(std::time::Duration::from_secs(5), serving)
        .await
        .expect("serve returns once stopped")
        .unwrap()
        .unwrap();
    assert!(client
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn cors_preflight_allows_localhost_origin() {
    KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let server = bind(Config::desktop(dir.path())).await.unwrap();
    let addr = server.local_addr();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    let client = reqwest::Client::new();
    let preflight = client
        .request(reqwest::Method::OPTIONS, format!("http://{addr}/chats"))
        .header(reqwest::header::ORIGIN, "http://localhost:1420")
        .header(reqwest::header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(
            reqwest::header::ACCESS_CONTROL_REQUEST_HEADERS,
            "authorization,range,if-range",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), reqwest::StatusCode::OK);
    let allow_origin = preflight
        .headers()
        .get(reqwest::header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok());
    assert_eq!(allow_origin, Some("http://localhost:1420"));
    let allow_headers = preflight
        .headers()
        .get(reqwest::header::ACCESS_CONTROL_ALLOW_HEADERS)
        .and_then(|value| value.to_str().ok())
        .unwrap();
    for expected in ["authorization", "range", "if-range"] {
        assert!(
            allow_headers
                .split(',')
                .any(|header| header.trim().eq_ignore_ascii_case(expected)),
            "missing {expected} in {allow_headers}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn api_rejects_missing_and_wrong_tokens() {
    let (addr, _token, _dir) = serve_test_server().await;
    let client = reqwest::Client::new();

    let missing = client
        .get(format!("http://{addr}/chats"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::UNAUTHORIZED);

    let wrong = client
        .get(format!("http://{addr}/chats"))
        .bearer_auth("not-the-token")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn document_routes_require_a_token() {
    let (addr, _token, _dir) = serve_test_server().await;
    let client = reqwest::Client::new();

    // Document routes sit behind the bearer-token layer, not out in the
    // open like /healthz — a request with no token is rejected before it runs.
    for uri in ["/documents"] {
        let response = client
            .post(format!("http://{addr}{uri}"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "{uri} must require a token"
        );
    }
}

/// Write `records` as the profile's saved MCP servers before boot.
async fn seed_mcp_records(
    config: &Config,
    records: &[tidebreak_core::connected_app::ConnectedApp],
) {
    let store = crate::connect_store(config).await.unwrap();
    store
        .replace_connected_apps(
            tidebreak_core::connected_app::ConnectedAppKind::McpServer,
            records,
        )
        .await
        .unwrap();
}

fn mcp_record(
    name: &str,
    definition: serde_json::Value,
) -> tidebreak_core::connected_app::ConnectedApp {
    let now = chrono::Utc::now();
    tidebreak_core::connected_app::ConnectedApp {
        id: tidebreak_core::id::ConnectedAppId::new(),
        name: name.to_string(),
        kind: tidebreak_core::connected_app::ConnectedAppKind::McpServer,
        definition,
        created_at: now,
        updated_at: now,
    }
}

/// A Streamable HTTP MCP server with one `lookup` tool that answers nothing
/// until `release` turns true.
async fn serve_held_mcp(release: tokio::sync::watch::Receiver<bool>) -> std::net::SocketAddr {
    use axum::routing::post;

    let app = axum::Router::new().route(
        "/mcp",
        post(move |body: String| {
            let mut release = release.clone();
            async move {
                let _ = release.wait_for(|released| *released).await;
                let request: serde_json::Value = serde_json::from_str(&body).unwrap();
                let id = request.get("id").cloned().unwrap_or_default();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialize" => serde_json::json!({
                        "protocolVersion": tidebreak_mcp::PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "boot-fixture", "version": "1"}
                    }),
                    "tools/list" => serde_json::json!({
                        "tools": [{"name": "lookup", "inputSchema": {"type": "object"}}]
                    }),
                    _ => serde_json::json!({}),
                };
                (
                    [("content-type", "application/json")],
                    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    address
}

async fn get_authorized(addr: std::net::SocketAddr, token: &str, path: &str) -> serde_json::Value {
    let response = reqwest::Client::new()
        .get(format!("http://{addr}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.status()
    );
    response.json().await.unwrap()
}

/// Boot used to connect every saved MCP server before it bound the port, so
/// a server that never answered held the app closed for the handshake
/// timeout and came back degraded. Now the port opens first: `/healthz`
/// answers and the server reads as connecting while it has still not
/// answered, and its tools arrive once it does.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_mcp_server_does_not_hold_the_port_closed() {
    KeychainSecretProvider::use_mock();
    let (release, held) = tokio::sync::watch::channel(false);
    let mcp = serve_held_mcp(held).await;
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());
    seed_mcp_records(
        &config,
        &[mcp_record(
            "slow",
            serde_json::json!({"name": "slow", "url": format!("http://{mcp}/mcp")}),
        )],
    )
    .await;

    let server = bind(config).await.expect("boot binds the port");
    let addr = server.local_addr();
    let token = server.token().to_string();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    let health = reqwest::get(format!("http://{addr}/healthz"))
        .await
        .unwrap();
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let listing = get_authorized(addr, &token, "/mcp/servers").await;
    assert_eq!(listing["servers"][0]["name"], "slow");
    assert_eq!(
        listing["servers"][0]["health"], "initializing",
        "the saved server is still connecting while the port already serves"
    );

    release.send(true).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let listing = get_authorized(addr, &token, "/mcp/servers").await;
        if listing["servers"][0]["health"] == "healthy" {
            assert_eq!(listing["servers"][0]["tool_count"], 1);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server did not come up once it answered: {listing}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A saved record this build cannot read used to fail boot, taking the whole
/// app down with it. Now boot completes, the record's neighbors load, and the
/// Connected apps listing names the record and why.
#[tokio::test(flavor = "multi_thread")]
async fn a_saved_mcp_record_that_cannot_load_does_not_stop_boot() {
    KeychainSecretProvider::use_mock();
    let dir = tempfile::tempdir().unwrap();
    let config = Config::desktop(dir.path());
    let corrupt = mcp_record(
        "corrupt",
        serde_json::json!({"name": "corrupt", "command": "/bin/tool", "transport": "stdio"}),
    );
    seed_mcp_records(
        &config,
        &[
            corrupt.clone(),
            mcp_record(
                "docs",
                serde_json::json!({
                    "name": "docs",
                    "url": "https://mcp.example.test/mcp",
                    "enabled": false
                }),
            ),
        ],
    )
    .await;

    let server = bind(config)
        .await
        .expect("a saved record that cannot load does not stop boot");
    let addr = server.local_addr();
    let token = server.token().to_string();
    tokio::spawn(async move {
        let _ = server.serve().await;
    });

    let servers = get_authorized(addr, &token, "/mcp/servers").await;
    let names: Vec<&str> = servers["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|server| server["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["docs"]);
    let apps = get_authorized(addr, &token, "/connected-apps").await;
    let skipped = apps["skipped_mcp_servers"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["name"], "corrupt");
    assert_eq!(skipped[0]["id"], serde_json::json!(corrupt.id));
    assert!(
        skipped[0]["reason"]
            .as_str()
            .unwrap()
            .contains("\"transport\""),
        "{skipped:?}"
    );
}
