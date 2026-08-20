//! On-behalf-of inference for gateway-authenticated hosted machines
//! (`docs/decisions/0051-on-behalf-of-inference-for-hosted-machines.md`).
//!
//! A hosted machine that authenticates callers against a Model Gateway
//! (decision 49) already holds each caller's short-lived, machine-bound
//! token. This module turns that token into inference authority for the same
//! user: it exchanges the caller's bearer for a short-lived, inference-only
//! gateway token and hands that token to the router as the credential for the
//! caller's turns. The deployment needs no inference secret of its own, and
//! the gateway meters every turn to the user who drove it.
//!
//! Three rules are load-bearing:
//!
//! - **Nothing here is durable.** Exchanged tokens live in this process's
//!   memory, keyed by owner, and are minted again near expiry. They are never
//!   written to the store, never logged, and never returned to a client.
//! - **The exchange never falls back.** A refusal fails the caller's turn
//!   closed. The server does not retry onto a shared credential, and one
//!   user's refusal leaves every other user's turns running.
//! - **A stored provider configuration wins.** This source is offered to the
//!   router only where the deployment states no other inference path; see
//!   [`crate::providers::collect_routes`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt as _;

use tidebreak_core::{AgentError, OwnerId, Profile, Result};
use tidebreak_router::BearerTokenSource;

/// Mint a replacement this close to expiry instead of using the cached token.
///
/// Matches the desktop's refresh leeway, so a turn never opens a request with
/// a credential that dies mid-stream.
const EXPIRY_LEEWAY_SECONDS: u64 = 60;

/// Cap on an exchange response body. The gateway answers with a small JSON
/// object; anything larger is a misconfigured endpoint, not a token.
const RESPONSE_LIMIT: usize = 16 * 1024;

/// The audience the exchanged token is minted for. Inference only.
const INFERENCE_AUDIENCE: &str = "llm";

/// The OAuth token-exchange grant the gateway accepts for this flow.
const TOKEN_EXCHANGE_GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";

/// The subject-token type this server presents: the caller's access token.
const SUBJECT_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";

/// Seconds since the Unix epoch, saturating at zero before it.
fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// One user's inference token and when it stops being usable.
#[derive(Clone)]
struct CachedToken {
    token: Arc<str>,
    expires_at_unix: u64,
}

impl CachedToken {
    /// Whether this token is far enough from expiry to open a request with.
    fn is_fresh(&self) -> bool {
        self.expires_at_unix > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS)
    }
}

/// What this process remembers about one caller.
///
/// `subject` is the most recent machine-bound bearer that caller presented,
/// and `token` is the inference token exchanged from it. The `token` mutex is
/// also the single-flight gate: concurrent turns for the same user queue on it
/// and all but the first find a fresh token already there.
struct UserSlot {
    subject: std::sync::Mutex<Arc<str>>,
    token: tokio::sync::Mutex<Option<CachedToken>>,
}

/// The OAuth error shape the gateway returns for a refused exchange.
#[derive(serde::Deserialize)]
struct OAuthError {
    error: String,
    error_description: Option<String>,
}

/// A successful exchange.
#[derive(serde::Deserialize)]
struct ExchangeResponse {
    access_token: String,
    expires_in: u64,
}

/// Per-caller inference credentials for a gateway-authenticated deployment.
///
/// Construct one per process with [`OboInference::from_config`], record each
/// authenticated caller's bearer with [`OboInference::record_caller`], and ask
/// for a caller's credential supplier with
/// [`OboInference::token_source_for`].
pub(crate) struct OboInference {
    /// Where the exchange is POSTed. Honors the verifier override, because
    /// this is a server-to-server call like principal validation.
    token_url: reqwest::Url,
    /// Anthropic-compatible inference base for exchanged tokens.
    inference_base_url: String,
    client: reqwest::Client,
    users: std::sync::Mutex<HashMap<OwnerId, Arc<UserSlot>>>,
}

impl OboInference {
    /// Build the per-caller inference path this deployment's configuration
    /// asks for, or `None` when it asks for none.
    ///
    /// `None` is the answer for every deployment that is not a
    /// gateway-authenticated hosted machine: the desktop profile, and a
    /// self-host server running on static tokens. Those have no machine-bound
    /// caller token to exchange, so they keep their configured providers
    /// (decision 51, rules 5 and 6).
    ///
    /// # Errors
    /// Fails when the configured gateway URL cannot carry a server-to-server
    /// call, so a misconfigured deployment refuses to assemble rather than
    /// failing every turn at run time.
    pub(crate) fn from_config(config: &tidebreak_core::Config) -> Result<Option<Arc<Self>>> {
        if config.profile != Profile::SelfHost {
            return Ok(None);
        }
        let Some(gateway_url) = config.auth_gateway_url.as_deref() else {
            return Ok(None);
        };
        let base = config
            .auth_gateway_verifier_url
            .as_deref()
            .unwrap_or(gateway_url);
        Ok(Some(Arc::new(Self::new(base)?)))
    }

    /// Build an exchange client against `base_url`.
    ///
    /// # Errors
    /// Fails when `base_url` is unparseable, carries credentials, a query, or
    /// a fragment, or is cleartext outside loopback development.
    pub(crate) fn new(base_url: &str) -> Result<Self> {
        let base = normalized_gateway_base(base_url)?;
        let token_url = join_below(&base, "oauth/token")?;
        let inference_base_url = join_below(&base, "compat/anthropic")?.to_string();
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                AgentError::config(format!("on-behalf-of inference client: {error}"))
            })?;
        Ok(Self {
            token_url,
            inference_base_url,
            client,
            users: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// The Anthropic-compatible base URL exchanged tokens authenticate against.
    pub(crate) fn inference_base_url(&self) -> &str {
        &self.inference_base_url
    }

    /// Remember the bearer `owner` just authenticated with.
    ///
    /// Called from the authentication middleware for every gateway-verified
    /// request, so the newest live token is the one a later turn exchanges. A
    /// replacement subject does not invalidate the cached inference token:
    /// both name the same user, and the cached one is still that user's.
    pub(crate) fn record_caller(&self, owner: &OwnerId, bearer: Arc<str>) {
        let Ok(mut users) = self.users.lock() else {
            return;
        };
        match users.get(owner) {
            Some(slot) => {
                if let Ok(mut subject) = slot.subject.lock() {
                    *subject = bearer;
                }
            }
            None => {
                users.insert(
                    owner.clone(),
                    Arc::new(UserSlot {
                        subject: std::sync::Mutex::new(bearer),
                        token: tokio::sync::Mutex::new(None),
                    }),
                );
            }
        }
    }

    /// A credential supplier for `owner`'s turns, or `None` when this process
    /// has seen no live token for them.
    ///
    /// `None` is a fail-closed answer, not a fallback: the router is offered
    /// no route, and the turn fails with the missing-credential shape rather
    /// than running on somebody else's authority.
    pub(crate) fn token_source_for(
        self: &Arc<Self>,
        owner: &OwnerId,
    ) -> Option<Arc<dyn BearerTokenSource>> {
        let users = self.users.lock().ok()?;
        users.contains_key(owner).then(|| {
            Arc::new(OboTokenSource {
                inference: self.clone(),
                owner: owner.clone(),
            }) as Arc<dyn BearerTokenSource>
        })
    }

    /// The current inference token for `owner`, exchanging one if the cached
    /// token is missing or near expiry.
    ///
    /// # Errors
    /// Fails when this process holds no subject token for `owner`, or when
    /// the gateway refuses the exchange.
    async fn bearer_for(&self, owner: &OwnerId) -> Result<String> {
        let slot = {
            let users = self.users.lock().map_err(|_| {
                AgentError::msg("on-behalf-of inference state is unavailable in this process")
            })?;
            users.get(owner).cloned()
        };
        let Some(slot) = slot else {
            return Err(AgentError::SignInRequired(
                "this machine holds no live Model Gateway session for you; sign in again".into(),
            ));
        };

        // Holding this across the exchange is the single-flight gate: a second
        // turn for the same user waits here and then finds a fresh token.
        let mut cached = slot.token.lock().await;
        if let Some(current) = cached.as_ref() {
            if current.is_fresh() {
                return Ok(current.token.to_string());
            }
        }
        let subject = {
            let subject = slot.subject.lock().map_err(|_| {
                AgentError::msg("on-behalf-of inference state is unavailable in this process")
            })?;
            subject.clone()
        };
        let minted = self.exchange(&subject).await?;
        let token = minted.token.to_string();
        *cached = Some(minted);
        Ok(token)
    }

    /// Exchange one caller bearer for a short-lived inference token.
    ///
    /// # Errors
    /// `invalid_grant` means the subject token, its session, or its user is
    /// gone; that becomes the sign-in-required shape the client already
    /// handles. Every other non-success is a refusal too — this never retries
    /// onto another credential.
    async fn exchange(&self, subject: &str) -> Result<CachedToken> {
        let form: [(&str, &str); 4] = [
            ("grant_type", TOKEN_EXCHANGE_GRANT),
            ("subject_token", subject),
            ("subject_token_type", SUBJECT_TOKEN_TYPE),
            ("audience", INFERENCE_AUDIENCE),
        ];
        let response = self
            .client
            .post(self.token_url.clone())
            .form(&form)
            .send()
            .await
            .map_err(|error| {
                AgentError::msg(format!("on-behalf-of token exchange failed: {error}"))
            })?;
        let status = response.status();
        let body = read_bounded(response).await?;
        if !status.is_success() {
            return Err(exchange_refusal(&body));
        }
        let exchanged: ExchangeResponse = serde_json::from_slice(&body).map_err(|error| {
            AgentError::msg(format!(
                "the Model Gateway returned an unreadable token exchange response: {error}"
            ))
        })?;
        if exchanged.access_token.is_empty() {
            return Err(AgentError::msg(
                "the Model Gateway returned an empty on-behalf-of token",
            ));
        }
        Ok(CachedToken {
            token: exchanged.access_token.into(),
            expires_at_unix: unix_time().saturating_add(exchanged.expires_in),
        })
    }
}

/// Turn a refused exchange into the error its cause deserves.
///
/// A body that does not decode is still a refusal — never a reason to keep
/// going with another credential.
fn exchange_refusal(body: &[u8]) -> AgentError {
    let Ok(refusal) = serde_json::from_slice::<OAuthError>(body) else {
        return AgentError::msg("the Model Gateway refused the on-behalf-of token exchange");
    };
    let detail = refusal
        .error_description
        .unwrap_or_else(|| refusal.error.clone());
    match refusal.error.as_str() {
        "invalid_grant" => AgentError::SignInRequired(format!(
            "your Model Gateway session is no longer valid: {detail}"
        )),
        "invalid_target" => AgentError::InvalidTarget(format!(
            "the Model Gateway does not allow inference on your behalf: {detail}"
        )),
        _ => AgentError::msg(format!(
            "the Model Gateway refused the on-behalf-of token exchange: {detail}"
        )),
    }
}

/// Read at most [`RESPONSE_LIMIT`] bytes, refusing anything larger.
///
/// # Errors
/// Fails when the body is oversized or the connection breaks mid-read.
async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > RESPONSE_LIMIT as u64)
    {
        return Err(AgentError::msg(
            "the Model Gateway token exchange response exceeded the size limit",
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            AgentError::msg(format!(
                "the Model Gateway token exchange response failed: {error}"
            ))
        })?;
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err(AgentError::msg(
                "the Model Gateway token exchange response exceeded the size limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Validate a gateway base URL for server-to-server use and normalize it so a
/// subpath deployment keeps its prefix when paths are joined below it.
///
/// # Errors
/// Fails on an unparseable URL, embedded credentials, a query or fragment, or
/// cleartext outside loopback development.
fn normalized_gateway_base(raw: &str) -> Result<reqwest::Url> {
    let mut base = reqwest::Url::parse(raw.trim())
        .map_err(|error| AgentError::config(format!("invalid Model Gateway URL: {error}")))?;
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(AgentError::config(
            "the Model Gateway URL must not contain credentials, a query, or a fragment",
        ));
    }
    let loopback = base.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if base.scheme() != "https" && !(base.scheme() == "http" && loopback) {
        return Err(AgentError::config(
            "the Model Gateway URL must use https (http is allowed only for loopback development)",
        ));
    }
    // A trailing slash makes `join` extend the base instead of replacing its
    // last segment, which is what keeps a subpath deployment routable.
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    Ok(base)
}

/// Join `path` below `base`.
///
/// # Errors
/// Fails when the result is not a valid URL.
fn join_below(base: &reqwest::Url, path: &str) -> Result<reqwest::Url> {
    base.join(path)
        .map_err(|error| AgentError::config(format!("invalid Model Gateway endpoint: {error}")))
}

/// One caller's credential supplier, as the router sees it.
///
/// The router asks for a bearer at each request, so a token that expires
/// mid-turn is replaced without rebuilding the route, and a session revoked
/// mid-turn fails the next request closed.
struct OboTokenSource {
    inference: Arc<OboInference>,
    owner: OwnerId,
}

#[async_trait]
impl BearerTokenSource for OboTokenSource {
    /// The owner this credential is bound to, so a cached router built for one
    /// caller is never reused for another.
    fn binding_id(&self) -> Option<&str> {
        Some(self.owner.as_str())
    }

    async fn bearer_token(&self) -> Result<String> {
        self.inference.bearer_for(&self.owner).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;

    /// What a fake gateway does with the next exchange it receives.
    #[derive(Clone)]
    struct FakeGateway {
        /// How many exchanges it has served.
        mints: Arc<AtomicUsize>,
        /// Lifetime, in seconds, of each token it mints.
        lifetime: Arc<AtomicUsize>,
        /// The OAuth error to refuse with, or empty to succeed.
        refusal: Arc<std::sync::Mutex<String>>,
        /// How long each exchange takes to answer.
        latency: Duration,
    }

    impl FakeGateway {
        fn new() -> Self {
            Self {
                mints: Arc::new(AtomicUsize::new(0)),
                lifetime: Arc::new(AtomicUsize::new(3600)),
                refusal: Arc::new(std::sync::Mutex::new(String::new())),
                latency: Duration::ZERO,
            }
        }

        fn refuse_with(&self, error: &str) {
            if let Ok(mut refusal) = self.refusal.lock() {
                *refusal = error.to_owned();
            }
        }

        fn served(&self) -> usize {
            self.mints.load(Ordering::SeqCst)
        }

        /// Serve this gateway on loopback and return an exchange client
        /// pointed at it, plus the running server's handle.
        async fn start(self) -> (Arc<OboInference>, tokio::task::JoinHandle<()>) {
            let state = self.clone();
            let app = Router::new().route(
                "/oauth/token",
                post(move |form: axum::Form<HashMap<String, String>>| {
                    let state = state.clone();
                    async move {
                        // The wire contract, asserted on the server side so a
                        // drifted request fails the test rather than passing
                        // silently.
                        assert_eq!(
                            form.get("grant_type").map(String::as_str),
                            Some(TOKEN_EXCHANGE_GRANT)
                        );
                        assert_eq!(
                            form.get("subject_token_type").map(String::as_str),
                            Some(SUBJECT_TOKEN_TYPE)
                        );
                        assert_eq!(
                            form.get("audience").map(String::as_str),
                            Some(INFERENCE_AUDIENCE)
                        );
                        let subject = form
                            .get("subject_token")
                            .cloned()
                            .unwrap_or_else(|| "missing".to_owned());
                        assert!(subject.starts_with("mg_at_"));

                        if !state.latency.is_zero() {
                            tokio::time::sleep(state.latency).await;
                        }
                        let refusal = state
                            .refusal
                            .lock()
                            .map(|refusal| refusal.clone())
                            .unwrap_or_default();
                        if !refusal.is_empty() {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                Json(serde_json::json!({
                                    "error": refusal,
                                    "error_description": "the fake gateway refused",
                                })),
                            )
                                .into_response();
                        }
                        let serial = state.mints.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({
                            "access_token": format!("mg_at_inference_{serial}_for_{subject}"),
                            "token_type": "Bearer",
                            "expires_in": state.lifetime.load(Ordering::SeqCst),
                            "scope": "inference:invoke",
                        }))
                        .into_response()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let inference = Arc::new(OboInference::new(&format!("http://{address}")).unwrap());
            (inference, server)
        }
    }

    use axum::response::IntoResponse as _;

    fn owner(name: &str) -> OwnerId {
        OwnerId::new(name).unwrap()
    }

    /// The happy path: a caller's machine-bound bearer becomes an
    /// inference-only token for the same caller, and the inference base URL
    /// is the gateway's Anthropic-compatible surface.
    #[tokio::test]
    async fn an_exchange_mints_an_inference_token_for_the_caller() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());

        let token = inference.bearer_for(&alice).await.unwrap();
        assert!(token.starts_with("mg_at_inference_"));
        assert!(token.ends_with("mg_at_alice"));
        assert!(inference
            .inference_base_url()
            .ends_with("/compat/anthropic"));
        assert_eq!(gateway.served(), 1);
        server.abort();
    }

    /// Each caller's turns run on their own token. One exchange never serves
    /// another user.
    #[tokio::test]
    async fn each_caller_gets_their_own_token() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        let bob = owner("user:bob");
        inference.record_caller(&alice, "mg_at_alice".into());
        inference.record_caller(&bob, "mg_at_bob".into());

        let alice_token = inference.bearer_for(&alice).await.unwrap();
        let bob_token = inference.bearer_for(&bob).await.unwrap();
        assert!(alice_token.ends_with("mg_at_alice"));
        assert!(bob_token.ends_with("mg_at_bob"));
        assert_ne!(alice_token, bob_token);
        assert_eq!(gateway.served(), 2);
        server.abort();
    }

    /// A live token is reused; one inside the expiry leeway is replaced.
    #[tokio::test]
    async fn a_cached_token_is_reused_until_it_nears_expiry() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());

        let first = inference.bearer_for(&alice).await.unwrap();
        let second = inference.bearer_for(&alice).await.unwrap();
        assert_eq!(first, second, "a live token must be reused");
        assert_eq!(gateway.served(), 1);

        // A caller whose tokens are minted already inside the leeway window
        // must exchange again for every request, rather than opening one with
        // a credential that dies mid-stream.
        gateway
            .lifetime
            .store(EXPIRY_LEEWAY_SECONDS as usize / 2, Ordering::SeqCst);
        let bob = owner("user:bob");
        inference.record_caller(&bob, "mg_at_bob".into());
        let third = inference.bearer_for(&bob).await.unwrap();
        assert_eq!(gateway.served(), 2);
        let fourth = inference.bearer_for(&bob).await.unwrap();
        assert_eq!(gateway.served(), 3);
        assert_ne!(third, fourth, "a token near expiry must be minted again");
        server.abort();
    }

    /// Concurrent turns for one caller share a single exchange.
    #[tokio::test]
    async fn concurrent_turns_for_one_caller_mint_once() {
        let mut gateway = FakeGateway::new();
        gateway.latency = Duration::from_millis(150);
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());

        let mut turns = Vec::new();
        for _ in 0..8 {
            let inference = inference.clone();
            let alice = alice.clone();
            turns.push(tokio::spawn(
                async move { inference.bearer_for(&alice).await },
            ));
        }
        let mut tokens = Vec::new();
        for turn in turns {
            tokens.push(turn.await.unwrap().unwrap());
        }
        assert_eq!(gateway.served(), 1, "eight turns must not stampede");
        assert!(
            tokens.windows(2).all(|pair| pair[0] == pair[1]),
            "every turn must run on the same token"
        );
        server.abort();
    }

    /// A revoked session, a deactivated user, or an expired subject token all
    /// arrive as `invalid_grant`, and all become the sign-in-required shape
    /// the client already handles.
    #[tokio::test]
    async fn a_dead_session_fails_closed_as_sign_in_required() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());
        gateway.refuse_with("invalid_grant");

        let error = inference.bearer_for(&alice).await.unwrap_err();
        assert!(
            matches!(error, AgentError::SignInRequired(_)),
            "expected a sign-in-required refusal, got {error:?}"
        );
        assert_eq!(error.kind(), "authentication");
        server.abort();
    }

    /// An audience this token may not reach is a refusal too, and a distinct
    /// one: the deployment is misconfigured, the caller is not signed out.
    #[tokio::test]
    async fn a_disallowed_audience_is_its_own_refusal() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());
        gateway.refuse_with("invalid_target");

        let error = inference.bearer_for(&alice).await.unwrap_err();
        assert!(
            matches!(error, AgentError::InvalidTarget(_)),
            "expected an invalid-target refusal, got {error:?}"
        );
        server.abort();
    }

    /// Rule 4: a session revoked mid-session fails the next turn closed. The
    /// cached token is not served past its life, and nothing falls back.
    #[tokio::test]
    async fn a_refusal_mid_session_fails_the_next_turn_closed() {
        let gateway = FakeGateway::new();
        gateway
            .lifetime
            .store(EXPIRY_LEEWAY_SECONDS as usize / 2, Ordering::SeqCst);
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());

        let first = inference.bearer_for(&alice).await.unwrap();
        assert!(first.starts_with("mg_at_inference_"));

        gateway.refuse_with("invalid_grant");
        let error = inference.bearer_for(&alice).await.unwrap_err();
        assert!(
            matches!(error, AgentError::SignInRequired(_)),
            "expected the turn to fail closed, got {error:?}"
        );
        server.abort();
    }

    /// A caller this process has never authenticated has no credential to
    /// borrow, and is refused rather than served somebody else's.
    #[tokio::test]
    async fn an_unknown_caller_is_offered_no_credential() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let stranger = owner("user:stranger");

        assert!(inference.token_source_for(&stranger).is_none());
        let error = inference.bearer_for(&stranger).await.unwrap_err();
        assert!(
            matches!(error, AgentError::SignInRequired(_)),
            "expected a sign-in-required refusal, got {error:?}"
        );
        assert_eq!(gateway.served(), 0);
        server.abort();
    }

    /// The source the router holds names its caller, so a router cached for
    /// one caller can never be mistaken for another's.
    #[tokio::test]
    async fn a_token_source_is_bound_to_its_caller() {
        let gateway = FakeGateway::new();
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice".into());

        let source = inference.token_source_for(&alice).unwrap();
        assert_eq!(source.binding_id(), Some("user:alice"));
        assert!(source
            .bearer_token()
            .await
            .unwrap()
            .ends_with("mg_at_alice"));
        server.abort();
    }

    /// A caller who refreshes their machine-bound token keeps running: the
    /// newest subject is the one a later exchange presents.
    #[tokio::test]
    async fn a_refreshed_subject_token_replaces_the_old_one() {
        let gateway = FakeGateway::new();
        gateway
            .lifetime
            .store(EXPIRY_LEEWAY_SECONDS as usize / 2, Ordering::SeqCst);
        let (inference, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        inference.record_caller(&alice, "mg_at_alice_first".into());
        assert!(inference
            .bearer_for(&alice)
            .await
            .unwrap()
            .ends_with("mg_at_alice_first"));

        inference.record_caller(&alice, "mg_at_alice_second".into());
        assert!(inference
            .bearer_for(&alice)
            .await
            .unwrap()
            .ends_with("mg_at_alice_second"));
        server.abort();
    }

    /// Rules 5 and 6: a deployment that is not a gateway-authenticated hosted
    /// machine builds no per-caller inference path at all.
    #[test]
    fn only_a_gateway_authenticated_hosted_machine_resolves_per_caller() {
        let mut config =
            tidebreak_core::Config::desktop(std::path::PathBuf::from("/tmp/tidebreak-obo-test"));

        config.profile = Profile::Desktop;
        config.auth_gateway_url = Some("https://gateway.example".to_owned());
        assert!(
            OboInference::from_config(&config).unwrap().is_none(),
            "the desktop profile is unchanged"
        );

        config.profile = Profile::SelfHost;
        config.auth_gateway_url = None;
        assert!(
            OboInference::from_config(&config).unwrap().is_none(),
            "a static-token server is unchanged"
        );

        config.auth_gateway_url = Some("https://gateway.example".to_owned());
        let inference = OboInference::from_config(&config).unwrap().unwrap();
        assert_eq!(
            inference.inference_base_url(),
            "https://gateway.example/compat/anthropic"
        );
    }

    /// The exchange is a server-to-server call, so it honors the same
    /// cluster-routable override the principal check does.
    #[test]
    fn the_verifier_override_redirects_the_exchange() {
        let mut config =
            tidebreak_core::Config::desktop(std::path::PathBuf::from("/tmp/tidebreak-obo-test"));
        config.profile = Profile::SelfHost;
        config.auth_gateway_url = Some("https://public.example".to_owned());
        config.auth_gateway_verifier_url = Some("https://gateway.internal".to_owned());

        let inference = OboInference::from_config(&config).unwrap().unwrap();
        assert_eq!(
            inference.token_url.as_str(),
            "https://gateway.internal/oauth/token"
        );
        assert_eq!(
            inference.inference_base_url(),
            "https://gateway.internal/compat/anthropic"
        );
    }

    /// A gateway deployed under a subpath keeps its prefix.
    #[test]
    fn a_subpath_deployment_keeps_its_prefix() {
        let inference = OboInference::new("https://example.test/gateway").unwrap();
        assert_eq!(
            inference.token_url.as_str(),
            "https://example.test/gateway/oauth/token"
        );
        assert_eq!(
            inference.inference_base_url(),
            "https://example.test/gateway/compat/anthropic"
        );
    }

    /// A URL that cannot carry a server-to-server credential is refused at
    /// assembly, not at the first turn.
    #[test]
    fn an_unusable_gateway_url_is_refused_at_assembly() {
        assert!(OboInference::new("https://user:pass@example.test").is_err());
        assert!(OboInference::new("https://example.test?probe=1").is_err());
        assert!(OboInference::new("http://gateway.example").is_err());
        assert!(OboInference::new("http://127.0.0.1:8080").is_ok());
    }
}
