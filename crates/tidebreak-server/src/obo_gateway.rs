//! Per-caller gateway capabilities for gateway-authenticated hosted machines
//! (decisions 51 and 62).
//!
//! A hosted machine that authenticates callers against a Model Gateway
//! (decision 49) already holds each caller's short-lived, machine-bound
//! token. This module turns that token into two capabilities for the same
//! user, each through the gateway's RFC 8693 exchange:
//!
//! - **Inference** (decision 51): a short-lived, inference-only token the
//!   router presents as the credential for the caller's turns.
//! - **Entitlements** (decision 62): a short-lived catalog capability that
//!   reads the caller's own member catalog, so the picker offers exactly the
//!   models their account entitles them to.
//!
//! The deployment needs no inference secret of its own, and the gateway
//! meters every turn to the user who drove it.
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

/// The audience the exchanged inference token is minted for.
const INFERENCE_AUDIENCE: &str = "llm";

/// The audience of the exchanged catalog capability (decision 62).
const CATALOG_AUDIENCE: &str = "catalog";

/// How long one caller's fetched catalog stays fresh before the next read
/// revalidates it against the gateway.
const CATALOG_FRESH_SECONDS: u64 = 300;

/// How long a held catalog keeps serving after revalidation starts failing on
/// transport. Refusals never coast on this grace: a revoked session stops the
/// caller at the next revalidation.
const CATALOG_STALE_GRACE_SECONDS: u64 = 3600;

/// Cap on a member-catalog response body. Far above any real catalog, far
/// below anything that could stall the process.
const CATALOG_RESPONSE_LIMIT: usize = 1024 * 1024;

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
    catalog: tokio::sync::Mutex<Option<CachedCatalog>>,
}

/// One caller's fetched entitlement snapshot and its revalidation state.
struct CachedCatalog {
    snapshot: crate::providers::GatewayModelSnapshot,
    etag: Option<String>,
    fetched_at_unix: u64,
}

/// What one member-catalog read came back as.
enum CatalogFetch {
    /// Unchanged since the `If-None-Match` revision the caller holds.
    NotModified,
    /// A fresh snapshot, with the `ETag` for the next conditional read.
    Fresh {
        snapshot: crate::providers::GatewayModelSnapshot,
        etag: Option<String>,
    },
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
/// Construct one per process with [`OboGateway::from_config`], record each
/// authenticated caller's bearer with [`OboGateway::record_caller`], and ask
/// for a caller's credential supplier with
/// [`OboGateway::token_source_for`].
pub(crate) struct OboGateway {
    /// Where the exchange is POSTed. Honors the verifier override, because
    /// this is a server-to-server call like principal validation.
    token_url: reqwest::Url,
    /// Anthropic-compatible inference base for exchanged tokens.
    inference_base_url: String,
    /// The member catalog a caller's exchanged capability reads.
    catalog_url: reqwest::Url,
    /// The normalized gateway base, stamped onto per-caller snapshots so
    /// their frozen model identities digest a stable deployment URL.
    gateway_base_url: String,
    client: reqwest::Client,
    users: std::sync::Mutex<HashMap<OwnerId, Arc<UserSlot>>>,
}

impl OboGateway {
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
        let catalog_url = join_below(&base, "api/v1/me/catalog")?;
        let gateway_base_url = base.as_str().trim_end_matches('/').to_owned();
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
            catalog_url,
            gateway_base_url,
            client,
            users: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// The normalized gateway base URL, as stamped onto per-caller snapshots.
    pub(crate) fn gateway_base_url(&self) -> &str {
        &self.gateway_base_url
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
                        catalog: tokio::sync::Mutex::new(None),
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
        let minted = self.exchange(&subject, INFERENCE_AUDIENCE).await?;
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
    async fn exchange(&self, subject: &str, audience: &str) -> Result<CachedToken> {
        let form: [(&str, &str); 4] = [
            ("grant_type", TOKEN_EXCHANGE_GRANT),
            ("subject_token", subject),
            ("subject_token_type", SUBJECT_TOKEN_TYPE),
            ("audience", audience),
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
        let body = read_bounded(response, RESPONSE_LIMIT).await?;
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

    /// This caller's own entitled-model snapshot, read from the gateway's
    /// member catalog with a `catalog` capability exchanged from their live
    /// machine-bound token (decision 62).
    ///
    /// Fresh for [`CATALOG_FRESH_SECONDS`], then revalidated with the held
    /// `ETag`. Revalidation that fails on transport keeps serving the held
    /// snapshot for [`CATALOG_STALE_GRACE_SECONDS`] — the same user's
    /// slightly stale entitlements, never another identity. A gateway
    /// refusal always propagates instead: a revoked session stops the caller
    /// at the next revalidation rather than coasting on grace.
    ///
    /// `Ok(None)` means this process has seen no live token for `owner`. The
    /// caller is offered no gateway models, which fails closed.
    ///
    /// # Errors
    /// Fails when the gateway refuses the exchange or the read, and on
    /// transport failure with nothing inside the stale grace to serve.
    pub(crate) async fn snapshot_for(
        &self,
        owner: &OwnerId,
    ) -> Result<Option<crate::providers::GatewayModelSnapshot>> {
        let slot = {
            let users = self.users.lock().map_err(|_| {
                AgentError::msg("on-behalf-of gateway state is unavailable in this process")
            })?;
            users.get(owner).cloned()
        };
        let Some(slot) = slot else {
            return Ok(None);
        };
        // Holding this across the fetch is the single-flight gate, exactly
        // like the inference token: a second surface for the same caller
        // waits here and then finds a fresh snapshot.
        let mut cached = slot.catalog.lock().await;
        let now = unix_time();
        if let Some(held) = cached.as_ref() {
            if now.saturating_sub(held.fetched_at_unix) < CATALOG_FRESH_SECONDS {
                return Ok(Some(held.snapshot.clone()));
            }
        }
        let subject = {
            let subject = slot.subject.lock().map_err(|_| {
                AgentError::msg("on-behalf-of gateway state is unavailable in this process")
            })?;
            subject.clone()
        };
        let held_etag = cached.as_ref().and_then(|held| held.etag.clone());
        let outcome = async {
            let capability = self.exchange(&subject, CATALOG_AUDIENCE).await?;
            self.fetch_catalog(&capability.token, held_etag.as_deref())
                .await
        }
        .await;
        match outcome {
            Ok(CatalogFetch::NotModified) => {
                let Some(held) = cached.as_mut() else {
                    // A 304 with nothing held is a protocol fault, not a
                    // snapshot.
                    return Err(AgentError::msg(
                        "the Model Gateway answered not-modified to an unconditional catalog read",
                    ));
                };
                held.fetched_at_unix = now;
                Ok(Some(held.snapshot.clone()))
            }
            Ok(CatalogFetch::Fresh { snapshot, etag }) => {
                *cached = Some(CachedCatalog {
                    snapshot: snapshot.clone(),
                    etag,
                    fetched_at_unix: now,
                });
                Ok(Some(snapshot))
            }
            Err(error) => {
                let refusal = matches!(
                    error,
                    AgentError::SignInRequired(_) | AgentError::InvalidTarget(_)
                );
                if !refusal {
                    if let Some(held) = cached.as_ref() {
                        if now.saturating_sub(held.fetched_at_unix) < CATALOG_STALE_GRACE_SECONDS {
                            return Ok(Some(held.snapshot.clone()));
                        }
                    }
                }
                Err(error)
            }
        }
    }

    /// One member-catalog read with the exchanged capability.
    ///
    /// # Errors
    /// `401`/`403` and `404` are refusals — a dead session and a gateway
    /// without the route — and never fall back. Transport failures and other
    /// statuses are transient and eligible for the caller's stale grace.
    async fn fetch_catalog(
        &self,
        bearer: &str,
        if_none_match: Option<&str>,
    ) -> Result<CatalogFetch> {
        let mut request = self
            .client
            .get(self.catalog_url.clone())
            .bearer_auth(bearer);
        if let Some(etag) = if_none_match {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(|error| {
            AgentError::msg(format!("the Model Gateway catalog read failed: {error}"))
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(CatalogFetch::NotModified);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(AgentError::InvalidTarget(
                "the Model Gateway does not serve the member catalog; update the deployment".into(),
            ));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AgentError::SignInRequired(
                "the Model Gateway refused the catalog read for your session; sign in again".into(),
            ));
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = read_bounded(response, CATALOG_RESPONSE_LIMIT).await?;
        if !status.is_success() {
            return Err(AgentError::msg(format!(
                "the Model Gateway catalog read failed with status {status}"
            )));
        }
        let catalog: crate::connectors::GatewayCatalog =
            serde_json::from_slice(&body).map_err(|error| {
                AgentError::msg(format!(
                    "the Model Gateway returned an unreadable catalog: {error}"
                ))
            })?;
        let (models, model_protocols) = crate::providers::member_catalog_models(catalog);
        // The gateway is trusted for entitlements, not for shapes: the
        // caller's set is held to the same bounds as user-entered custom
        // models, exactly like the managed sync.
        crate::providers::validate_custom_models(&models).map_err(|error| {
            AgentError::msg(format!("the Model Gateway catalog was rejected: {error:?}"))
        })?;
        Ok(CatalogFetch::Fresh {
            snapshot: crate::providers::GatewayModelSnapshot {
                gateway_url: self.gateway_base_url.clone(),
                installation_id: None,
                models,
                model_protocols,
                member_catalog: Some(crate::connectors::MEMBER_CATALOG_V1.to_owned()),
                catalog_etag: etag.clone(),
            },
            etag,
        })
    }

    /// Seed a caller's snapshot directly, for tests that need routes without
    /// a live fake gateway behind them.
    #[cfg(test)]
    pub(crate) async fn seed_snapshot_for_test(
        &self,
        owner: &OwnerId,
        snapshot: crate::providers::GatewayModelSnapshot,
    ) {
        let slot = {
            let users = self.users.lock().unwrap();
            users.get(owner).cloned()
        };
        if let Some(slot) = slot {
            *slot.catalog.lock().await = Some(CachedCatalog {
                snapshot,
                etag: None,
                fetched_at_unix: unix_time(),
            });
        }
    }

    /// Force the next [`OboGateway::snapshot_for`] to revalidate, from tests.
    #[cfg(test)]
    async fn expire_catalog_for_test(&self, owner: &OwnerId) {
        let slot = {
            let users = self.users.lock().unwrap();
            users.get(owner).cloned()
        };
        if let Some(slot) = slot {
            if let Some(held) = slot.catalog.lock().await.as_mut() {
                held.fetched_at_unix = 0;
            }
        }
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

/// Read at most `limit` bytes, refusing anything larger.
///
/// # Errors
/// Fails when the body is oversized or the connection breaks mid-read.
async fn read_bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
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
        if bytes.len().saturating_add(chunk.len()) > limit {
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
    inference: Arc<OboGateway>,
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
        /// How many member-catalog reads it has served (304s included).
        catalog_reads: Arc<AtomicUsize>,
        /// The member catalog it serves, with a fixed `ETag` of `"fake-1"`.
        catalog: Arc<std::sync::Mutex<serde_json::Value>>,
    }

    impl FakeGateway {
        fn new() -> Self {
            Self {
                mints: Arc::new(AtomicUsize::new(0)),
                lifetime: Arc::new(AtomicUsize::new(3600)),
                refusal: Arc::new(std::sync::Mutex::new(String::new())),
                latency: Duration::ZERO,
                catalog_reads: Arc::new(AtomicUsize::new(0)),
                catalog: Arc::new(std::sync::Mutex::new(serde_json::json!({
                    "models": [
                        {
                            "id": "acme-opus",
                            "name": "Acme Opus",
                            "protocols": ["anthropic_messages"],
                            "aliases": [],
                            "supports_tools": true,
                            "supports_vision": true,
                            "context_window": 200_000,
                            "max_output_tokens": 32_000,
                            "provider_name": "Anthropic",
                        },
                        {
                            "id": "acme-gpt",
                            "name": "Acme GPT",
                            "protocols": ["openai_responses"],
                            "aliases": [],
                            "supports_tools": true,
                            "supports_vision": false,
                            "context_window": 128_000,
                            "max_output_tokens": 16_000,
                            "provider_name": "OpenAI",
                        },
                    ],
                    "apps": [],
                }))),
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
        async fn start(self) -> (Arc<OboGateway>, tokio::task::JoinHandle<()>) {
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
                        let audience = form.get("audience").cloned().unwrap_or_default();
                        assert!(
                            audience == INFERENCE_AUDIENCE || audience == CATALOG_AUDIENCE,
                            "unexpected audience {audience:?}"
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
                        let label = if audience == CATALOG_AUDIENCE {
                            "catalog"
                        } else {
                            "inference"
                        };
                        Json(serde_json::json!({
                            "access_token": format!("mg_at_{label}_{serial}_for_{subject}"),
                            "token_type": "Bearer",
                            "expires_in": state.lifetime.load(Ordering::SeqCst),
                            "scope": "inference:invoke",
                        }))
                        .into_response()
                    }
                }),
            );
            let catalog_state = self.clone();
            let app = app.route(
                "/api/v1/me/catalog",
                axum::routing::get(move |headers: axum::http::HeaderMap| {
                    let state = catalog_state.clone();
                    async move {
                        // The read must ride an exchanged catalog capability,
                        // never the machine-bound subject or an inference
                        // token — asserted server-side so a drifted client
                        // fails the test.
                        let bearer = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.strip_prefix("Bearer "))
                            .unwrap_or_default()
                            .to_owned();
                        assert!(
                            bearer.starts_with("mg_at_catalog_"),
                            "the catalog read must present a catalog capability, got {bearer:?}"
                        );
                        state.catalog_reads.fetch_add(1, Ordering::SeqCst);
                        if headers
                            .get(axum::http::header::IF_NONE_MATCH)
                            .and_then(|value| value.to_str().ok())
                            == Some("\"fake-1\"")
                        {
                            return axum::http::StatusCode::NOT_MODIFIED.into_response();
                        }
                        let body = state
                            .catalog
                            .lock()
                            .map(|catalog| catalog.clone())
                            .unwrap_or_default();
                        ([(axum::http::header::ETAG, "\"fake-1\"")], Json(body)).into_response()
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let inference = Arc::new(OboGateway::new(&format!("http://{address}")).unwrap());
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
            OboGateway::from_config(&config).unwrap().is_none(),
            "the desktop profile is unchanged"
        );

        config.profile = Profile::SelfHost;
        config.auth_gateway_url = None;
        assert!(
            OboGateway::from_config(&config).unwrap().is_none(),
            "a static-token server is unchanged"
        );

        config.auth_gateway_url = Some("https://gateway.example".to_owned());
        let inference = OboGateway::from_config(&config).unwrap().unwrap();
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

        let inference = OboGateway::from_config(&config).unwrap().unwrap();
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
        let inference = OboGateway::new("https://example.test/gateway").unwrap();
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
        assert!(OboGateway::new("https://user:pass@example.test").is_err());
        assert!(OboGateway::new("https://example.test?probe=1").is_err());
        assert!(OboGateway::new("http://gateway.example").is_err());
        assert!(OboGateway::new("http://127.0.0.1:8080").is_ok());
    }

    /// Decision 62's happy path: a recorded caller's snapshot is their own
    /// member catalog, read with an exchanged catalog capability and shaped
    /// exactly like the managed sync would store it.
    #[tokio::test]
    async fn a_caller_reads_their_own_entitlement_snapshot() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        let snapshot = obo.snapshot_for(&alice).await.unwrap().unwrap();
        assert_eq!(snapshot.gateway_url, obo.gateway_base_url());
        let ids: Vec<_> = snapshot
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert_eq!(ids, ["acme-opus", "acme-gpt"]);
        assert_eq!(
            snapshot.model_protocols.get("acme-gpt").copied(),
            Some(crate::providers::GatewayModelProtocol::OpenaiResponses)
        );
        assert_eq!(gateway.catalog_reads.load(Ordering::SeqCst), 1);
        assert_eq!(gateway.served(), 1, "one catalog capability was minted");
        server.abort();
    }

    /// A fresh snapshot is served from memory; an expired one revalidates
    /// with the held `ETag` and keeps the snapshot on not-modified.
    #[tokio::test]
    async fn a_fresh_snapshot_is_memory_and_a_stale_one_revalidates() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        let first = obo.snapshot_for(&alice).await.unwrap().unwrap();
        let second = obo.snapshot_for(&alice).await.unwrap().unwrap();
        assert_eq!(first.models.len(), second.models.len());
        assert_eq!(
            gateway.catalog_reads.load(Ordering::SeqCst),
            1,
            "a fresh snapshot must not refetch"
        );

        obo.expire_catalog_for_test(&alice).await;
        let third = obo.snapshot_for(&alice).await.unwrap().unwrap();
        assert_eq!(third.models.len(), first.models.len());
        assert_eq!(
            gateway.catalog_reads.load(Ordering::SeqCst),
            2,
            "a stale snapshot must revalidate"
        );
        server.abort();
    }

    /// A caller this process has never authenticated has no snapshot, which
    /// offers them no models rather than somebody else's.
    #[tokio::test]
    async fn an_unknown_caller_has_no_snapshot() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let stranger = owner("user:stranger");
        assert!(obo.snapshot_for(&stranger).await.unwrap().is_none());
        assert_eq!(gateway.served(), 0);
        server.abort();
    }

    /// A refused exchange stops the catalog too: a revoked session becomes
    /// sign-in-required, and nothing is served on grace past a refusal.
    #[tokio::test]
    async fn a_dead_session_stops_the_catalog_closed() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());
        obo.snapshot_for(&alice).await.unwrap().unwrap();

        gateway.refuse_with("invalid_grant");
        obo.expire_catalog_for_test(&alice).await;
        let error = obo.snapshot_for(&alice).await.unwrap_err();
        assert!(
            matches!(error, AgentError::SignInRequired(_)),
            "expected sign-in-required, got {error:?}"
        );
        server.abort();
    }

    /// Two callers hold two snapshots: one caller's fetch never serves the
    /// other's surfaces.
    #[tokio::test]
    async fn each_caller_holds_their_own_snapshot() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        let bob = owner("user:bob");
        obo.record_caller(&alice, "mg_at_alice".into());
        obo.record_caller(&bob, "mg_at_bob".into());

        obo.snapshot_for(&alice).await.unwrap().unwrap();
        obo.snapshot_for(&bob).await.unwrap().unwrap();
        assert_eq!(
            gateway.catalog_reads.load(Ordering::SeqCst),
            2,
            "each caller fetches their own catalog"
        );
        server.abort();
    }
}
