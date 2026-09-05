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

use tidebreak_core::{AgentError, HarnessKind, OwnerId, SessionId};

use std::path::Path;

use crate::obo_gateway::{GatewayCompatModel, OboGateway};

/// Whose inference a relay key spends.
#[derive(Clone)]
pub struct HarnessLlmSubject {
    pub owner: OwnerId,
    pub session: SessionId,
}

/// The gateway compat endpoint one relay route forwards to.
#[derive(Clone, Copy)]
pub enum RelayEndpoint {
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
    pub fn error_response(self, status: StatusCode, kind: &str, message: &str) -> Response {
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
pub struct HarnessLlmRelay {
    obo: Arc<OboGateway>,
    client: reqwest::Client,
    state: Mutex<RelayState>,
    external: Option<Arc<crate::obo_gateway::external::ExternalDelegations>>,
}

#[derive(Default)]
struct RelayState {
    by_session: HashMap<SessionId, String>,
    keys: HashMap<String, HarnessLlmSubject>,
}

impl HarnessLlmRelay {
    pub fn new(obo: Arc<OboGateway>) -> Self {
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
            external: None,
        }
    }

    /// Attach durable external consent before any worker can use this relay.
    pub fn with_external_delegations(mut self, db: Arc<tidebreak_core::db::DbStore>) -> Self {
        self.external = Some(Arc::new(
            crate::obo_gateway::external::ExternalDelegations::new(self.obo.clone(), db),
        ));
        self
    }

    pub fn external_delegations(
        &self,
    ) -> Option<&Arc<crate::obo_gateway::external::ExternalDelegations>> {
        self.external.as_ref()
    }

    pub async fn gateway_for_session(
        &self,
        owner: &OwnerId,
        session: SessionId,
    ) -> tidebreak_core::Result<Arc<OboGateway>> {
        match self.external.as_ref() {
            Some(external) => Ok(external
                .for_session(owner, session)
                .await?
                .unwrap_or_else(|| self.obo.clone())),
            None => Ok(self.obo.clone()),
        }
    }

    pub async fn external_gateway_for_session(
        &self,
        owner: &OwnerId,
        session: SessionId,
    ) -> tidebreak_core::Result<Option<Arc<OboGateway>>> {
        match self.external.as_ref() {
            Some(external) => external.for_session(owner, session).await,
            None => Ok(None),
        }
    }

    pub async fn catalog_for_session(
        &self,
        owner: &OwnerId,
        session: SessionId,
    ) -> tidebreak_core::Result<Option<crate::providers::GatewayModelSnapshot>> {
        self.gateway_for_session(owner, session)
            .await?
            .snapshot_for(owner)
            .await
    }

    pub async fn catalog_for_grant(
        &self,
        owner: &OwnerId,
        grant: tidebreak_core::CodeGrantId,
    ) -> tidebreak_core::Result<Option<crate::providers::GatewayModelSnapshot>> {
        self.external
            .as_ref()
            .ok_or_else(|| AgentError::SignInRequired("reconnect this external connection".into()))?
            .for_grant(owner, grant)
            .await?
            .snapshot_for(owner)
            .await
    }

    /// Both of the caller's compat listings. Keeping the read behind the
    /// relay means hosted pickers and the turns they start always answer to
    /// the same gateway deployment.
    pub async fn listings(
        &self,
        owner: &OwnerId,
    ) -> tidebreak_core::Result<(
        tidebreak_core::Result<Vec<GatewayCompatModel>>,
        tidebreak_core::Result<Vec<GatewayCompatModel>>,
    )> {
        self.obo.compat_listings(owner).await
    }

    /// The caller's model catalog, including per-model reasoning ladders when
    /// the gateway states them.
    pub async fn catalog(
        &self,
        owner: &OwnerId,
    ) -> tidebreak_core::Result<Option<crate::providers::GatewayModelSnapshot>> {
        self.obo.snapshot_for(owner).await
    }

    /// Mint the relay key for `subject`, replacing any prior key the same
    /// session held. Reissue-on-attach is what keeps a reaped or relaunched
    /// worker from extending the life of a key an old child still knows.
    pub fn issue(&self, subject: HarnessLlmSubject) -> String {
        let key = generate_key();
        let mut state = self.state.lock().expect("harness llm registry");
        if let Some(old) = state.by_session.insert(subject.session, key.clone()) {
            state.keys.remove(&old);
        }
        state.keys.insert(key.clone(), subject);
        key
    }

    /// Revoke the key for `session_id`. Idempotent.
    pub fn revoke(&self, session_id: SessionId) {
        let mut state = self.state.lock().expect("harness llm registry");
        if let Some(key) = state.by_session.remove(&session_id) {
            state.keys.remove(&key);
        }
    }

    /// Whose session a relayed request speaks for, from the same key the
    /// inference routes accept. `None` for a missing, unknown, or revoked
    /// key; the caller words the refusal for its own protocol.
    pub fn subject_for_headers(&self, headers: &HeaderMap) -> Option<HarnessLlmSubject> {
        relay_key(headers).and_then(|key| self.subject_for_key(key))
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
    #[allow(clippy::result_large_err)]
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
        let bearer = async {
            self.gateway_for_session(&subject.owner, subject.session)
                .await?
                .bearer_for(&subject.owner)
                .await
        }
        .await;
        match bearer {
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
    pub async fn forward(
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
    pub async fn forward_models(&self, endpoint: RelayEndpoint, headers: &HeaderMap) -> Response {
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

/// Environment variable name the relay wiring uses to carry the session
/// key to engines whose clients read a credential from the environment.
/// Callers put the same name in `SessionSpec::relay_key_env` so the harness
/// adapters' reserved-namespace handling lets exactly this one variable
/// through.
pub const RELAY_KEY_ENV: &str = "TIDEBREAK_LLM_KEY";

/// The argv and environment that point one engine child at the relay.
///
/// A thin wrapper over [`tidebreak_harness::wiring::spawn_wiring`], which
/// owns the engine knowledge; this side owns the host knowledge — the
/// relay's endpoint paths under the loopback base and the key variable
/// name.
pub fn spawn_wiring(
    kind: HarnessKind,
    loopback_base: &str,
    key: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let base = loopback_base.trim_end_matches('/');
    tidebreak_harness::wiring::spawn_wiring(
        kind,
        &tidebreak_harness::wiring::InferenceWiring {
            anthropic_base: &format!("{base}/code/llm/anthropic"),
            openai_base: &format!("{base}/code/llm/openai"),
            key_env: RELAY_KEY_ENV,
            key,
        },
    )
}

/// The loopback route a machine session's git asks for a credential.
pub const GIT_CREDENTIAL_PATH: &str = "/code/git/credential";

/// The environment that points a machine session's own `git` at the
/// loopback credential route, so the harness's shell can push the branch it
/// made without the person holding a token on the machine.
///
/// Two helper entries: the first empty one clears any helper the machine's
/// git config names, the second answers `get` by posting git's description
/// to the route with the session's relay key and passing the answer back.
/// `store` and `erase` swallow their input, so a dying credential is never
/// written by another helper. The route pins the host to the workspace's
/// origin, so a rewritten remote gets no credential. The shell policy
/// refuses the agent's own `credential.helper` changes, which keeps this
/// the only helper the session ever runs.
pub fn git_credential_wiring(loopback_base: &str) -> Vec<(String, String)> {
    let base = loopback_base.trim_end_matches('/');
    let helper = format!(
        "!f() {{ if [ \"$1\" = get ]; then curl -fsS -X POST --data-binary @- \
         -H \"Authorization: Bearer ${RELAY_KEY_ENV}\" \"{base}{GIT_CREDENTIAL_PATH}\"; \
         else cat >/dev/null; fi; }}; f"
    );
    vec![
        ("GIT_CONFIG_COUNT".to_owned(), "2".to_owned()),
        (
            "GIT_CONFIG_KEY_0".to_owned(),
            "credential.helper".to_owned(),
        ),
        ("GIT_CONFIG_VALUE_0".to_owned(), String::new()),
        (
            "GIT_CONFIG_KEY_1".to_owned(),
            "credential.helper".to_owned(),
        ),
        ("GIT_CONFIG_VALUE_1".to_owned(), helper),
    ]
}

/// The `gh` a machine session runs: a wrapper that borrows the session's
/// forge credential for one call through the same loopback route git uses,
/// hands it to the real `gh` as `GH_TOKEN`, and never writes it anywhere.
/// `gh auth status` reads signed in, `gh pr create` works against the
/// workspace's own repository, and a token that dies within the hour is
/// fetched fresh on the next call. With no relay key in the environment, or
/// no credential lent, the real `gh` runs as it would have anyway.
pub fn gh_shim_script(real_gh: &Path, loopback_base: &str, origin_host: &str) -> String {
    let base = loopback_base.trim_end_matches('/');
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    format!(
        "#!/bin/sh\n\
         # Tidebreak: gh borrows this session's forge credential per call.\n\
         if [ -n \"${RELAY_KEY_ENV}\" ]; then\n\
         \x20 token=$(printf 'protocol=https\\nhost=%s\\n' {host} | \
         curl -fsS -X POST --data-binary @- \
         -H \"Authorization: Bearer ${RELAY_KEY_ENV}\" {route} 2>/dev/null | \
         sed -n 's/^password=//p')\n\
         \x20 if [ -n \"$token\" ]; then GH_TOKEN=\"$token\"; export GH_TOKEN; fi\n\
         fi\n\
         exec {real} \"$@\"\n",
        host = quote(origin_host),
        route = quote(&format!("{base}{GIT_CREDENTIAL_PATH}")),
        real = quote(&real_gh.to_string_lossy()),
    )
}

/// Whether the on-behalf-of relay can carry this engine's inference.
///
/// Exactly the engines [`spawn_wiring`] points at the relay. The doctor reads
/// this on a gateway-hosted machine, where a covered engine needs no local
/// sign-in and an uncovered one cannot run at all; everywhere else the local
/// probe decides, and this answer does not matter.
pub fn relay_covered(kind: HarnessKind) -> bool {
    match kind {
        HarnessKind::ClaudeCode
        | HarnessKind::Codex
        | HarnessKind::Opencode
        | HarnessKind::Grok => true,
        // The in-process engine never reaches the relay: it resolves
        // inference through the server's own provider resolution, which on
        // a hosted machine is the same gateway. Covered, in the sense the
        // doctor asks about — it needs no local sign-in.
        HarnessKind::Internal => true,
    }
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
            session: SessionId::new(),
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

    #[test]
    fn git_wiring_clears_inherited_helpers_and_posts_to_the_loopback_route() {
        let env = git_credential_wiring("http://127.0.0.1:4321/");
        let value = |key: &str| {
            env.iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .unwrap_or_default()
        };
        assert_eq!(value("GIT_CONFIG_COUNT"), "2");
        assert_eq!(value("GIT_CONFIG_KEY_0"), "credential.helper");
        assert_eq!(
            value("GIT_CONFIG_VALUE_0"),
            "",
            "the first entry clears every configured helper"
        );
        assert_eq!(value("GIT_CONFIG_KEY_1"), "credential.helper");
        let helper = value("GIT_CONFIG_VALUE_1");
        assert!(helper.starts_with("!f() {"), "{helper}");
        assert!(
            helper.contains("http://127.0.0.1:4321/code/git/credential"),
            "{helper}"
        );
        assert!(
            helper.contains("Bearer $TIDEBREAK_LLM_KEY"),
            "the session's own key: {helper}"
        );
        assert!(
            helper.contains("= get ]"),
            "only `get` reaches the route: {helper}"
        );
        assert!(
            helper.contains("cat >/dev/null"),
            "store and erase swallow: {helper}"
        );
    }

    #[test]
    fn the_gh_shim_borrows_per_call_and_execs_the_real_binary() {
        let script = gh_shim_script(
            Path::new("/usr/bin/gh"),
            "http://127.0.0.1:4321/",
            "github.com",
        );
        assert!(script.starts_with("#!/bin/sh\n"), "{script}");
        assert!(script.contains("host=%s"), "{script}");
        assert!(
            script.contains("'github.com'"),
            "the origin host is pinned: {script}"
        );
        assert!(
            script.contains("'http://127.0.0.1:4321/code/git/credential'"),
            "{script}"
        );
        assert!(script.contains("Bearer $TIDEBREAK_LLM_KEY"), "{script}");
        assert!(
            script.contains("GH_TOKEN=\"$token\"; export GH_TOKEN"),
            "{script}"
        );
        assert!(script.ends_with("exec '/usr/bin/gh' \"$@\"\n"), "{script}");
        assert!(
            !script.contains("gh auth login"),
            "nothing durable is written: {script}"
        );
        let odd = gh_shim_script(Path::new("/opt/it's/gh"), "http://h", "GitHub.Example");
        assert!(
            odd.contains("exec '/opt/it'\\''s/gh'"),
            "a quote in the path survives: {odd}"
        );
    }
}
