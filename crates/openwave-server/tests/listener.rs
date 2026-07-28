use openwave_core::{Chat, Config, KeychainSecretProvider};
use openwave_server::bind;

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
async fn retrieval_routes_require_a_token() {
    let (addr, _token, _dir) = serve_test_server().await;
    let client = reqwest::Client::new();

    // Both retrieval routes sit behind the bearer-token layer, not out in the
    // open like /healthz — a request with no token is rejected before it runs.
    for uri in ["/documents", "/search"] {
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
