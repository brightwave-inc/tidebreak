//! The gateway-backed [`SandboxProvisioner`].
//!
//! Transport only, against the routes pinned in the module docs. The URL
//! join keeps a gateway deployed under a subpath working, the same way
//! `connectors::gateway` joins. Per-request timeouts differ by call: spawn
//! runs the environment's whole preflight (repository `ls-remote` included),
//! and an events read may deliberately hold up to 25 seconds.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tidebreak_core::OwnerId;

use super::wire::{
    EventCursor, MessageReceipt, RuntimeErrorBody, SandboxEvents, SandboxLease, SandboxMessage,
    SandboxStatus, SpawnArguments, EVENTS_MAX_WAIT_SECONDS,
};
use super::{RemoteSandboxError, RuntimeTokenSource, SandboxProvisioner};

/// Ceiling on a spawn request: preflight resolves every declared
/// repository against its remote before anything commits.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(120);
/// Ceiling on status, send, and cancel.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Slack an events read gets beyond its own held wait.
const EVENTS_TIMEOUT_SLACK: Duration = Duration::from_secs(15);

/// The provisioning client for a gateway-confined sandbox.
pub struct GatewayProvisioner {
    base_url: reqwest::Url,
    endpoint_slug: String,
    tokens: Arc<dyn RuntimeTokenSource>,
    http: reqwest::Client,
}

impl GatewayProvisioner {
    /// Builds a client against one gateway deployment and runtime endpoint.
    pub fn new(
        base_url: &str,
        endpoint_slug: &str,
        tokens: Arc<dyn RuntimeTokenSource>,
    ) -> Result<Self, RemoteSandboxError> {
        let mut base = base_url.to_owned();
        if !base.ends_with('/') {
            base.push('/');
        }
        let base_url = reqwest::Url::parse(&base).map_err(|error| {
            RemoteSandboxError::InvalidRequest(format!("gateway base URL is unusable: {error}"))
        })?;
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| RemoteSandboxError::Unavailable {
                operation: "client",
                detail: error.to_string(),
            })?;
        Ok(Self {
            base_url,
            endpoint_slug: endpoint_slug.to_owned(),
            tokens,
            http,
        })
    }

    /// Joins below the configured base so a subpath deployment keeps its
    /// prefix.
    fn endpoint(&self, path: &str) -> Result<reqwest::Url, RemoteSandboxError> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| {
                RemoteSandboxError::InvalidRequest(format!("could not build endpoint URL: {error}"))
            })
    }

    /// Sends one prepared request and maps the outcome.
    async fn dispatch(
        &self,
        operation: &'static str,
        request: reqwest::RequestBuilder,
        owner: &OwnerId,
    ) -> Result<reqwest::Response, RemoteSandboxError> {
        let token = self.tokens.runtime_token(owner).await?;
        let response = request
            .bearer_auth(&token.secret)
            .send()
            .await
            .map_err(|error| RemoteSandboxError::Unavailable {
                operation,
                detail: error.without_url().to_string(),
            })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body: RuntimeErrorBody = response.json().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(RemoteSandboxError::SignInRequired(
                if body.error_description.is_empty() {
                    format!("HTTP status {}", status.as_u16())
                } else {
                    body.error_description
                },
            ));
        }
        if status.is_client_error() {
            return Err(RemoteSandboxError::Refused {
                operation,
                code: if body.error.is_empty() {
                    format!("http_{}", status.as_u16())
                } else {
                    body.error
                },
                message: body.error_description,
            });
        }
        Err(RemoteSandboxError::Unavailable {
            operation,
            detail: format!("HTTP status {}", status.as_u16()),
        })
    }

    /// Decodes a JSON body, naming the operation on failure.
    async fn decode<T: for<'de> serde::Deserialize<'de>>(
        operation: &'static str,
        response: reqwest::Response,
    ) -> Result<T, RemoteSandboxError> {
        response
            .json()
            .await
            .map_err(|_| RemoteSandboxError::Unavailable {
                operation,
                detail: "invalid response body".to_owned(),
            })
    }
}

#[async_trait]
impl SandboxProvisioner for GatewayProvisioner {
    async fn spawn(
        &self,
        owner: &OwnerId,
        arguments: &SpawnArguments,
    ) -> Result<SandboxLease, RemoteSandboxError> {
        let url = self.endpoint(&format!(
            "api/v1/runtime/endpoints/{}/sandboxes",
            self.endpoint_slug
        ))?;
        let request = self
            .http
            .post(url)
            .timeout(SPAWN_TIMEOUT)
            .json(&serde_json::json!({ "arguments": arguments }));
        let response = self.dispatch("spawn", request, owner).await?;
        Self::decode("spawn", response).await
    }

    async fn status(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
    ) -> Result<SandboxStatus, RemoteSandboxError> {
        let url = self.endpoint(&format!("api/v1/runtime/sandboxes/{sandbox_id}"))?;
        let request = self.http.get(url).timeout(CALL_TIMEOUT);
        let response = self.dispatch("status", request, owner).await?;
        Self::decode("status", response).await
    }

    async fn events(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
        cursor: EventCursor,
    ) -> Result<SandboxEvents, RemoteSandboxError> {
        let wait = cursor
            .wait_seconds
            .unwrap_or(0)
            .min(EVENTS_MAX_WAIT_SECONDS);
        let url = self.endpoint(&format!("api/v1/runtime/sandboxes/{sandbox_id}/events"))?;
        let request = self
            .http
            .get(url)
            .timeout(Duration::from_secs(u64::from(wait)) + EVENTS_TIMEOUT_SLACK)
            .query(&EventCursor {
                wait_seconds: (wait > 0).then_some(wait),
                ..cursor
            });
        let response = self.dispatch("events", request, owner).await?;
        Self::decode("events", response).await
    }

    async fn send(
        &self,
        owner: &OwnerId,
        sandbox_id: &str,
        message: &SandboxMessage,
    ) -> Result<MessageReceipt, RemoteSandboxError> {
        message
            .validate()
            .map_err(RemoteSandboxError::InvalidRequest)?;
        let url = self.endpoint(&format!("api/v1/runtime/sandboxes/{sandbox_id}/messages"))?;
        let request = self.http.post(url).timeout(CALL_TIMEOUT).json(message);
        let response = self.dispatch("send", request, owner).await?;
        Self::decode("send", response).await
    }

    async fn cancel(&self, owner: &OwnerId, sandbox_id: &str) -> Result<(), RemoteSandboxError> {
        let url = self.endpoint(&format!("api/v1/runtime/sandboxes/{sandbox_id}/cancel"))?;
        let request = self.http.post(url).timeout(CALL_TIMEOUT);
        self.dispatch("cancel", request, owner).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{json, Value};

    use super::super::wire::SandboxState;
    use super::super::RuntimeToken;
    use super::*;

    /// One captured request: the bearer presented and the JSON body, if any.
    #[derive(Clone, Debug)]
    struct Captured {
        bearer: Option<String>,
        body: Value,
        query: HashMap<String, String>,
    }

    /// A fake runtime surface that records what it was asked and answers
    /// from a canned script.
    #[derive(Default)]
    struct FakeRuntime {
        captured: Mutex<Vec<Captured>>,
        requests: AtomicUsize,
        /// Status code and body the next requests answer with; empty means
        /// the happy path.
        refusal: Option<(StatusCode, Value)>,
    }

    impl FakeRuntime {
        fn capture(&self, headers: &HeaderMap, body: Value, query: HashMap<String, String>) {
            self.requests.fetch_add(1, Ordering::SeqCst);
            self.captured.lock().unwrap().push(Captured {
                bearer: headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .map(str::to_owned),
                body,
                query,
            });
        }

        fn scripted(&self) -> Option<(StatusCode, Json<Value>)> {
            self.refusal
                .as_ref()
                .map(|(status, body)| (*status, Json(body.clone())))
        }
    }

    async fn spawn_route(
        State(runtime): State<Arc<FakeRuntime>>,
        Path(slug): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        runtime.capture(
            &headers,
            json!({ "slug": slug, "body": body }),
            HashMap::new(),
        );
        if let Some(scripted) = runtime.scripted() {
            return scripted;
        }
        (
            StatusCode::OK,
            Json(json!({
                "sandbox_id": "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f",
                "execution_id": "ignored",
                "execution_mode": "buffered",
                "state": "pending",
                "latest_event_seq": 0,
                "expires_in_seconds": 3600,
            })),
        )
    }

    async fn status_route(
        State(runtime): State<Arc<FakeRuntime>>,
        Path(sandbox_id): Path<String>,
        headers: HeaderMap,
    ) -> (StatusCode, Json<Value>) {
        runtime.capture(
            &headers,
            json!({ "sandbox_id": sandbox_id }),
            HashMap::new(),
        );
        if let Some(scripted) = runtime.scripted() {
            return scripted;
        }
        (
            StatusCode::OK,
            Json(json!({
                "sandbox_id": sandbox_id,
                "state": "running",
                "latest_event_seq": 12,
                "pending_messages": 1,
                "possibly_stalled": false,
            })),
        )
    }

    async fn events_route(
        State(runtime): State<Arc<FakeRuntime>>,
        Path(sandbox_id): Path<String>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        runtime.capture(&headers, json!({ "sandbox_id": sandbox_id }), query);
        Json(json!({
            "sandbox_id": sandbox_id,
            "state": "running",
            "latest_event_seq": 4,
            "events": [
                { "seq": 3, "kind": "turn_started", "payload": { "turn": 2 }, "created_at": "2026-08-27T00:00:00Z" },
                { "seq": 4, "kind": "kind_from_the_future", "payload": {}, "created_at": "2026-08-27T00:00:01Z" },
            ],
        }))
    }

    async fn messages_route(
        State(runtime): State<Arc<FakeRuntime>>,
        Path(sandbox_id): Path<String>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (StatusCode, Json<Value>) {
        runtime.capture(&headers, body, HashMap::new());
        if let Some(scripted) = runtime.scripted() {
            return scripted;
        }
        (
            StatusCode::OK,
            Json(json!({
                "sandbox_id": sandbox_id,
                "seq": 7,
                "interrupt": false,
                "pending_messages": 0,
            })),
        )
    }

    async fn cancel_route(
        State(runtime): State<Arc<FakeRuntime>>,
        Path(sandbox_id): Path<String>,
        headers: HeaderMap,
    ) -> StatusCode {
        runtime.capture(
            &headers,
            json!({ "sandbox_id": sandbox_id }),
            HashMap::new(),
        );
        StatusCode::NO_CONTENT
    }

    async fn serve(runtime: Arc<FakeRuntime>) -> String {
        let app = Router::new()
            .route(
                "/api/v1/runtime/endpoints/{slug}/sandboxes",
                post(spawn_route),
            )
            .route("/api/v1/runtime/sandboxes/{sandbox_id}", get(status_route))
            .route(
                "/api/v1/runtime/sandboxes/{sandbox_id}/events",
                get(events_route),
            )
            .route(
                "/api/v1/runtime/sandboxes/{sandbox_id}/messages",
                post(messages_route),
            )
            .route(
                "/api/v1/runtime/sandboxes/{sandbox_id}/cancel",
                post(cancel_route),
            )
            .with_state(runtime);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}")
    }

    /// Hands out one static token, the way tests inject credentials.
    struct StaticTokens;

    #[async_trait]
    impl RuntimeTokenSource for StaticTokens {
        async fn runtime_token(&self, _: &OwnerId) -> Result<RuntimeToken, RemoteSandboxError> {
            Ok(RuntimeToken {
                secret: "mg_at_test".to_owned(),
            })
        }
    }

    fn owner() -> OwnerId {
        OwnerId::new("person-1").unwrap()
    }

    async fn client(runtime: Arc<FakeRuntime>) -> GatewayProvisioner {
        let base = serve(runtime).await;
        GatewayProvisioner::new(&base, "primary", Arc::new(StaticTokens)).unwrap()
    }

    #[tokio::test]
    async fn spawn_presents_the_bearer_on_the_endpoint_path_and_reads_the_lease() {
        let runtime = Arc::new(FakeRuntime::default());
        let provisioner = client(runtime.clone()).await;
        let lease = provisioner
            .spawn(
                &owner(),
                &SpawnArguments {
                    profile: "default".to_owned(),
                    harness: "claude_code".to_owned(),
                    task: "fix the flaky test".to_owned(),
                    repository: Some("https://github.com/org/repo".to_owned()),
                    ..SpawnArguments::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(lease.sandbox_id, "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f");
        assert_eq!(lease.state, SandboxState::Pending);
        assert_eq!(lease.expires_in_seconds, 3600);

        let captured = runtime.captured.lock().unwrap();
        assert_eq!(captured[0].bearer.as_deref(), Some("mg_at_test"));
        assert_eq!(captured[0].body["slug"], json!("primary"));
        let arguments = &captured[0].body["body"]["arguments"];
        assert_eq!(arguments["harness"], json!("claude_code"));
        // Absent options must be omitted, not null: the spawn body rejects
        // unknown or malformed fields.
        assert!(arguments.get("model").is_none());
        assert!(arguments.get("repositories").is_none());
    }

    #[tokio::test]
    async fn status_reads_the_fields_the_session_acts_on() {
        let runtime = Arc::new(FakeRuntime::default());
        let provisioner = client(runtime).await;
        let status = provisioner
            .status(&owner(), "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f")
            .await
            .unwrap();
        assert_eq!(status.sandbox_id, "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f");
        assert_eq!(status.state, SandboxState::Running);
        assert!(!status.state.is_terminal());
        assert_eq!(status.latest_event_seq, 12);
        assert_eq!(status.pending_messages, 1);
        assert!(!status.possibly_stalled);
        // Lenient decode: everything the fake omitted reads as absent.
        assert_eq!(status.failure_reason, None);
        assert_eq!(status.termination_reason, None);
        assert_eq!(status.repository_url, None);
        assert_eq!(status.spend_microusd, None);
        assert_eq!(status.spend_ceiling_microusd, None);
        assert_eq!(status.completed_at, None);
    }

    #[tokio::test]
    async fn a_named_refusal_carries_the_environments_code_and_is_not_retryable() {
        let runtime = Arc::new(FakeRuntime {
            refusal: Some((
                StatusCode::BAD_REQUEST,
                json!({
                    "error": "repository_not_operable",
                    "error_description": "The GitHub App installation does not cover org/private.",
                }),
            )),
            ..FakeRuntime::default()
        });
        let provisioner = client(runtime).await;
        let error = provisioner
            .spawn(
                &owner(),
                &SpawnArguments {
                    profile: "default".to_owned(),
                    harness: "claude_code".to_owned(),
                    task: "task".to_owned(),
                    ..SpawnArguments::default()
                },
            )
            .await
            .unwrap_err();
        assert!(!error.is_retryable());
        match error {
            RemoteSandboxError::Refused { code, message, .. } => {
                assert_eq!(code, "repository_not_operable");
                assert!(message.contains("org/private"));
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_invalid_token_asks_for_sign_in() {
        let runtime = Arc::new(FakeRuntime {
            refusal: Some((
                StatusCode::UNAUTHORIZED,
                json!({
                    "error": "invalid_runtime_token",
                    "error_description": "A valid runtime access token is required.",
                }),
            )),
            ..FakeRuntime::default()
        });
        let provisioner = client(runtime).await;
        let error = provisioner
            .status(&owner(), "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f")
            .await
            .unwrap_err();
        assert!(matches!(error, RemoteSandboxError::SignInRequired(_)));
    }

    #[tokio::test]
    async fn a_server_fault_is_retryable() {
        let runtime = Arc::new(FakeRuntime {
            refusal: Some((StatusCode::BAD_GATEWAY, json!({}))),
            ..FakeRuntime::default()
        });
        let provisioner = client(runtime).await;
        let error = provisioner
            .status(&owner(), "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f")
            .await
            .unwrap_err();
        assert!(error.is_retryable());
    }

    #[tokio::test]
    async fn events_round_trip_the_cursor_and_clamp_the_wait() {
        let runtime = Arc::new(FakeRuntime::default());
        let provisioner = client(runtime.clone()).await;
        let events = provisioner
            .events(
                &owner(),
                "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f",
                EventCursor {
                    after_seq: Some(2),
                    limit: Some(100),
                    wait_seconds: Some(600),
                },
            )
            .await
            .unwrap();
        assert_eq!(events.latest_event_seq, 4);
        assert_eq!(events.sandbox_id, "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f");
        // State rides the events page so a poll loop need not also poll status.
        assert_eq!(events.state, SandboxState::Running);
        assert_eq!(events.events[0].seq, 3);
        assert_eq!(events.events[0].payload["turn"], json!(2));
        assert_eq!(events.events[0].created_at, "2026-08-27T00:00:00Z");
        assert_eq!(events.events.len(), 2);
        // Unknown kinds pass through untouched; interpretation is the
        // session runtime's job, not the transport's.
        assert_eq!(events.events[1].kind, "kind_from_the_future");

        let captured = runtime.captured.lock().unwrap();
        assert_eq!(captured[0].query.get("after_seq").unwrap(), "2");
        assert_eq!(captured[0].query.get("limit").unwrap(), "100");
        // 600 exceeds the environment's held-wait ceiling; the client clamps
        // rather than letting the server clamp with a shorter HTTP timeout
        // than the wait it asked for.
        assert_eq!(captured[0].query.get("wait_seconds").unwrap(), "25");
    }

    #[tokio::test]
    async fn an_empty_message_never_reaches_the_environment() {
        let runtime = Arc::new(FakeRuntime::default());
        let provisioner = client(runtime.clone()).await;
        let error = provisioner
            .send(
                &owner(),
                "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f",
                &SandboxMessage {
                    body: "  ".to_owned(),
                    interrupt: false,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RemoteSandboxError::InvalidRequest(_)));
        assert_eq!(runtime.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_reads_the_receipt_and_cancel_accepts_no_content() {
        let runtime = Arc::new(FakeRuntime::default());
        let provisioner = client(runtime.clone()).await;
        let sandbox = "6b8e7f2c-1d0a-4b3c-9e5f-2a1b3c4d5e6f";
        let receipt = provisioner
            .send(
                &owner(),
                sandbox,
                &SandboxMessage {
                    body: "also check the retry path".to_owned(),
                    interrupt: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(receipt.seq, 7);
        assert!(!receipt.interrupt);
        assert_eq!(receipt.pending_messages, 0);
        provisioner.cancel(&owner(), sandbox).await.unwrap();
        let captured = runtime.captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(captured[0].body.get("interrupt").is_none());
    }

    #[tokio::test]
    async fn the_token_never_appears_in_debug_output() {
        let token = RuntimeToken {
            secret: "mg_at_secret".to_owned(),
        };
        assert!(!format!("{token:?}").contains("mg_at_secret"));
    }
}
