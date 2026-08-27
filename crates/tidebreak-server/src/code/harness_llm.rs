//! Session-scoped inference relay for engine children (decision 71).
//!
//! A gateway-authenticated hosted machine holds no provider credentials an
//! engine child could use: the image carries no `ANTHROPIC_API_KEY`, no
//! Codex login, and decision 51's on-behalf-of exchange lives in this
//! process, never in the child. Left alone, a child falls back to its
//! vendor endpoint and fails the first turn with a 401.
//!
//! This module gives each session a private inference path instead. At
//! spawn, the runtime mints an opaque relay key and points the child's own
//! HTTP client at `{loopback_base}/code/llm/...` on this server's listener
//! ([`spawn_wiring`]). Each relayed request presents that key; the relay
//! maps it back to the owning caller, exchanges the caller's live
//! machine-bound token for a fresh inference token, and streams the request
//! through to the gateway's compat endpoint with that token in place of the
//! key. Because the exchange runs per upstream request, a session outlives
//! any single short-lived token.
//!
//! Security properties mirror the browser channel
//! ([`super::browser_channel`]):
//!
//! * Keys are random v4 UUIDs, never derived from owner or session ids.
//! * A key authorizes exactly the relay routes — it is not the app token
//!   and opens nothing else on the listener.
//! * Reissue for the same session replaces the prior key, and revocation
//!   follows the browser channel's lifecycle.
//! * The caller's gateway tokens never reach the child. The relay key is
//!   the only secret in the child's environment, and it is useless off
//!   this machine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::Response;

use tidebreak_core::{AgentError, CodeSessionId, HarnessKind, OwnerId};

use crate::obo_gateway::{GatewayCompatModel, OboGateway};

/// Whose inference a relay key spends.
#[derive(Clone)]
pub(crate) struct HarnessLlmSubject {
    pub(crate) owner: OwnerId,
    pub(crate) session: CodeSessionId,
}

/// The gateway compat endpoint one relay route forwards to.
#[derive(Clone, Copy)]
pub(crate) enum RelayEndpoint {
    /// Anthropic Messages, spoken by Claude Code.
    AnthropicMessages,
    /// OpenAI Responses, spoken by Codex.
    OpenAiResponses,
    /// The OpenAI-compatible model listing, prefetched by Grok's custom
    /// models endpoint.
    OpenAiModels,
}

impl RelayEndpoint {
    fn upstream_path(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "/compat/anthropic/v1/messages",
            Self::OpenAiResponses => "/compat/openai/v1/responses",
            Self::OpenAiModels => "/compat/openai/v1/models",
        }
    }

    /// A vendor-shaped error the engine's own client reports legibly,
    /// instead of a server shape it would print as opaque JSON.
    pub(crate) fn error_response(self, status: StatusCode, kind: &str, message: &str) -> Response {
        let body = match self {
            Self::AnthropicMessages => serde_json::json!({
                "type": "error",
                "error": { "type": kind, "message": message },
            }),
            Self::OpenAiResponses | Self::OpenAiModels => serde_json::json!({
                "error": { "type": kind, "message": message, "param": null, "code": null },
            }),
        };
        Response::builder()
            .status(status)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .expect("a constant error response builds")
    }
}

/// In-memory key registry plus the forwarding client.
pub(crate) struct HarnessLlmRelay {
    obo: Arc<OboGateway>,
    client: reqwest::Client,
    state: Mutex<RelayState>,
}

#[derive(Default)]
struct RelayState {
    by_session: HashMap<CodeSessionId, String>,
    keys: HashMap<String, HarnessLlmSubject>,
}

impl HarnessLlmRelay {
    pub(crate) fn new(obo: Arc<OboGateway>) -> Self {
        let client = reqwest::Client::builder()
            // Only the dial is bounded. An inference stream legitimately
            // runs for minutes, so a total request timeout would kill long
            // turns mid-stream.
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("the relay HTTP client builds from constant configuration");
        Self {
            obo,
            client,
            state: Mutex::new(RelayState::default()),
        }
    }

    /// Both of the caller's compat listings. Keeping the read behind the
    /// relay means hosted pickers and the turns they start always answer to
    /// the same gateway deployment.
    pub(crate) async fn listings(
        &self,
        owner: &OwnerId,
    ) -> tidebreak_core::Result<(
        tidebreak_core::Result<Vec<GatewayCompatModel>>,
        tidebreak_core::Result<Vec<GatewayCompatModel>>,
    )> {
        self.obo.compat_listings(owner).await
    }

    /// Mint the relay key for `subject`, replacing any prior key the same
    /// session held. Reissue-on-attach is what keeps a reaped or relaunched
    /// worker from extending the life of a key an old child still knows.
    pub(crate) fn issue(&self, subject: HarnessLlmSubject) -> String {
        let key = generate_key();
        let mut state = self.state.lock().expect("harness llm registry");
        if let Some(old) = state.by_session.insert(subject.session, key.clone()) {
            state.keys.remove(&old);
        }
        state.keys.insert(key.clone(), subject);
        key
    }

    /// Revoke the key for `session_id`. Idempotent.
    pub(crate) fn revoke(&self, session_id: CodeSessionId) {
        let mut state = self.state.lock().expect("harness llm registry");
        if let Some(key) = state.by_session.remove(&session_id) {
            state.keys.remove(&key);
        }
    }

    fn subject_for_key(&self, key: &str) -> Option<HarnessLlmSubject> {
        self.state
            .lock()
            .expect("harness llm registry")
            .keys
            .get(key)
            .cloned()
    }

    /// Authenticate one relayed request and exchange the caller's live
    /// machine-bound token for a fresh inference token.
    ///
    /// Refusals are 401 with the vendor's authentication shape so the
    /// engine fails the turn fast instead of retrying; a gateway that does
    /// not answer is 502 so the engine's own retry policy applies.
    async fn exchange(
        &self,
        endpoint: RelayEndpoint,
        headers: &HeaderMap,
    ) -> Result<String, Response> {
        let Some(key) = relay_key(headers) else {
            return Err(endpoint.error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "missing harness relay key",
            ));
        };
        let Some(subject) = self.subject_for_key(key) else {
            return Err(endpoint.error_response(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "unknown or revoked harness relay key",
            ));
        };
        match self.obo.bearer_for(&subject.owner).await {
            Ok(token) => Ok(token),
            Err(error @ (AgentError::SignInRequired(_) | AgentError::InvalidTarget(_))) => {
                Err(endpoint.error_response(
                    StatusCode::UNAUTHORIZED,
                    "authentication_error",
                    &error.to_string(),
                ))
            }
            Err(error) => Err(endpoint.error_response(
                StatusCode::BAD_GATEWAY,
                "api_error",
                &format!("the Model Gateway did not answer the token exchange: {error}"),
            )),
        }
    }

    /// Stream one relayed inference request through to the gateway's compat
    /// endpoint with a fresh on-behalf-of token.
    pub(crate) async fn forward(
        &self,
        endpoint: RelayEndpoint,
        headers: &HeaderMap,
        query: Option<String>,
        body: axum::body::Body,
    ) -> Response {
        let token = match self.exchange(endpoint, headers).await {
            Ok(token) => token,
            Err(response) => return response,
        };

        let mut url = format!(
            "{}{}",
            self.obo.gateway_base_url(),
            endpoint.upstream_path()
        );
        if let Some(query) = query.filter(|query| !query.is_empty()) {
            url.push('?');
            url.push_str(&query);
        }
        let mut request = self
            .client
            .post(&url)
            .body(reqwest::Body::wrap_stream(body.into_data_stream()));
        for (name, value) in headers {
            if forwardable_request_header(name) {
                request = request.header(name, value);
            }
        }
        request = request.header(AUTHORIZATION, format!("Bearer {token}"));

        let upstream = match request.send().await {
            Ok(upstream) => upstream,
            Err(error) => {
                return endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("the Model Gateway is unreachable: {error}"),
                );
            }
        };
        let mut response = Response::builder().status(upstream.status());
        if let Some(headers_mut) = response.headers_mut() {
            for (name, value) in upstream.headers() {
                if forwardable_response_header(name) {
                    headers_mut.append(name.clone(), value.clone());
                }
            }
        }
        response
            .body(axum::body::Body::from_stream(upstream.bytes_stream()))
            .unwrap_or_else(|error| {
                endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("could not stream the gateway response: {error}"),
                )
            })
    }

    /// Relay the gateway's OpenAI-compatible model listing, tagging each
    /// row with the backend this relay serves it through.
    ///
    /// Grok defaults a model prefetched from a custom models endpoint to
    /// its chat-completions backend, which neither this relay nor the
    /// gateway speaks; the `api_backend` row member is the extension it
    /// honors to pick the Responses backend instead. Every other client
    /// ignores unknown members — the gateway's own listing already carries
    /// additive fields for the same reason. The listing is a bounded JSON
    /// document, so this buffers rather than streams.
    pub(crate) async fn forward_models(
        &self,
        endpoint: RelayEndpoint,
        headers: &HeaderMap,
    ) -> Response {
        let token = match self.exchange(endpoint, headers).await {
            Ok(token) => token,
            Err(response) => return response,
        };
        let url = format!(
            "{}{}",
            self.obo.gateway_base_url(),
            endpoint.upstream_path()
        );
        let upstream = match self
            .client
            .get(&url)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(upstream) => upstream,
            Err(error) => {
                return endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("the Model Gateway is unreachable: {error}"),
                );
            }
        };
        let status = upstream.status();
        let body = match upstream.text().await {
            Ok(body) => body,
            Err(error) => {
                return endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("could not read the gateway model listing: {error}"),
                );
            }
        };
        if !status.is_success() {
            return Response::builder()
                .status(status)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(body))
                .unwrap_or_else(|error| {
                    endpoint.error_response(
                        StatusCode::BAD_GATEWAY,
                        "api_error",
                        &format!("could not relay the gateway model listing: {error}"),
                    )
                });
        }
        let mut listing: serde_json::Value = match serde_json::from_str(&body) {
            Ok(listing) => listing,
            Err(error) => {
                return endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("the gateway model listing is not valid JSON: {error}"),
                );
            }
        };
        let Some(rows) = listing.get_mut("data").and_then(|data| data.as_array_mut()) else {
            return endpoint.error_response(
                StatusCode::BAD_GATEWAY,
                "api_error",
                "the gateway model listing has no data array",
            );
        };
        for row in rows {
            if let Some(object) = row.as_object_mut() {
                object.insert("api_backend".to_owned(), "responses".into());
            }
        }
        Response::builder()
            .status(status)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(listing.to_string()))
            .unwrap_or_else(|error| {
                endpoint.error_response(
                    StatusCode::BAD_GATEWAY,
                    "api_error",
                    &format!("could not relay the gateway model listing: {error}"),
                )
            })
    }
}

/// The argv and environment that point one engine child at the relay.
///
/// Claude Code takes a base URL and bearer through its standard variables.
/// Codex takes a custom model provider on the command line; the custom
/// provider also keeps it off its websocket transport, whose vendor-only
/// endpoint produced the reconnect noise hosted sessions logged. Opencode
/// takes a provider override through its JSON config environment variable —
/// one entry per protocol the relay serves, so any session model under
/// those providers rides the caller's grant. Grok takes a custom models
/// endpoint plus the relay key itself; its CLI reads credentials only from
/// an auth file, so the grok adapter materializes the key as a
/// session-scoped `GROK_AUTH_PATH` file and strips the variable from the
/// child's environment.
pub(crate) fn spawn_wiring(
    kind: HarnessKind,
    loopback_base: &str,
    key: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let base = loopback_base.trim_end_matches('/');
    match kind {
        HarnessKind::ClaudeCode => (
            Vec::new(),
            vec![
                (
                    "ANTHROPIC_BASE_URL".into(),
                    format!("{base}/code/llm/anthropic"),
                ),
                ("ANTHROPIC_AUTH_TOKEN".into(), key.to_owned()),
            ],
        ),
        HarnessKind::Codex => (
            vec![
                "-c".into(),
                "model_provider=tidebreak".into(),
                "-c".into(),
                "model_providers.tidebreak.name=Tidebreak".into(),
                "-c".into(),
                format!("model_providers.tidebreak.base_url={base}/code/llm/openai/v1"),
                "-c".into(),
                "model_providers.tidebreak.env_key=TIDEBREAK_LLM_KEY".into(),
                "-c".into(),
                "model_providers.tidebreak.wire_api=responses".into(),
            ],
            vec![("TIDEBREAK_LLM_KEY".into(), key.to_owned())],
        ),
        HarnessKind::Opencode => (
            Vec::new(),
            vec![(
                "OPENCODE_CONFIG_CONTENT".into(),
                opencode_relay_config(base, key),
            )],
        ),
        HarnessKind::Grok => (
            Vec::new(),
            vec![
                ("TIDEBREAK_LLM_KEY".into(), key.to_owned()),
                (
                    "GROK_MODELS_BASE_URL".into(),
                    format!("{base}/code/llm/openai/v1"),
                ),
            ],
        ),
    }
}

/// Whether the on-behalf-of relay can carry this engine's inference.
///
/// Exactly the engines [`spawn_wiring`] points at the relay. The doctor reads
/// this on a gateway-hosted machine, where a covered engine needs no local
/// sign-in and an uncovered one cannot run at all; everywhere else the local
/// probe decides, and this answer does not matter.
pub(crate) fn relay_covered(kind: HarnessKind) -> bool {
    matches!(
        kind,
        HarnessKind::ClaudeCode | HarnessKind::Codex | HarnessKind::Opencode | HarnessKind::Grok
    )
}

/// Opencode's provider override: one entry per protocol the relay serves.
///
/// `OPENCODE_CONFIG_CONTENT` is the JSON config object the CLI merges over
/// its file config, and `options` is the base-URL and key surface every
/// catalog provider accepts. The Anthropic loader posts `{baseURL}/messages`
/// with the key as `x-api-key`; the OpenAI loader posts `{baseURL}/responses`
/// with the key as bearer. `model-gateway` is not a catalog provider — the
/// adapter maps vendor-neutral gateway model ids (deepseek, glm, kimi, ...)
/// to it — so the entry names the OpenAI loader itself: without `npm` the
/// CLI falls back to its OpenAI-compatible loader and posts
/// `{baseURL}/chat/completions`, which neither this relay nor the gateway
/// serves. Non-reasoning OpenAI models would also go to chat completions —
/// pinned against opencode 1.18.x.
fn opencode_relay_config(base: &str, key: &str) -> String {
    serde_json::json!({
        "provider": {
            "anthropic": {
                "options": {
                    "baseURL": format!("{base}/code/llm/anthropic/v1"),
                    "apiKey": key,
                },
            },
            "openai": {
                "options": {
                    "baseURL": format!("{base}/code/llm/openai/v1"),
                    "apiKey": key,
                },
            },
            "model-gateway": {
                "name": "Model Gateway",
                "npm": "@ai-sdk/openai",
                "options": {
                    "baseURL": format!("{base}/code/llm/openai/v1"),
                    "apiKey": key,
                },
            },
        },
    })
    .to_string()
}

fn generate_key() -> String {
    format!("tbreak_hl_{}", uuid::Uuid::new_v4())
}

/// The relay key on an inbound request. Claude Code presents its auth token
/// as a bearer; a client configured with an API key instead presents
/// `x-api-key`.
fn relay_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
}

/// Request headers the relay forwards upstream: everything except the
/// credentials it replaces and the framing headers its own client re-derives.
fn forwardable_request_header(name: &HeaderName) -> bool {
    !matches!(
        name.as_str(),
        "authorization"
            | "x-api-key"
            | "cookie"
            | "host"
            | "content-length"
            | "connection"
            | "transfer-encoding"
            | "upgrade"
            | "expect"
            | "keep-alive"
            | "te"
            | "trailer"
            | "proxy-authorization"
            | "proxy-connection"
    )
}

/// Response headers passed back to the child: everything except framing,
/// which this server's own stack re-derives for the streamed body.
fn forwardable_response_header(name: &HeaderName) -> bool {
    !matches!(
        name.as_str(),
        "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "trailer"
    )
}

#[cfg(test)]
mod tests {
    use axum::extract::RawQuery;
    use axum::response::IntoResponse as _;
    use axum::routing::post;
    use axum::Json;

    use super::*;

    /// The machine resource the fake exchange is asked for.
    const TEST_RESOURCE: &str =
        "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed";

    fn owner(name: &str) -> OwnerId {
        OwnerId::new(name).unwrap()
    }

    fn subject_for(name: &str) -> HarnessLlmSubject {
        HarnessLlmSubject {
            owner: owner(name),
            session: CodeSessionId::new(),
        }
    }

    async fn read_text(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    /// A gateway that mints one constant inference token and echoes what the
    /// compat endpoint received, so a test can assert on the relayed request
    /// through the relayed response.
    async fn fake_gateway() -> Arc<OboGateway> {
        async fn seen(headers: HeaderMap, RawQuery(query): RawQuery, body: String) -> Response {
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                format!(
                    "auth={} query={} body={body}",
                    headers
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default(),
                    query.unwrap_or_default(),
                ),
            )
                .into_response()
        }
        let app = axum::Router::new()
            .route(
                "/oauth/token",
                post(|| async {
                    Json(serde_json::json!({
                        "access_token": "mg_it_fresh",
                        "expires_in": 3600,
                        "token_type": "Bearer",
                    }))
                }),
            )
            .route(
                "/compat/openai/v1/models",
                axum::routing::get(|| async {
                    Json(serde_json::json!({
                        "object": "list",
                        "data": [
                            {"id": "glm-5.3", "object": "model", "created": 0, "owned_by": "model-gateway"},
                            {"id": "grok-4.5", "object": "model", "created": 0, "owned_by": "model-gateway"},
                        ]
                    }))
                }),
            )
            .route("/compat/anthropic/v1/messages", post(seen))
            .route("/compat/openai/v1/responses", post(seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Arc::new(OboGateway::new(&format!("http://{address}"), TEST_RESOURCE.to_owned()).unwrap())
    }

    fn bearer_headers(key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, format!("Bearer {key}").parse().unwrap());
        headers.insert("x-api-key", "should-never-forward".parse().unwrap());
        headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        headers
    }

    #[test]
    fn spawn_wiring_points_each_engine_at_the_relay() {
        let (argv, env) = spawn_wiring(HarnessKind::ClaudeCode, "http://0.0.0.0:8080/", "k1");
        assert!(argv.is_empty());
        assert_eq!(
            env,
            vec![
                (
                    "ANTHROPIC_BASE_URL".to_owned(),
                    "http://0.0.0.0:8080/code/llm/anthropic".to_owned()
                ),
                ("ANTHROPIC_AUTH_TOKEN".to_owned(), "k1".to_owned()),
            ]
        );

        let (argv, env) = spawn_wiring(HarnessKind::Codex, "http://0.0.0.0:8080", "k2");
        assert_eq!(env, vec![("TIDEBREAK_LLM_KEY".to_owned(), "k2".to_owned())]);
        assert_eq!(argv.len(), 10, "five -c pairs: {argv:?}");
        assert!(argv.contains(
            &"model_providers.tidebreak.base_url=http://0.0.0.0:8080/code/llm/openai/v1".to_owned()
        ));
        assert!(argv.contains(&"model_providers.tidebreak.wire_api=responses".to_owned()));
        assert!(argv.contains(&"model_provider=tidebreak".to_owned()));

        let (argv, env) = spawn_wiring(HarnessKind::Opencode, "http://0.0.0.0:8080", "k3");
        assert!(argv.is_empty());
        assert_eq!(env.len(), 1, "one config override: {env:?}");
        let (name, config) = &env[0];
        assert_eq!(name, "OPENCODE_CONFIG_CONTENT");
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        assert_eq!(
            config["provider"]["anthropic"]["options"]["baseURL"],
            "http://0.0.0.0:8080/code/llm/anthropic/v1",
            "the Anthropic loader posts {{baseURL}}/messages: {config}"
        );
        assert_eq!(config["provider"]["anthropic"]["options"]["apiKey"], "k3",);
        assert_eq!(
            config["provider"]["openai"]["options"]["baseURL"],
            "http://0.0.0.0:8080/code/llm/openai/v1",
            "the OpenAI loader posts {{baseURL}}/responses: {config}"
        );
        assert_eq!(config["provider"]["openai"]["options"]["apiKey"], "k3");
        assert_eq!(
            config["provider"]["model-gateway"]["options"]["baseURL"],
            "http://0.0.0.0:8080/code/llm/openai/v1",
            "provider-neutral gateway models use the model-gateway provider: {config}"
        );
        assert_eq!(
            config["provider"]["model-gateway"]["options"]["apiKey"],
            "k3"
        );
        assert_eq!(
            config["provider"]["model-gateway"]["npm"], "@ai-sdk/openai",
            "without a loader the CLI defaults to its OpenAI-compatible one and posts \
             chat completions, which the relay does not serve: {config}"
        );
        for (listed, expected_base) in [
            (
                "anthropic/shared/model",
                "http://0.0.0.0:8080/code/llm/anthropic/v1",
            ),
            (
                "model-gateway/shared/model",
                "http://0.0.0.0:8080/code/llm/openai/v1",
            ),
        ] {
            let provider = listed.split_once('/').unwrap().0;
            assert_eq!(
                config["provider"][provider]["options"]["baseURL"], expected_base,
                "the hosted picker prefix must select the relay surface that listed it: {listed}"
            );
        }
        let (argv, env) = spawn_wiring(HarnessKind::Grok, "http://0.0.0.0:8080/", "k4");
        assert!(argv.is_empty());
        assert_eq!(
            env,
            vec![
                ("TIDEBREAK_LLM_KEY".to_owned(), "k4".to_owned()),
                (
                    "GROK_MODELS_BASE_URL".to_owned(),
                    "http://0.0.0.0:8080/code/llm/openai/v1".to_owned()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn issue_replaces_the_prior_key_and_revoke_forgets() {
        let relay = HarnessLlmRelay::new(fake_gateway().await);
        let subject = subject_for("thet");
        let first = relay.issue(subject.clone());
        assert!(first.starts_with("tbreak_hl_"));
        assert!(relay.subject_for_key(&first).is_some());

        let second = relay.issue(subject.clone());
        assert_ne!(first, second);
        assert!(
            relay.subject_for_key(&first).is_none(),
            "reissue revokes the key an old child still knows"
        );
        assert!(relay.subject_for_key(&second).is_some());

        relay.revoke(subject.session);
        assert!(relay.subject_for_key(&second).is_none());
        relay.revoke(subject.session);
    }

    #[tokio::test]
    async fn forward_exchanges_and_streams_for_a_live_caller() {
        let obo = fake_gateway().await;
        obo.record_caller(&owner("thet"), Arc::from("mg_at_live"));
        let relay = HarnessLlmRelay::new(obo);
        let key = relay.issue(subject_for("thet"));

        let response = relay
            .forward(
                RelayEndpoint::OpenAiResponses,
                &bearer_headers(&key),
                Some("beta=true".to_owned()),
                axum::body::Body::from(r#"{"model":"gpt-5.6-sol"}"#),
            )
            .await;
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream"),
            "upstream headers pass through"
        );
        let (status, text) = read_text(response).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            text.contains("auth=Bearer mg_it_fresh"),
            "the exchanged token replaces the relay key: {text}"
        );
        assert!(text.contains("query=beta=true"), "{text}");
        assert!(text.contains(r#"body={"model":"gpt-5.6-sol"}"#), "{text}");
        assert!(
            !text.contains(&key) && !text.contains("should-never-forward"),
            "child credentials never reach the gateway: {text}"
        );
    }

    #[tokio::test]
    async fn forward_refuses_an_unknown_key() {
        let relay = HarnessLlmRelay::new(fake_gateway().await);
        let response = relay
            .forward(
                RelayEndpoint::AnthropicMessages,
                &bearer_headers("tbreak_hl_not-issued"),
                None,
                axum::body::Body::empty(),
            )
            .await;
        let (status, text) = read_text(response).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            text.contains("authentication_error") && text.contains("\"type\":\"error\""),
            "Claude Code needs the Anthropic error shape: {text}"
        );
    }

    #[tokio::test]
    async fn forward_fails_closed_without_a_caller_token() {
        let relay = HarnessLlmRelay::new(fake_gateway().await);
        let key = relay.issue(subject_for("thet"));
        let response = relay
            .forward(
                RelayEndpoint::OpenAiResponses,
                &bearer_headers(&key),
                None,
                axum::body::Body::empty(),
            )
            .await;
        let (status, text) = read_text(response).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "no recorded subject means sign in again, not someone else's token: {text}"
        );
    }

    #[tokio::test]
    async fn forward_reports_an_unreachable_gateway_as_502() {
        let obo =
            Arc::new(OboGateway::new("http://127.0.0.1:1", TEST_RESOURCE.to_owned()).unwrap());
        obo.record_caller(&owner("thet"), Arc::from("mg_at_live"));
        let relay = HarnessLlmRelay::new(obo);
        let key = relay.issue(subject_for("thet"));
        let response = relay
            .forward(
                RelayEndpoint::OpenAiResponses,
                &bearer_headers(&key),
                None,
                axum::body::Body::empty(),
            )
            .await;
        let (status, text) = read_text(response).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(text.contains("api_error"), "{text}");
    }

    #[tokio::test]
    async fn forward_models_tags_every_row_with_the_responses_backend() {
        let obo = fake_gateway().await;
        obo.record_caller(&owner("thet"), Arc::from("mg_at_live"));
        let relay = HarnessLlmRelay::new(obo);
        let key = relay.issue(subject_for("thet"));

        let response = relay
            .forward_models(RelayEndpoint::OpenAiModels, &bearer_headers(&key))
            .await;
        let (status, text) = read_text(response).await;
        assert_eq!(status, StatusCode::OK);
        let listing: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rows = listing["data"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(
                row["api_backend"], "responses",
                "Grok defaults a prefetched row to chat_completions, which the relay does not serve: {row}"
            );
        }
        assert_eq!(rows[0]["id"], "glm-5.3");
        assert!(
            !text.contains(&key),
            "child credentials never reach the gateway: {text}"
        );
    }

    #[tokio::test]
    async fn forward_models_refuses_an_unknown_key() {
        let relay = HarnessLlmRelay::new(fake_gateway().await);
        let response = relay
            .forward_models(
                RelayEndpoint::OpenAiModels,
                &bearer_headers("tbreak_hl_not-issued"),
            )
            .await;
        let (status, text) = read_text(response).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(
            text.contains("authentication_error") && text.contains("\"error\""),
            "the OpenAI error shape: {text}"
        );
    }
}
