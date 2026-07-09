//! OpenWave's in-process HTTP/WebSocket surface.
//!
//! Every client — the desktop webview, the CLI — drives the agent through this
//! one local API rather than linking the loop directly, so all surfaces share a
//! single wiring of `Config`, `Store`, and (next slice) the agent. The server
//! binds to an ephemeral **loopback** port and mints a per-launch **bearer
//! token**: only the local process it was handed to can reach it.
//!
//! The surface runs turns end to end: the chat CRUD routes, `POST
//! /chats/{id}/messages` to start a turn (one per chat at a time), and
//! `WS /chats/{id}/events` to watch it — journaled events are replayed on connect
//! and then streamed live (snapshot → replay → live).

mod auth;
mod bus;
mod error;
mod extract;
mod hub;
mod provider;
mod providers;
mod resolver;
mod routes;
mod state;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tokio::net::TcpListener;

use openwave_core::{
    AgentConfig, AgentError, Config, DbStore, KeychainSecretProvider, ListDir, Profile, ReadFile,
    Result, SecretProvider, Store, ToolRegistry, WriteFile,
};

use resolver::KeyedResolver;

pub use error::ServerError;
pub use state::AppState;

/// Build the router: unauthenticated health check plus the token-guarded API.
pub fn app(state: AppState) -> Router {
    // `route_layer` applies the token check to matched API routes only, so an
    // unknown path still answers `404` (not `401`), and `/healthz` stays open.
    let api = Router::new()
        .route(
            "/settings",
            get(routes::get_settings).put(routes::put_settings),
        )
        .route(
            "/projects",
            post(routes::create_project).get(routes::list_projects),
        )
        .route("/projects/{id}", get(routes::get_project))
        .route("/models", get(routes::list_models))
        .route("/providers", get(routes::list_providers))
        .route(
            "/providers/{kind}",
            axum::routing::put(routes::put_provider),
        )
        .route(
            "/providers/{kind}/credential",
            axum::routing::delete(routes::delete_provider_credential),
        )
        .route("/chats", post(routes::create_chat).get(routes::list_chats))
        .route(
            "/chats/{id}",
            get(routes::get_chat).patch(routes::patch_chat),
        )
        .route(
            "/settings/api-key",
            axum::routing::put(routes::put_api_key).delete(routes::delete_api_key),
        )
        .route("/chats/{id}/messages", post(routes::post_message))
        .route("/chats/{id}/events", get(routes::chat_events))
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

/// Default model when none is configured via settings or per-chat. Overridable
/// with `OPENWAVE_MODEL`.
const DEFAULT_MODEL: &str = "claude-opus-4-8";

/// Wire the store from `config` and bind the API to an ephemeral loopback port.
pub async fn bind(config: Config) -> Result<Server> {
    let store = connect_store(&config).await?;
    let secrets: Arc<dyn SecretProvider> = Arc::new(KeychainSecretProvider::new());
    // Pre-providers installs may only have an env/legacy key — enable Anthropic
    // so `KeyedResolver`'s enabled check doesn't fail-closed on upgrade.
    providers::migrate_legacy_anthropic(&*store, &*secrets).await?;
    let resolver = Arc::new(KeyedResolver::new(store.clone(), secrets.clone()));
    let (tools, agent_config) = agent_deps();
    let state = AppState::new(config, store, resolver, secrets, tools, agent_config);
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

/// Assemble the static agent dependencies for a real launch: the tool set and
/// the per-turn tuning. The model **provider** is not built here — it is resolved
/// per turn by the [`KeyedResolver`] (a composite router over enabled providers;
/// see [`resolver`]), so configuring a provider at runtime takes effect without a
/// restart. The model *name* comes from `OPENWAVE_MODEL` (or the built-in default)
/// and can be overridden at runtime via `PUT /settings` or per-chat.
fn agent_deps() -> (Arc<ToolRegistry>, AgentConfig) {
    let tools = Arc::new(
        ToolRegistry::new()
            .with(Box::new(ReadFile))
            .with(Box::new(ListDir))
            .with(Box::new(WriteFile)),
    );
    let model = std::env::var("OPENWAVE_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let agent_config = AgentConfig {
        model,
        ..AgentConfig::default()
    };
    (tools, agent_config)
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

    use std::time::Duration;

    use async_trait::async_trait;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, Request, StatusCode};
    use futures::stream::{self, BoxStream, StreamExt};
    use openwave_core::{
        AgentErrorInfo, AgentEvent, Chat, ChatId, ChatRequest, ModelProvider, Project, ProjectId,
        ProviderEvent, ProviderId, SecretProvider, SequencedEvent, StopReason, Usage,
    };
    use resolver::ProviderResolver;
    use serde::de::DeserializeOwned;
    use tokio::sync::Notify;
    use tower::ServiceExt;

    /// A provider that answers with a one-line completion and no tool calls.
    struct FakeProvider;

    #[async_trait]
    impl ModelProvider for FakeProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("fake")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            Ok(stream::iter(vec![
                ProviderEvent::TextDelta { text: "hi".into() },
                ProviderEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..Default::default()
                }),
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ])
            .boxed())
        }
    }

    /// A provider that records the model each request asked for, then answers
    /// like `FakeProvider`. Lets a test assert which model a turn ran against.
    #[derive(Clone, Default)]
    struct RecordingProvider {
        models: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ModelProvider for RecordingProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("recording")
        }
        async fn stream(&self, req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            self.models.lock().unwrap().push(req.model);
            Ok(stream::iter(vec![ProviderEvent::Stop {
                reason: StopReason::EndTurn,
            }])
            .boxed())
        }
    }

    /// A provider whose completion blocks on `gate` until the test releases it —
    /// so a turn stays active while the test checks concurrency behavior.
    struct GatedProvider {
        gate: Arc<Notify>,
    }

    #[async_trait]
    impl ModelProvider for GatedProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("gated")
        }
        async fn stream(&self, _req: ChatRequest) -> Result<BoxStream<'static, ProviderEvent>> {
            let gate = self.gate.clone();
            Ok(stream::once(async move {
                gate.notified().await;
                ProviderEvent::Stop {
                    reason: StopReason::EndTurn,
                }
            })
            .boxed())
        }
    }

    /// A resolver that always hands back a fixed provider — lets a test inject a
    /// fake in place of the real credential-driven resolution.
    struct FixedResolver(Arc<dyn ModelProvider>);

    #[async_trait]
    impl ProviderResolver for FixedResolver {
        async fn resolve(&self) -> Arc<dyn ModelProvider> {
            self.0.clone()
        }
    }

    /// An in-memory `SecretProvider` for tests (no OS keychain).
    #[derive(Default)]
    struct MemSecrets(std::sync::Mutex<std::collections::HashMap<String, String>>);

    #[async_trait]
    impl SecretProvider for MemSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// A router over a fresh temp SQLite store with the given provider; returns
    /// the router, token, the store (to inspect the journal), and the tempdir.
    async fn test_app_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("t.db").display()
            ))
            .await
            .unwrap(),
        );
        let state = AppState::new(
            Config::desktop(dir.path()),
            store.clone(),
            Arc::new(FixedResolver(provider)),
            Arc::new(MemSecrets::default()),
            Arc::new(ToolRegistry::new()),
            AgentConfig {
                model: "fake".into(),
                ..AgentConfig::default()
            },
        );
        let token = state.token.clone();
        (app(state), token, store, dir)
    }

    async fn test_app() -> (Router, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        test_app_with(Arc::new(FakeProvider)).await
    }

    async fn json_body<T: DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Create a chat and return it.
    async fn make_chat(router: &Router, bearer: &str) -> Chat {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    }

    /// POST a message to a chat, returning the response status.
    async fn send_message(
        router: &Router,
        bearer: &str,
        chat: ChatId,
        content: &str,
    ) -> StatusCode {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/chats/{chat}/messages"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"content": content}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Poll the journal until the turn terminates (or time out), returning its
    /// events in sequence order.
    async fn wait_for_turn(store: &Arc<dyn Store>, chat: ChatId) -> Vec<SequencedEvent> {
        for _ in 0..200 {
            let events = store.list_events(chat, 0).await.unwrap();
            if events.iter().any(|e| {
                matches!(
                    e.event,
                    AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. }
                )
            }) {
                return events;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("turn did not finish within the timeout");
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let (router, _token, _store, _dir) = test_app().await;
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
        let (router, _token, _store, _dir) = test_app().await;
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
        let (router, token, _store, _dir) = test_app().await;
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

    /// Create a project and return it.
    async fn make_project(router: &Router, bearer: &str) -> Project {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/projects")
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/proj", "title": "p"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_body(response).await
    }

    #[tokio::test]
    async fn project_create_get_and_list() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let created = make_project(&router, &bearer).await;
        assert_eq!(created.title.as_deref(), Some("p"));

        let fetched: Project = {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/projects/{}", created.id))
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

        let listed: Vec<Project> = {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/projects")
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
    async fn chat_can_be_filed_under_a_project() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let project = make_project(&router, &bearer).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "project_id": project.id})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let chat: Chat = json_body(response).await;
        assert_eq!(chat.project_id, Some(project.id));
    }

    #[tokio::test]
    async fn chat_referencing_an_unknown_project_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "project_id": ProjectId::new()})
                            .to_string(),
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
    async fn models_catalog_is_served() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/models")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let catalog: serde_json::Value = json_body(response).await;
        let models = catalog["models"].as_array().unwrap();
        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m["provider"] == "anthropic"));
    }

    #[tokio::test]
    async fn chat_created_with_a_model() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "model": "claude-x"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let chat: Chat = json_body(response).await;
        assert_eq!(chat.model.as_deref(), Some("claude-x"));
    }

    #[tokio::test]
    async fn chat_created_with_empty_model_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/chats")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"workspace_dir": "/tmp/ws", "model": ""}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = json_body(response).await;
        assert_eq!(info.kind, "bad_request");
    }

    /// PATCH a chat's model with a raw JSON body, returning the response.
    async fn patch_chat(
        router: &Router,
        bearer: &str,
        chat: ChatId,
        body: serde_json::Value,
    ) -> axum::response::Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/chats/{chat}"))
                    .header(header::AUTHORIZATION, bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn patch_chat_sets_and_clears_the_model() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;
        assert_eq!(chat.model, None);

        let set = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({"model": "m1"}),
        )
        .await;
        assert_eq!(set.status(), StatusCode::OK);
        assert_eq!(json_body::<Chat>(set).await.model.as_deref(), Some("m1"));

        let cleared = patch_chat(
            &router,
            &bearer,
            chat.id,
            serde_json::json!({"model": null}),
        )
        .await;
        assert_eq!(cleared.status(), StatusCode::OK);
        assert_eq!(json_body::<Chat>(cleared).await.model, None);
    }

    #[tokio::test]
    async fn patch_chat_rejects_empty_model_and_unknown_chat() {
        let (router, token, _store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        let empty = patch_chat(&router, &bearer, chat.id, serde_json::json!({"model": ""})).await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        let missing = patch_chat(
            &router,
            &bearer,
            ChatId::new(),
            serde_json::json!({"model": "m"}),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
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
        assert!(models.iter().any(|m| m["provider"] == "openai"));
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
            },
        )
        .await
        .unwrap();

        let resolver = resolver::KeyedResolver::new(store.clone(), secrets.clone());
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
            },
        )
        .await
        .unwrap();
        assert_eq!(resolver.resolve().await.id().0, "unconfigured");
    }

    #[tokio::test]
    async fn resolver_includes_openai_when_enabled() {
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
            &providers::ProviderCredential::api_key("sk-openai"),
        )
        .await
        .unwrap();
        providers::write_config(
            &*store,
            providers::ProviderKind::Openai,
            &providers::ProviderConfig {
                enabled: true,
                base_url: None,
            },
        )
        .await
        .unwrap();

        let routes = providers::collect_routes(&*store, &*secrets).await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].kind, openwave_router::RouteKind::Openai);

        let resolver = resolver::KeyedResolver::new(store, secrets);
        let provider = resolver.resolve().await;
        assert_eq!(provider.id().0, "router");

        // A curated openai model is selectable; an anthropic model is not
        // (no anthropic route, no openai_compatible fallback).
        let router = openwave_router::Router::build(routes);
        assert_eq!(
            router.select("gpt-4o"),
            Some(openwave_router::RouteKind::Openai)
        );
        assert_eq!(router.select("claude-opus-4-8"), None);
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
            },
        )
        .await
        .unwrap();

        let routes = providers::collect_routes(&*store, &*secrets).await;
        let router = openwave_router::Router::build(routes);
        assert_eq!(
            router.select("llama-3-local"),
            Some(openwave_router::RouteKind::OpenaiCompatible)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn configured_model_is_used_for_the_turn() {
        let recorder = RecordingProvider::default();
        let (router, token, store, _dir) = test_app_with(Arc::new(recorder.clone())).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        // Configure the model, then run a turn.
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/settings")
                    .header(header::AUTHORIZATION, &bearer)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"model": "claude-configured"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

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
                .any(|m| m == "claude-configured"),
            "the turn should run against the configured model"
        );
    }

    #[tokio::test]
    async fn relative_workspace_dir_is_rejected() {
        let (router, token, _store, _dir) = test_app().await;
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
        let (router, token, _store, _dir) = test_app().await;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn post_message_runs_a_turn_and_journals_its_events() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hello").await,
            StatusCode::ACCEPTED
        );

        let events = wait_for_turn(&store, chat.id).await;
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
        assert!(events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::TurnCompleted { .. })));
    }

    #[tokio::test]
    async fn message_to_unknown_chat_is_404() {
        let (router, token, _store, _dir) = test_app().await;
        assert_eq!(
            send_message(&router, &format!("Bearer {token}"), ChatId::new(), "hi").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_turn_on_the_same_chat_is_refused() {
        // A gated provider keeps the first turn active (blocked on the gate) while
        // we submit a second one, which must be refused with 409.
        let gate = Arc::new(Notify::new());
        let (router, token, _store, _dir) =
            test_app_with(Arc::new(GatedProvider { gate: gate.clone() })).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        // The handler claims the chat's slot synchronously before returning, so by
        // the time this 202 is observed the turn is holding the slot.
        assert_eq!(
            send_message(&router, &bearer, chat.id, "one").await,
            StatusCode::ACCEPTED
        );
        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::CONFLICT
        );

        // Release the first turn so it can finish and free the slot.
        gate.notify_one();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slot_frees_after_a_turn_completes() {
        let (router, token, store, _dir) = test_app().await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "one").await,
            StatusCode::ACCEPTED
        );
        wait_for_turn(&store, chat.id).await;

        // The turn finished, so its slot is released and a follow-up is accepted.
        assert_eq!(
            send_message(&router, &bearer, chat.id, "two").await,
            StatusCode::ACCEPTED
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn turn_fails_closed_with_no_provider_configured() {
        // The unconfigured provider errors without any network call; the turn must
        // end in TurnFailed, not hang or egress.
        let (router, token, store, _dir) =
            test_app_with(Arc::new(crate::provider::UnconfiguredProvider)).await;
        let bearer = format!("Bearer {token}");
        let chat = make_chat(&router, &bearer).await;

        assert_eq!(
            send_message(&router, &bearer, chat.id, "hello").await,
            StatusCode::ACCEPTED
        );
        let events = wait_for_turn(&store, chat.id).await;
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnFailed { .. }
        ));
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
        let (router, token, _store, _dir) = test_app().await;
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

    // --- WebSocket event stream ---

    use std::net::SocketAddr;

    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    /// Serve a router (with the given provider) over a real loopback socket.
    async fn serve_app_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (SocketAddr, Arc<str>, Arc<dyn Store>, tempfile::TempDir) {
        let (router, token, store, dir) = test_app_with(provider).await;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (addr, token, store, dir)
    }

    async fn make_chat_http(client: &reqwest::Client, addr: SocketAddr, token: &str) -> Chat {
        client
            .post(format!("http://{addr}/chats"))
            .bearer_auth(token)
            .json(&serde_json::json!({"workspace_dir": "/tmp/ws"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn send_message_http(
        client: &reqwest::Client,
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
    ) {
        let response = client
            .post(format!("http://{addr}/chats/{chat}/messages"))
            .bearer_auth(token)
            .json(&serde_json::json!({"content": "hi"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    }

    /// Connect to a chat's event socket (authenticated) and read frames until
    /// `want` turns have ended (or a timeout), returning the decoded events in
    /// arrival order.
    async fn read_until_turns_end(
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
        after: i64,
        want: usize,
    ) -> Vec<SequencedEvent> {
        let mut request = format!("ws://{addr}/chats/{chat}/events?after={after}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {token}").parse().unwrap());
        let (mut socket, _response) = connect_async(request).await.unwrap();

        let mut events = Vec::new();
        let mut completed = 0;
        let read = async {
            while let Some(frame) = socket.next().await {
                let WsMessage::Text(text) = frame.unwrap() else {
                    continue;
                };
                let event: SequencedEvent = serde_json::from_str(text.as_str()).unwrap();
                if matches!(
                    event.event,
                    AgentEvent::TurnCompleted { .. } | AgentEvent::TurnFailed { .. }
                ) {
                    completed += 1;
                }
                events.push(event);
                if completed >= want {
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .expect("turns did not complete over the socket");
        events
    }

    /// Read one turn's worth of events over a fresh connection.
    async fn read_until_turn_end(
        addr: SocketAddr,
        token: &str,
        chat: ChatId,
        after: i64,
    ) -> Vec<SequencedEvent> {
        read_until_turns_end(addr, token, chat, after, 1).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_replays_a_finished_turn_from_the_journal() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Run the turn to completion, then connect — everything comes from replay.
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        let events = read_until_turn_end(addr, &token, chat.id, 0).await;
        assert_eq!(events.first().unwrap().seq, 1, "replay starts at seq 1");
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(events
            .iter()
            .any(|e| matches!(&e.event, AgentEvent::TextDelta { text } if text == "hi")));
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnCompleted { .. }
        ));
        // Sequence numbers are strictly increasing.
        assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_streams_a_turn_started_after_connecting() {
        let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Connect first (journal empty), then trigger the turn — events arrive live.
        let reader = {
            let token = token.clone();
            tokio::spawn(async move { read_until_turn_end(addr, &token, chat.id, 0).await })
        };
        send_message_http(&client, addr, &token, chat.id).await;

        let events = reader.await.unwrap();
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert!(matches!(
            events.last().unwrap().event,
            AgentEvent::TurnCompleted { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_after_cursor_replays_only_newer_events() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        // Resume after seq 1: the first replayed event must be seq 2, and seq 1 is
        // not re-sent.
        let events = read_until_turn_end(addr, &token, chat.id, 1).await;
        assert_eq!(events.first().unwrap().seq, 2);
        assert!(events.iter().all(|e| e.seq > 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_replays_one_turn_then_streams_the_next_live() {
        let (addr, token, store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;

        // Turn 1 runs to completion and is journaled.
        send_message_http(&client, addr, &token, chat.id).await;
        wait_for_turn(&store, chat.id).await;

        // Connect (replays turn 1) and keep reading; then run turn 2, whose events
        // arrive live on the same connection. Assert both turns come through in
        // one gap-free, duplicate-free, strictly-increasing stream.
        let reader = {
            let token = token.clone();
            tokio::spawn(async move { read_until_turns_end(addr, &token, chat.id, 0, 2).await })
        };
        // Let the reader connect, subscribe, and drain the replay before turn 2.
        tokio::time::sleep(Duration::from_millis(100)).await;
        send_message_http(&client, addr, &token, chat.id).await;

        let events = reader.await.unwrap();
        assert!(matches!(events[0].event, AgentEvent::TurnStarted { .. }));
        assert_eq!(events[0].seq, 1);
        assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, AgentEvent::TurnCompleted { .. }))
                .count(),
            2,
            "both turns completed over one connection"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_bad_after_cursor_is_a_json_400() {
        let (addr, token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let client = reqwest::Client::new();
        let chat = make_chat_http(&client, addr, &token).await;
        // A non-integer `after` fails extraction; it must answer the API-wide
        // `{ kind, message }` JSON, not axum's plain-text rejection.
        let response = client
            .get(format!("http://{addr}/chats/{}/events?after=abc", chat.id))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let info: AgentErrorInfo = response.json().await.unwrap();
        assert_eq!(info.kind, "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ws_without_a_token_is_rejected() {
        let (addr, _token, _store, _dir) = serve_app_with(Arc::new(FakeProvider)).await;
        let chat = ChatId::new();
        let request = format!("ws://{addr}/chats/{chat}/events")
            .into_client_request()
            .unwrap();
        // No Authorization header: the handshake must fail (auth runs before upgrade).
        assert!(connect_async(request).await.is_err());
    }
}
