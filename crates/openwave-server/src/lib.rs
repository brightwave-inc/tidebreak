//! OpenWave's in-process HTTP/WebSocket surface.
//!
//! Every client — the desktop webview, the CLI — drives the agent through this
//! one local API rather than linking the loop directly, so all surfaces share a
//! single wiring of `Config`, `Store`, and (next slice) the agent. The server
//! binds to an ephemeral **loopback** port and mints a per-launch **bearer
//! token**: only the local process it was handed to can reach it.
//!
//! This slice is the chat surface — create / list / get, behind the token.
//! Posting a message and streaming the turn over `WS /chats/{id}/events`
//! lands next, on top of this skeleton.

mod auth;
mod error;
mod extract;
mod routes;
mod state;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tokio::net::TcpListener;

use openwave_core::{AgentError, Config, DbStore, Profile, Result, Store};

pub use error::ServerError;
pub use state::AppState;

/// Build the router: unauthenticated health check plus the token-guarded API.
pub fn app(state: AppState) -> Router {
    // `route_layer` applies the token check to matched API routes only, so an
    // unknown path still answers `404` (not `401`), and `/healthz` stays open.
    let api = Router::new()
        .route("/chats", post(routes::create_chat).get(routes::list_chats))
        .route("/chats/{id}", get(routes::get_chat))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .with_state(state);

    Router::new().route("/healthz", get(healthz)).merge(api)
}

/// Liveness probe — no auth, no state.
async fn healthz() -> &'static str {
    "ok"
}

/// A bound server: the loopback address and per-launch token are known, so the
/// spawning client can be told where to connect before the accept loop starts.
pub struct Server {
    local_addr: SocketAddr,
    token: Arc<str>,
    listener: TcpListener,
    router: Router,
}

impl Server {
    /// The loopback address the server is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The bearer token clients must present.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Run the accept loop until the process exits.
    pub async fn serve(self) -> Result<()> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(|e| AgentError::msg(format!("server error: {e}")))
    }
}

/// Wire the store from `config` and bind the API to an ephemeral loopback port.
pub async fn bind(config: Config) -> Result<Server> {
    let store = connect_store(&config).await?;
    let state = AppState::new(config, store);
    let token = state.token.clone();
    let router = app(state);

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| AgentError::config(format!("failed to bind loopback: {e}")))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| AgentError::config(format!("no local address: {e}")))?;

    Ok(Server {
        local_addr,
        token,
        listener,
        router,
    })
}

/// Open the durable store the profile selects.
///
/// Only the desktop profile (SQLite under `data_dir`) is wired today; the
/// self-host Postgres store lands with that profile's slice.
async fn connect_store(config: &Config) -> Result<Arc<dyn Store>> {
    match config.profile {
        Profile::Desktop => {
            std::fs::create_dir_all(&config.data_dir)
                .map_err(|e| AgentError::config(format!("failed to create data dir: {e}")))?;
            let store = DbStore::connect(&config.database_url()?).await?;
            Ok(Arc::new(store))
        }
        _ => Err(AgentError::config(
            "only the desktop profile is supported for now",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use openwave_core::{AgentErrorInfo, Chat, ChatId, Profile};
    use serde::de::DeserializeOwned;
    use tower::ServiceExt;

    /// A router backed by a fresh temp SQLite store, plus its token and the
    /// tempdir (kept alive for the test's duration).
    async fn test_app() -> (Router, Arc<str>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("t.db").display()
        ))
        .await
        .unwrap();
        let state = AppState::new(Config::desktop(dir.path()), Arc::new(store));
        let token = state.token.clone();
        (app(state), token, dir)
    }

    async fn json_body<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let (router, _token, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_rejects_missing_and_wrong_tokens() {
        let (router, _token, _dir) = test_app().await;
        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let wrong = router
            .oneshot(
                Request::builder()
                    .uri("/chats")
                    .header(header::AUTHORIZATION, "Bearer not-the-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn create_then_get_and_list() {
        let (router, token, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        let created: Chat = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/chats")
                        .header(header::AUTHORIZATION, &bearer)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({"workspace_dir": "/tmp/ws", "title": "hi"})
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
            json_body(response).await
        };
        assert_eq!(created.title.as_deref(), Some("hi"));

        let fetched: Chat = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/chats/{}", created.id))
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(fetched, created);

        let listed: Vec<Chat> = {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/chats")
                        .header(header::AUTHORIZATION, &bearer)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            json_body(response).await
        };
        assert_eq!(listed, vec![created]);
    }

    #[tokio::test]
    async fn relative_workspace_dir_is_rejected() {
        let (router, token, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "relative/dir"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn unknown_chat_is_404() {
        let (router, token, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri(format!("/chats/{}", ChatId::new()))
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn bind_yields_a_loopback_addr_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let server = bind(Config::desktop(dir.path())).await.unwrap();
        assert!(server.local_addr().ip().is_loopback());
        assert!(!server.token().is_empty());
    }

    #[tokio::test]
    async fn malformed_requests_get_json_errors_not_plaintext() {
        let (router, token, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");

        // A non-UUID path segment: 400 with a parseable `{ kind, message }` body,
        // not axum's default plain-text rejection.
        let bad_path = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/chats/not-a-uuid")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad_path.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(bad_path).await;
        assert_eq!(info.kind, "bad_request");

        // A body with no `Content-Type: application/json`: also a JSON 400.
        let no_content_type = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .body(Body::from(r#"{"workspace_dir":"/tmp/ws"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_content_type.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(no_content_type).await;
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test]
    async fn self_host_profile_is_not_yet_supported() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            profile: Profile::SelfHost,
            data_dir: dir.path().to_path_buf(),
        };
        assert!(bind(config).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serve_answers_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let server = bind(Config::desktop(dir.path())).await.unwrap();
        let addr = server.local_addr();
        let token = server.token().to_string();
        // The listener is already bound, so connections queue immediately; drive
        // the accept loop in the background for the duration of the test.
        tokio::spawn(async move {
            let _ = server.serve().await;
        });

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
}
