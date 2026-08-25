//! Per-caller gateway capabilities for gateway-authenticated hosted machines
//! (decisions 51, 62, and 63).
//!
//! A hosted machine that authenticates callers against a Model Gateway
//! (decision 49) already holds each caller's short-lived, machine-bound
//! token. This module turns that token into three capabilities for the same
//! user:
//!
//! - **Inference** (decision 51): a short-lived, inference-only token the
//!   router presents as the credential for the caller's turns, through the
//!   gateway's RFC 8693 exchange.
//! - **Entitlements** (decision 62): a short-lived catalog capability that
//!   reads the caller's own member catalog, so the picker offers exactly the
//!   models their account entitles them to.
//! - **Git identity** (decision 63): a repository-scoped forge credential
//!   borrowed per clone or push, presented with the machine-bound token
//!   directly — the gateway's git-credential surface is authenticated like
//!   its principal read, not through the exchange.
//!
//! The deployment needs no inference secret of its own, and the gateway
//! meters every turn to the user who drove it.
//!
//! Three rules are load-bearing:
//!
//! - **Nothing here is durable.** Exchanged tokens live in this process's
//!   memory, keyed by owner, and are minted again near expiry; a borrowed git
//!   credential lives only for the operation that borrowed it. None of them
//!   are written to the store, logged, or returned to a client.
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

/// How long one caller's probed git-forge identity stays fresh.
///
/// The identity decorates surfaces that refetch often — the PR card reloads
/// on every content revision — while the underlying answer changes on the
/// pace of a deployment registering a forge. Sixty seconds keeps the picker
/// honest within a minute of a forge appearing without asking the gateway
/// per render.
const GIT_FORGE_FRESH_SECONDS: u64 = 60;

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
    git_forge: tokio::sync::Mutex<Option<CachedGitForge>>,
}

/// One caller's probed git-forge answer and when it was read.
///
/// Refusals are cached beside availability: a deployment with no forge is the
/// common case, and re-asking the gateway on every PR-card render would turn
/// "not offered" into traffic. Transport failures are never cached — the next
/// read asks again.
struct CachedGitForge {
    outcome: Result<GitForgeIdentity, GitForgeError>,
    fetched_at_unix: u64,
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

/// The forge identity a hosted machine's git operations act as.
///
/// Work done with a borrowed credential lands as the deployment's GitHub App
/// (decision 63) or, once the caller has connected their own account at the
/// gateway, as the caller (decision 65) — this is what the UI names the
/// acting identity with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitForgeIdentity {
    /// The gateway's display name for the forge app.
    pub app_name: String,
    /// Whose account work lands as.
    pub attribution: GitForgeAttribution,
}

/// Whose account work done with a borrowed forge credential lands as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitForgeAttribution {
    /// The App's bot account (decision 63): `<slug>[bot]` when the gateway
    /// has recorded the App slug.
    Bot {
        /// The bot account work lands as, when known.
        bot_login: Option<String>,
    },
    /// The caller's own account (decision 65), lent per operation from
    /// their gateway connection.
    Person {
        /// The forge login work lands as.
        login: String,
        /// The account's display name, when the forge records one.
        display_name: Option<String>,
        /// The no-reply email commits should name the person by.
        commit_email: Option<String>,
    },
}

/// One repository the gateway list offers (decision 70).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHubRepository {
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

/// One borrowed, repository-scoped forge credential (decision 63).
///
/// Held for the length of a single git operation and dropped. Deliberately
/// no `Clone`, and `Debug` redacts the secret: the secret is the whole
/// value, and the fewer copies and formatters that can touch it, the better.
pub(crate) struct GitCredential {
    /// Login half of the HTTP basic pair — `x-access-token` for GitHub Apps.
    pub username: String,
    /// The repository-scoped installation token, dead within about an hour.
    pub secret: String,
}

impl std::fmt::Debug for GitCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitCredential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Why the gateway would not lend a git identity or credential.
///
/// The stable codes of the gateway's git-credential surface (gateway ADR
/// 0083), plus the two local outcomes: a caller this process holds no live
/// session for, and a gateway that could not be read. Each variant is phrased
/// by its consumer — the repo-source probe words them as availability, a
/// clone or push words them as the operation's failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GitForgeError {
    /// The caller's machine-bound session is gone; they sign in again.
    SignInRequired(String),
    /// No forge with a mintable App identity is available to this caller.
    NoGitForge,
    /// More than one forge is available; the gateway refuses to pick.
    AmbiguousGitForge,
    /// The forge's identity is a person's own Connect, and this machine did
    /// not or could not ask to act as them.
    ConnectModeForge,
    /// The forge acts as each caller individually, and this caller has not
    /// connected their account at the gateway yet (decision 65). Carries
    /// the gateway page where they connect, when the gateway names one.
    NotConnected { connect_url: Option<String> },
    /// The forge has no approved GitHub App installation yet.
    ForgeAppNotInstalled,
    /// The App installation does not cover the requested repository.
    RepositoryNotInstalled,
    /// The gateway or the forge could not serve the request; retryable.
    Unavailable(String),
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
    /// The member catalog a caller's exchanged capability reads.
    catalog_url: reqwest::Url,
    /// The git-credential mint, presented with the machine-bound token
    /// directly (decision 63).
    git_credential_url: reqwest::Url,
    /// The no-mint git-forge availability probe beside it.
    git_forge_url: reqwest::Url,
    /// The repository list beside the probe (gateway ADR 0091).
    git_repositories_url: reqwest::Url,
    /// This machine's `tidebreak:<sha256>` resource, named in every
    /// git-credential request so the gateway verifies the token lives in
    /// exactly this machine's resource.
    resource: String,
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
        // The same requirement gateway authentication states at boot
        // (decision 49): without the public URL there is no machine resource,
        // and without the resource no git-credential request can name what
        // the presented token must live in.
        let Some(public_url) = config.public_url.as_deref() else {
            return Err(AgentError::config(
                "TIDEBREAK_PUBLIC_URL is required with TIDEBREAK_AUTH_GATEWAY_URL so user credentials can be bound to this exact machine",
            ));
        };
        let resource = tidebreak_core::config::tidebreak_machine_resource(
            &crate::auth::canonical_public_url(public_url)?,
        );
        let base = config
            .auth_gateway_verifier_url
            .as_deref()
            .unwrap_or(gateway_url);
        Ok(Some(Arc::new(Self::new(base, resource)?)))
    }

    /// Build an exchange client against `base_url`, acting as the machine
    /// bound to `resource`.
    ///
    /// # Errors
    /// Fails when `base_url` is unparseable, carries credentials, a query, or
    /// a fragment, or is cleartext outside loopback development.
    pub(crate) fn new(base_url: &str, resource: String) -> Result<Self> {
        let base = normalized_gateway_base(base_url)?;
        let token_url = join_below(&base, "oauth/token")?;
        let catalog_url = join_below(&base, "api/v1/me/catalog")?;
        let git_credential_url = join_below(&base, "api/v1/tidebreak/git-credential")?;
        let git_forge_url = join_below(&base, "api/v1/tidebreak/git-forge")?;
        let git_repositories_url = join_below(&base, "api/v1/tidebreak/git-repositories")?;
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
            catalog_url,
            git_credential_url,
            git_forge_url,
            git_repositories_url,
            resource,
            gateway_base_url,
            client,
            users: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// The normalized gateway base URL, as stamped onto per-caller snapshots.
    pub(crate) fn gateway_base_url(&self) -> &str {
        &self.gateway_base_url
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
                        git_forge: tokio::sync::Mutex::new(None),
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

    /// The forge identity this caller's git operations would act as, probed
    /// from the gateway without minting anything (decision 63).
    ///
    /// Settled answers — an identity, or a named refusal like "no forge" —
    /// are held fresh for [`GIT_FORGE_FRESH_SECONDS`], because the surfaces
    /// that render them refetch far faster than deployments change. A dead
    /// session and a transport failure are never held: the next read asks
    /// again.
    ///
    /// # Errors
    /// Fails when this process holds no live token for `owner`, when the
    /// gateway names a refusal, and on transport failure.
    pub(crate) async fn git_forge_identity(
        &self,
        owner: &OwnerId,
    ) -> Result<GitForgeIdentity, GitForgeError> {
        let Some(slot) = self.slot_for(owner)? else {
            return Err(GitForgeError::SignInRequired(
                "this machine holds no live Model Gateway session for you; sign in again".into(),
            ));
        };
        // Holding this across the probe is the single-flight gate, exactly
        // like the inference token and the catalog.
        let mut cached = slot.git_forge.lock().await;
        let now = unix_time();
        if let Some(held) = cached.as_ref() {
            if now.saturating_sub(held.fetched_at_unix) < GIT_FORGE_FRESH_SECONDS {
                return held.outcome.clone();
            }
        }
        let subject = subject_of(&slot)?;
        let outcome = self.fetch_git_forge(&subject).await;
        match &outcome {
            Ok(_)
            | Err(
                GitForgeError::NoGitForge
                | GitForgeError::AmbiguousGitForge
                | GitForgeError::ConnectModeForge
                | GitForgeError::NotConnected { .. }
                | GitForgeError::ForgeAppNotInstalled
                | GitForgeError::RepositoryNotInstalled,
            ) => {
                *cached = Some(CachedGitForge {
                    outcome: outcome.clone(),
                    fetched_at_unix: now,
                });
            }
            Err(GitForgeError::SignInRequired(_) | GitForgeError::Unavailable(_)) => {}
        }
        outcome
    }

    /// Borrow one repository-scoped forge credential for `owner`'s git
    /// operation against `repository` (`owner/repo`), minted by the gateway
    /// per request (decision 63).
    ///
    /// Deliberately no cache and no single-flight: each clone or push
    /// borrows its own dying credential, which is the contract that keeps
    /// nothing durable on this machine. The caller holds the result for the
    /// length of the operation and drops it.
    ///
    /// # Errors
    /// Fails when this process holds no live token for `owner`, when the
    /// gateway names a refusal, and on transport failure.
    pub(crate) async fn git_credential(
        &self,
        owner: &OwnerId,
        repository: &str,
    ) -> Result<GitCredential, GitForgeError> {
        let Some(slot) = self.slot_for(owner)? else {
            return Err(GitForgeError::SignInRequired(
                "this machine holds no live Model Gateway session for you; sign in again".into(),
            ));
        };
        let subject = subject_of(&slot)?;
        let response = self
            .client
            .post(self.git_credential_url.clone())
            .bearer_auth(subject.as_ref())
            .json(&serde_json::json!({
                "resource": self.resource,
                "repository": repository,
                // This machine renders person attribution (decision 65), so
                // a connect-mode forge may lend the caller's own credential.
                // A gateway that predates the field ignores it and refuses
                // connect-mode forges exactly as before.
                "attribution": "person",
            }))
            .send()
            .await
            .map_err(|error| {
                GitForgeError::Unavailable(format!(
                    "the Model Gateway git-credential request failed: {error}"
                ))
            })?;
        let status = response.status();
        let body = read_bounded(response, RESPONSE_LIMIT)
            .await
            .map_err(|error| GitForgeError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(git_refusal(status, &body));
        }
        #[derive(serde::Deserialize)]
        struct CredentialAnswer {
            username: String,
            secret: String,
        }
        let answer: CredentialAnswer = serde_json::from_slice(&body).map_err(|error| {
            GitForgeError::Unavailable(format!(
                "the Model Gateway returned an unreadable git credential: {error}"
            ))
        })?;
        if answer.username.is_empty() || answer.secret.is_empty() {
            return Err(GitForgeError::Unavailable(
                "the Model Gateway returned an empty git credential".into(),
            ));
        }
        Ok(GitCredential {
            username: answer.username,
            secret: answer.secret,
        })
    }

    /// Repositories this caller can clone, listed by the gateway so this
    /// machine never holds a token for the read (decision 70).
    pub(crate) async fn list_repositories(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<GitHubRepository>, GitForgeError> {
        let Some(slot) = self.slot_for(owner)? else {
            return Err(GitForgeError::SignInRequired(
                "this machine holds no live Model Gateway session for you; sign in again".into(),
            ));
        };
        let subject = subject_of(&slot)?;
        let response = self
            .client
            .get(self.git_repositories_url.clone())
            .query(&[
                ("resource", self.resource.as_str()),
                ("attribution", "person"),
            ])
            .bearer_auth(subject.as_ref())
            .send()
            .await
            .map_err(|error| {
                GitForgeError::Unavailable(format!(
                    "the Model Gateway git-repositories request failed: {error}"
                ))
            })?;
        let status = response.status();
        let body = read_bounded(response, RESPONSE_LIMIT)
            .await
            .map_err(|error| GitForgeError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(git_refusal(status, &body));
        }
        #[derive(serde::Deserialize)]
        struct ListAnswer {
            #[serde(default)]
            repositories: Vec<RepoAnswer>,
        }
        #[derive(serde::Deserialize)]
        struct RepoAnswer {
            full_name: String,
            #[serde(default)]
            private: bool,
            #[serde(default)]
            description: Option<String>,
        }
        let answer: ListAnswer = serde_json::from_slice(&body).map_err(|error| {
            GitForgeError::Unavailable(format!(
                "the Model Gateway returned an unreadable git-repositories answer: {error}"
            ))
        })?;
        Ok(answer
            .repositories
            .into_iter()
            .filter(|repo| !repo.full_name.is_empty() && repo.full_name.contains('/'))
            .map(|repo| GitHubRepository {
                full_name: repo.full_name,
                private: repo.private,
                description: repo.description.filter(|value| !value.is_empty()),
            })
            .collect())
    }

    /// One probe of the gateway's git-forge surface with the caller's
    /// machine-bound token.
    ///
    /// The `attribution=person` parameter declares that this machine renders
    /// person attribution (decision 65): a connect-mode forge may answer with
    /// the caller's own identity instead of refusing. A gateway that predates
    /// the parameter ignores it.
    async fn fetch_git_forge(&self, subject: &str) -> Result<GitForgeIdentity, GitForgeError> {
        let response = self
            .client
            .get(self.git_forge_url.clone())
            .query(&[
                ("resource", self.resource.as_str()),
                ("attribution", "person"),
            ])
            .bearer_auth(subject)
            .send()
            .await
            .map_err(|error| {
                GitForgeError::Unavailable(format!(
                    "the Model Gateway git-forge probe failed: {error}"
                ))
            })?;
        let status = response.status();
        let body = read_bounded(response, RESPONSE_LIMIT)
            .await
            .map_err(|error| GitForgeError::Unavailable(error.to_string()))?;
        if !status.is_success() {
            return Err(git_refusal(status, &body));
        }
        #[derive(serde::Deserialize)]
        struct GitForgeAnswer {
            app_name: String,
            #[serde(default)]
            bot_login: Option<String>,
            #[serde(default)]
            attribution: Option<String>,
            #[serde(default)]
            acts_as: Option<String>,
            #[serde(default)]
            display_name: Option<String>,
            #[serde(default)]
            commit_email: Option<String>,
        }
        let answer: GitForgeAnswer = serde_json::from_slice(&body).map_err(|error| {
            GitForgeError::Unavailable(format!(
                "the Model Gateway returned an unreadable git-forge answer: {error}"
            ))
        })?;
        // Person attribution is trusted only when the gateway states it
        // (decision 65); an answer without the field is the App's bot
        // (decision 63). An attribution this build does not know is refused
        // rather than guessed — a wrong identity on screen is worse than a
        // retryable error.
        let attribution = match answer.attribution.as_deref() {
            None | Some("bot") => GitForgeAttribution::Bot {
                bot_login: answer.bot_login,
            },
            Some("person") => {
                let Some(login) = answer.acts_as.filter(|login| !login.is_empty()) else {
                    return Err(GitForgeError::Unavailable(
                        "the Model Gateway answered person attribution with no login".into(),
                    ));
                };
                GitForgeAttribution::Person {
                    login,
                    display_name: answer.display_name.filter(|name| !name.is_empty()),
                    commit_email: answer.commit_email.filter(|email| !email.is_empty()),
                }
            }
            Some(other) => {
                return Err(GitForgeError::Unavailable(format!(
                    "the Model Gateway answered an attribution this machine does not know: {other}"
                )));
            }
        };
        Ok(GitForgeIdentity {
            app_name: answer.app_name,
            attribution,
        })
    }

    /// The recorded slot for `owner`, or `None` for a caller this process
    /// has never authenticated.
    fn slot_for(&self, owner: &OwnerId) -> Result<Option<Arc<UserSlot>>, GitForgeError> {
        let users = self.users.lock().map_err(|_| {
            GitForgeError::Unavailable(
                "on-behalf-of gateway state is unavailable in this process".into(),
            )
        })?;
        Ok(users.get(owner).cloned())
    }

    /// Force the next [`OboGateway::git_forge_identity`] to probe, from tests.
    #[cfg(test)]
    async fn expire_git_forge_for_test(&self, owner: &OwnerId) {
        let slot = {
            let users = self.users.lock().unwrap();
            users.get(owner).cloned()
        };
        if let Some(slot) = slot {
            if let Some(held) = slot.git_forge.lock().await.as_mut() {
                held.fetched_at_unix = 0;
            }
        }
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

/// The newest machine-bound bearer recorded in `slot`.
fn subject_of(slot: &UserSlot) -> Result<Arc<str>, GitForgeError> {
    slot.subject
        .lock()
        .map(|subject| subject.clone())
        .map_err(|_| {
            GitForgeError::Unavailable(
                "on-behalf-of gateway state is unavailable in this process".into(),
            )
        })
}

/// Turn a refused git-forge or git-credential response into its typed cause.
///
/// The gateway's stable codes map one to one; a response without a readable
/// code falls back on the status — a bare 404 is a gateway too old to serve
/// the surface, and a bare 401/403 is a dead session. A body that decodes to
/// nothing recognizable is still a refusal, never a reason to keep going.
fn git_refusal(status: reqwest::StatusCode, body: &[u8]) -> GitForgeError {
    if let Ok(refusal) = serde_json::from_slice::<OAuthError>(body) {
        let detail = refusal
            .error_description
            .clone()
            .unwrap_or_else(|| refusal.error.clone());
        match refusal.error.as_str() {
            "no_git_forge" => return GitForgeError::NoGitForge,
            "ambiguous_git_forge" => return GitForgeError::AmbiguousGitForge,
            "connect_mode_forge" => return GitForgeError::ConnectModeForge,
            "not_connected" => {
                // The refusal names where the caller connects, when the
                // gateway knows the page. Parsed beside the OAuth error
                // shape rather than inside it: the field is this refusal's
                // alone.
                #[derive(serde::Deserialize)]
                struct NotConnectedAnswer {
                    #[serde(default)]
                    connect_url: Option<String>,
                }
                let connect_url = serde_json::from_slice::<NotConnectedAnswer>(body)
                    .ok()
                    .and_then(|answer| answer.connect_url)
                    .filter(|url| !url.is_empty());
                return GitForgeError::NotConnected { connect_url };
            }
            "forge_app_not_installed" => return GitForgeError::ForgeAppNotInstalled,
            "repository_not_installed" => return GitForgeError::RepositoryNotInstalled,
            "forge_credential_mint_failed" => {
                return GitForgeError::Unavailable(format!(
                    "the deployment's git forge could not mint a credential: {detail}"
                ));
            }
            _ => {
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    return GitForgeError::SignInRequired(format!(
                        "the Model Gateway refused your session for git credentials: {detail}"
                    ));
                }
                return GitForgeError::Unavailable(format!(
                    "the Model Gateway refused the git-credential request: {detail}"
                ));
            }
        }
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return GitForgeError::Unavailable(
            "the Model Gateway does not serve git credentials; update the deployment".into(),
        );
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return GitForgeError::SignInRequired(
            "the Model Gateway refused your session for git credentials; sign in again".into(),
        );
    }
    GitForgeError::Unavailable(format!(
        "the Model Gateway git-credential request failed with status {status}"
    ))
}

/// Per-caller git-forge lending, as code mode consumes it (decision 63).
///
/// A seam rather than a concrete handle so code-mode tests fake the
/// gateway's answers without a live server behind them.
#[async_trait]
pub(crate) trait GitCredentialLender: Send + Sync {
    /// The identity work would land as, or the named reason none is offered.
    async fn git_forge_identity(&self, owner: &OwnerId) -> Result<GitForgeIdentity, GitForgeError>;

    /// Borrow one repository-scoped credential for one git operation against
    /// `repository` (`owner/repo`).
    async fn git_credential(
        &self,
        owner: &OwnerId,
        repository: &str,
    ) -> Result<GitCredential, GitForgeError>;

    /// Repositories this caller can clone (decision 70).
    async fn list_repositories(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<GitHubRepository>, GitForgeError>;
}

#[async_trait]
impl GitCredentialLender for OboGateway {
    async fn git_forge_identity(&self, owner: &OwnerId) -> Result<GitForgeIdentity, GitForgeError> {
        OboGateway::git_forge_identity(self, owner).await
    }

    async fn git_credential(
        &self,
        owner: &OwnerId,
        repository: &str,
    ) -> Result<GitCredential, GitForgeError> {
        OboGateway::git_credential(self, owner, repository).await
    }

    async fn list_repositories(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<GitHubRepository>, GitForgeError> {
        OboGateway::list_repositories(self, owner).await
    }
}

/// Scripted git-forge lending for code-mode tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// A lender whose answers are set by the test: an identity (or refusal)
    /// for the probe, an optional refusal for mints, and a record of every
    /// repository a mint was asked for.
    pub(crate) struct FakeLender {
        pub(crate) identity: std::sync::Mutex<Result<GitForgeIdentity, GitForgeError>>,
        pub(crate) mint_refusal: std::sync::Mutex<Option<GitForgeError>>,
        pub(crate) minted: std::sync::Mutex<Vec<String>>,
        pub(crate) listed: std::sync::Mutex<Result<Vec<GitHubRepository>, GitForgeError>>,
    }

    impl FakeLender {
        /// A deployment whose forge serves this caller as `bot_login`.
        pub(crate) fn offering(bot_login: &str) -> Self {
            Self {
                identity: std::sync::Mutex::new(Ok(GitForgeIdentity {
                    app_name: "Acme Forge".to_owned(),
                    attribution: GitForgeAttribution::Bot {
                        bot_login: Some(bot_login.to_owned()),
                    },
                })),
                mint_refusal: std::sync::Mutex::new(None),
                minted: std::sync::Mutex::new(Vec::new()),
                listed: std::sync::Mutex::new(Ok(Vec::new())),
            }
        }

        /// A deployment whose forge acts as this caller's own account
        /// (decision 65).
        pub(crate) fn offering_person(login: &str) -> Self {
            Self {
                identity: std::sync::Mutex::new(Ok(GitForgeIdentity {
                    app_name: "Acme Forge".to_owned(),
                    attribution: GitForgeAttribution::Person {
                        login: login.to_owned(),
                        display_name: Some("Mira Chen".to_owned()),
                        commit_email: Some(format!("8675309+{login}@users.noreply.github.com")),
                    },
                })),
                mint_refusal: std::sync::Mutex::new(None),
                minted: std::sync::Mutex::new(Vec::new()),
                listed: std::sync::Mutex::new(Ok(vec![GitHubRepository {
                    full_name: format!("{login}/notes"),
                    private: true,
                    description: Some("scratch".into()),
                }])),
            }
        }

        /// A deployment whose gateway refuses both surfaces with `error`.
        pub(crate) fn refusing(error: GitForgeError) -> Self {
            Self {
                identity: std::sync::Mutex::new(Err(error.clone())),
                mint_refusal: std::sync::Mutex::new(Some(error.clone())),
                minted: std::sync::Mutex::new(Vec::new()),
                listed: std::sync::Mutex::new(Err(error)),
            }
        }

        /// Every repository a mint was asked for, in order.
        pub(crate) fn minted(&self) -> Vec<String> {
            self.minted.lock().expect("minted").clone()
        }
    }

    #[async_trait]
    impl GitCredentialLender for FakeLender {
        async fn git_forge_identity(
            &self,
            _owner: &OwnerId,
        ) -> Result<GitForgeIdentity, GitForgeError> {
            self.identity.lock().expect("identity").clone()
        }

        async fn git_credential(
            &self,
            _owner: &OwnerId,
            repository: &str,
        ) -> Result<GitCredential, GitForgeError> {
            self.minted
                .lock()
                .expect("minted")
                .push(repository.to_owned());
            match self.mint_refusal.lock().expect("mint refusal").clone() {
                Some(error) => Err(error),
                None => Ok(GitCredential {
                    username: "x-access-token".to_owned(),
                    secret: "ghs_fake_borrowed".to_owned(),
                }),
            }
        }

        async fn list_repositories(
            &self,
            _owner: &OwnerId,
        ) -> Result<Vec<GitHubRepository>, GitForgeError> {
            self.listed.lock().expect("listed").clone()
        }
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
        /// How many git-forge probes it has served.
        forge_probes: Arc<AtomicUsize>,
        /// How many git credentials it has minted.
        credential_mints: Arc<AtomicUsize>,
        /// The `error` code the git surfaces refuse with, with its status, or
        /// empty to succeed.
        git_refusal: Arc<std::sync::Mutex<Option<(u16, String)>>>,
        /// The probe answer the git-forge route serves, or `None` for the
        /// default bot-attributed answer.
        forge_answer: Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    }

    impl FakeGateway {
        fn new() -> Self {
            Self {
                mints: Arc::new(AtomicUsize::new(0)),
                lifetime: Arc::new(AtomicUsize::new(3600)),
                refusal: Arc::new(std::sync::Mutex::new(String::new())),
                latency: Duration::ZERO,
                catalog_reads: Arc::new(AtomicUsize::new(0)),
                forge_probes: Arc::new(AtomicUsize::new(0)),
                credential_mints: Arc::new(AtomicUsize::new(0)),
                git_refusal: Arc::new(std::sync::Mutex::new(None)),
                forge_answer: Arc::new(std::sync::Mutex::new(None)),
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
            let forge_state = self.clone();
            let app = app.route(
                "/api/v1/tidebreak/git-forge",
                axum::routing::get(
                    move |headers: axum::http::HeaderMap,
                          query: axum::extract::Query<HashMap<String, String>>| {
                        let state = forge_state.clone();
                        async move {
                            // The probe rides the caller's machine-bound
                            // token directly, never an exchanged capability,
                            // and names this machine's exact resource.
                            let bearer = machine_bound_bearer(&headers);
                            assert!(
                                bearer.starts_with("mg_at_"),
                                "the probe must present the machine-bound subject, got {bearer:?}"
                            );
                            assert_eq!(
                                query.get("resource").map(String::as_str),
                                Some(TEST_RESOURCE)
                            );
                            // The machine declares it renders person
                            // attribution (decision 65) — asserted server-side
                            // so a drifted client fails the test.
                            assert_eq!(
                                query.get("attribution").map(String::as_str),
                                Some("person")
                            );
                            state.forge_probes.fetch_add(1, Ordering::SeqCst);
                            if let Some(refused) = state.next_git_refusal() {
                                return refused;
                            }
                            let scripted = state
                                .forge_answer
                                .lock()
                                .map(|answer| answer.clone())
                                .unwrap_or_default();
                            Json(scripted.unwrap_or_else(|| {
                                serde_json::json!({
                                    "app_id": "0193a1c0-0000-7000-8000-000000000001",
                                    "app_name": "Acme Forge",
                                    "bot_login": "acme-ship[bot]",
                                })
                            }))
                            .into_response()
                        }
                    },
                ),
            );
            let credential_state = self.clone();
            let app = app.route(
                "/api/v1/tidebreak/git-credential",
                axum::routing::post(
                    move |headers: axum::http::HeaderMap, Json(body): Json<serde_json::Value>| {
                        let state = credential_state.clone();
                        async move {
                            let bearer = machine_bound_bearer(&headers);
                            assert!(
                                bearer.starts_with("mg_at_"),
                                "the mint must present the machine-bound subject, got {bearer:?}"
                            );
                            assert_eq!(body["resource"], TEST_RESOURCE);
                            let repository =
                                body["repository"].as_str().unwrap_or_default().to_owned();
                            assert!(
                                repository.split('/').count() == 2,
                                "the mint must name owner/repo, got {repository:?}"
                            );
                            // Decision 65's opt-in rides every mint.
                            assert_eq!(body["attribution"], "person");
                            state.credential_mints.fetch_add(1, Ordering::SeqCst);
                            if let Some(refused) = state.next_git_refusal() {
                                return refused;
                            }
                            let serial = state.credential_mints.load(Ordering::SeqCst);
                            Json(serde_json::json!({
                                "username": "x-access-token",
                                "secret": format!("ghs_fake_{serial}_for_{repository}"),
                                "expires_at": "2027-01-01T00:00:00Z",
                                "app_id": "0193a1c0-0000-7000-8000-000000000001",
                            }))
                            .into_response()
                        }
                    },
                ),
            );
            let list_state = self.clone();
            let app = app.route(
                "/api/v1/tidebreak/git-repositories",
                axum::routing::get(
                    move |headers: axum::http::HeaderMap,
                          query: axum::extract::Query<HashMap<String, String>>| {
                        let state = list_state.clone();
                        async move {
                            let bearer = machine_bound_bearer(&headers);
                            assert!(
                                bearer.starts_with("mg_at_"),
                                "the list must present the machine-bound subject, got {bearer:?}"
                            );
                            assert_eq!(
                                query.get("resource").map(String::as_str),
                                Some(TEST_RESOURCE)
                            );
                            assert_eq!(
                                query.get("attribution").map(String::as_str),
                                Some("person")
                            );
                            if let Some(refused) = state.next_git_refusal() {
                                return refused;
                            }
                            Json(serde_json::json!({
                                "repositories": [{
                                    "full_name": "acme/ship",
                                    "private": true,
                                    "description": "the product",
                                }]
                            }))
                            .into_response()
                        }
                    },
                ),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let inference = Arc::new(
                OboGateway::new(&format!("http://{address}"), TEST_RESOURCE.to_owned()).unwrap(),
            );
            (inference, server)
        }

        /// The configured git refusal as a ready response, or `None`.
        ///
        /// A `not_connected` refusal carries the connect page beside the
        /// OAuth error shape, exactly as the gateway sends it.
        fn next_git_refusal(&self) -> Option<axum::response::Response> {
            let held = self
                .git_refusal
                .lock()
                .map(|refusal| refusal.clone())
                .unwrap_or_default()?;
            let (status, code) = held;
            let mut body = serde_json::json!({
                "error": code,
                "error_description": "the fake gateway refused",
            });
            if code == "not_connected" {
                body["connect_url"] =
                    serde_json::Value::String("https://gateway.example/account/apps".to_owned());
            }
            Some(
                (
                    axum::http::StatusCode::from_u16(status)
                        .unwrap_or(axum::http::StatusCode::BAD_REQUEST),
                    Json(body),
                )
                    .into_response(),
            )
        }
    }

    /// The bearer half of an Authorization header.
    fn machine_bound_bearer(headers: &axum::http::HeaderMap) -> String {
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_owned()
    }

    /// The machine resource every git request must name.
    const TEST_RESOURCE: &str =
        "tidebreak:feedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeedfeed";

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
        assert!(
            OboGateway::from_config(&config).is_err(),
            "a gateway deployment without a public URL cannot name its machine resource"
        );

        config.public_url = Some("https://machine.example".to_owned());
        let inference = OboGateway::from_config(&config).unwrap().unwrap();
        assert_eq!(inference.gateway_base_url(), "https://gateway.example");
        assert_eq!(
            inference.resource,
            tidebreak_core::config::tidebreak_machine_resource("https://machine.example"),
            "the git-credential resource is the machine's own"
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
        config.public_url = Some("https://machine.example".to_owned());

        let inference = OboGateway::from_config(&config).unwrap().unwrap();
        assert_eq!(
            inference.token_url.as_str(),
            "https://gateway.internal/oauth/token"
        );
        assert_eq!(inference.gateway_base_url(), "https://gateway.internal");
    }

    /// A gateway deployed under a subpath keeps its prefix.
    #[test]
    fn a_subpath_deployment_keeps_its_prefix() {
        let inference =
            OboGateway::new("https://example.test/gateway", TEST_RESOURCE.to_owned()).unwrap();
        assert_eq!(
            inference.token_url.as_str(),
            "https://example.test/gateway/oauth/token"
        );
        assert_eq!(
            inference.git_credential_url.as_str(),
            "https://example.test/gateway/api/v1/tidebreak/git-credential"
        );
        assert_eq!(inference.gateway_base_url(), "https://example.test/gateway");
    }

    /// A URL that cannot carry a server-to-server credential is refused at
    /// assembly, not at the first turn.
    #[test]
    fn an_unusable_gateway_url_is_refused_at_assembly() {
        let resource = || TEST_RESOURCE.to_owned();
        assert!(OboGateway::new("https://user:pass@example.test", resource()).is_err());
        assert!(OboGateway::new("https://example.test?probe=1", resource()).is_err());
        assert!(OboGateway::new("http://gateway.example", resource()).is_err());
        assert!(OboGateway::new("http://127.0.0.1:8080", resource()).is_ok());
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

    /// Decision 63's happy path: a recorded caller borrows a
    /// repository-scoped credential, minted per operation — two borrows are
    /// two mints, because nothing durable is held.
    #[tokio::test]
    async fn a_caller_borrows_a_dying_credential_per_operation() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        let first = obo.git_credential(&alice, "acme/demo").await.unwrap();
        assert_eq!(first.username, "x-access-token");
        assert!(first.secret.ends_with("for_acme/demo"));
        let second = obo.git_credential(&alice, "acme/demo").await.unwrap();
        assert_ne!(first.secret, second.secret, "each operation borrows anew");
        assert_eq!(gateway.credential_mints.load(Ordering::SeqCst), 2);
        assert_eq!(gateway.served(), 0, "no exchange is involved");
        server.abort();
    }

    /// The probe names the forge identity for the UI and stays fresh across
    /// the rapid refetches its surfaces make.
    #[tokio::test]
    async fn the_probe_names_the_forge_identity_and_holds_it_fresh() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        let identity = obo.git_forge_identity(&alice).await.unwrap();
        assert_eq!(identity.app_name, "Acme Forge");
        assert_eq!(
            identity.attribution,
            GitForgeAttribution::Bot {
                bot_login: Some("acme-ship[bot]".to_owned())
            },
            "an answer without an attribution field is the App's bot"
        );
        obo.git_forge_identity(&alice).await.unwrap();
        assert_eq!(
            gateway.forge_probes.load(Ordering::SeqCst),
            1,
            "a fresh identity must not re-probe"
        );

        obo.expire_git_forge_for_test(&alice).await;
        obo.git_forge_identity(&alice).await.unwrap();
        assert_eq!(gateway.forge_probes.load(Ordering::SeqCst), 2);
        server.abort();
    }

    /// Decision 65: a gateway that states person attribution is read as the
    /// caller's own account, and an attribution this build does not know is
    /// refused rather than guessed.
    #[tokio::test]
    async fn the_probe_reads_person_attribution_only_when_the_gateway_states_it() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        *gateway.forge_answer.lock().unwrap() = Some(serde_json::json!({
            "app_id": "0193a1c0-0000-7000-8000-000000000001",
            "app_name": "Acme Forge",
            "attribution": "person",
            "acts_as": "mira-chen",
            "display_name": "Mira Chen",
            "commit_email": "8675309+mira-chen@users.noreply.github.com",
        }));
        let identity = obo.git_forge_identity(&alice).await.unwrap();
        assert_eq!(
            identity.attribution,
            GitForgeAttribution::Person {
                login: "mira-chen".to_owned(),
                display_name: Some("Mira Chen".to_owned()),
                commit_email: Some("8675309+mira-chen@users.noreply.github.com".to_owned()),
            }
        );

        *gateway.forge_answer.lock().unwrap() = Some(serde_json::json!({
            "app_id": "0193a1c0-0000-7000-8000-000000000001",
            "app_name": "Acme Forge",
            "attribution": "org",
            "acts_as": "acme",
        }));
        obo.expire_git_forge_for_test(&alice).await;
        assert!(
            matches!(
                obo.git_forge_identity(&alice).await.unwrap_err(),
                GitForgeError::Unavailable(_)
            ),
            "an unknown attribution must refuse, never guess an identity"
        );
        server.abort();
    }

    /// Decision 65: the not-connected refusal is typed, carries the gateway
    /// page where the caller connects, and is held like any settled answer.
    #[tokio::test]
    async fn a_not_connected_refusal_names_the_connect_page_and_is_held() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        *gateway.git_refusal.lock().unwrap() = Some((403, "not_connected".to_owned()));
        let refusal = obo.git_forge_identity(&alice).await.unwrap_err();
        assert_eq!(
            refusal,
            GitForgeError::NotConnected {
                connect_url: Some("https://gateway.example/account/apps".to_owned()),
            }
        );
        obo.git_forge_identity(&alice).await.unwrap_err();
        assert_eq!(
            gateway.forge_probes.load(Ordering::SeqCst),
            1,
            "not-connected is settled until the caller acts; it must not re-probe"
        );
        assert_eq!(
            obo.git_credential(&alice, "acme/demo").await.unwrap_err(),
            refusal,
            "the mint refuses with the same typed cause"
        );
        server.abort();
    }

    /// The gateway's stable refusal codes arrive typed, and a settled refusal
    /// is held like a settled identity — "not offered" is the common case and
    /// must not turn every render into gateway traffic.
    #[tokio::test]
    async fn a_named_git_refusal_is_typed_and_held() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        *gateway.git_refusal.lock().unwrap() = Some((404, "no_git_forge".to_owned()));
        assert_eq!(
            obo.git_forge_identity(&alice).await.unwrap_err(),
            GitForgeError::NoGitForge
        );
        assert_eq!(
            obo.git_forge_identity(&alice).await.unwrap_err(),
            GitForgeError::NoGitForge
        );
        assert_eq!(
            gateway.forge_probes.load(Ordering::SeqCst),
            1,
            "a settled refusal must not re-probe"
        );

        *gateway.git_refusal.lock().unwrap() = Some((403, "connect_mode_forge".to_owned()));
        obo.expire_git_forge_for_test(&alice).await;
        assert_eq!(
            obo.git_forge_identity(&alice).await.unwrap_err(),
            GitForgeError::ConnectModeForge
        );

        *gateway.git_refusal.lock().unwrap() = Some((422, "repository_not_installed".to_owned()));
        assert_eq!(
            obo.git_credential(&alice, "acme/demo").await.unwrap_err(),
            GitForgeError::RepositoryNotInstalled
        );
        server.abort();
    }

    /// A dead session is sign-in-required on the git surfaces too, and a
    /// caller this process never authenticated borrows nothing.
    #[tokio::test]
    async fn git_credentials_fail_closed_without_a_live_session() {
        let gateway = FakeGateway::new();
        let (obo, server) = gateway.clone().start().await;
        let alice = owner("user:alice");
        obo.record_caller(&alice, "mg_at_alice".into());

        *gateway.git_refusal.lock().unwrap() = Some((401, "invalid_token".to_owned()));
        assert!(matches!(
            obo.git_credential(&alice, "acme/demo").await.unwrap_err(),
            GitForgeError::SignInRequired(_)
        ));

        let stranger = owner("user:stranger");
        assert!(matches!(
            obo.git_credential(&stranger, "acme/demo")
                .await
                .unwrap_err(),
            GitForgeError::SignInRequired(_)
        ));
        assert!(matches!(
            obo.git_forge_identity(&stranger).await.unwrap_err(),
            GitForgeError::SignInRequired(_)
        ));
        assert_eq!(gateway.credential_mints.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
