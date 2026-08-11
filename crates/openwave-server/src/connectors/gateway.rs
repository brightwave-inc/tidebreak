//! Loopback OAuth client for a model-gateway deployment.
//!
//! The gateway is a public-client authorization server: PKCE S256 is
//! mandatory, there is no client secret, redirects are loopback-only, and
//! tokens are opaque with rotation and reuse detection on refresh. This
//! module mirrors the contract its own CLI uses, with one addition: an
//! explicit installation pin so a swapped-out deployment behind the same URL
//! is detected instead of silently trusted.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use openwave_core::{AgentError, Result, SecretProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};

/// The audience for profile and entitlement reads (`/api/v1/cli/*`).
pub const RESOURCE_CONTROL: &str = "control";
/// The audience for inference calls on the gateway's compatible routes.
pub const RESOURCE_LLM: &str = "llm";

const SCOPE: &str = "openid profile offline_access models:read inference:invoke";
const SECRET_KEY: &str = "gateway.credentials_v1";
/// Refresh an access token this close to expiry instead of using it.
const EXPIRY_LEEWAY_SECONDS: u64 = 60;
const SIGN_IN_REQUIRED_PREFIX: &str = "gateway sign-in required";

/// True when an operation failed because there is no usable gateway session —
/// never signed in, session revoked, or refresh-token reuse detected. Callers
/// should surface a reconnect affordance instead of treating this as a fault.
#[must_use]
pub fn is_sign_in_required(error: &AgentError) -> bool {
    matches!(error, AgentError::Authentication(_))
}

fn sign_in_required(detail: &str) -> AgentError {
    AgentError::Authentication(format!("{SIGN_IN_REQUIRED_PREFIX}: {detail}"))
}

fn gateway_error(context: &str, detail: impl std::fmt::Display) -> AgentError {
    AgentError::config(format!("model-gateway {context}: {detail}"))
}

/// Whether a URL's host is the local machine.
///
/// Cleartext `http` is only tolerated for a gateway running beside the app —
/// a developer deployment on `localhost`. Everything else carries an OAuth
/// authorization code and opaque tokens, so it must be `https`; a provision
/// link parked on a webpage would otherwise be able to point the whole
/// exchange at an attacker-readable origin. `localhost` counts because the
/// resolver is required to map it to a loopback address; other names do not,
/// since what they resolve to is not knowable here.
pub(crate) fn is_loopback_url(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

/// How this client identifies itself to the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayAuthConfig {
    base_url: reqwest::Url,
    client_id: String,
    /// Attribution declared on refresh. `None` lets the gateway default to
    /// `generic`; the gateway rejects names outside its known-client set.
    client_name: Option<String>,
}

impl GatewayAuthConfig {
    /// The registered `openwave` public client: the gateway's client registry
    /// names it on the consent page and attributes its sessions to OpenWave.
    /// The name is also declared on every refresh, because usage attribution
    /// reads the access token's client name — omitting it there would leave
    /// inference calls attributed `generic` despite the registered client id.
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_client(base_url, "openwave", Some("openwave".to_string()))
    }

    fn with_client(base_url: &str, client_id: &str, client_name: Option<String>) -> Result<Self> {
        let base_url = reqwest::Url::parse(base_url)
            .map_err(|_| gateway_error("configuration", "base URL is invalid"))?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(gateway_error(
                "configuration",
                "base URL must use http or https",
            ));
        }
        if base_url.scheme() == "http" && !is_loopback_url(&base_url) {
            return Err(gateway_error(
                "configuration",
                "base URL must use https unless the gateway is on loopback",
            ));
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            return Err(gateway_error(
                "configuration",
                "base URL must not embed credentials",
            ));
        }
        Ok(Self {
            base_url,
            client_id: client_id.to_string(),
            client_name,
        })
    }

    /// The configured gateway origin.
    #[must_use]
    pub fn base_url(&self) -> &reqwest::Url {
        &self.base_url
    }
}

/// Unauthenticated deployment identity from `/api/v1/meta`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayMeta {
    pub api_version: String,
    pub installation_id: String,
    pub gateway_version: String,
    pub public_url: String,
    pub auth_mode: String,
}

/// The authenticated user, from `/api/v1/cli/me`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GatewayIdentity {
    pub user_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub session_id: String,
    pub installation_id: String,
}

/// One entitled model from `/api/v1/cli/models`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayModel {
    pub id: String,
    /// The gateway inference protocol this model is served through.
    ///
    /// Older gateways exposed only Anthropic Messages and omitted the field,
    /// so absence preserves that route. Newer deployments report their exact
    /// protocol (`anthropic_messages` or `openai_chat_completions`) so clients
    /// can select the matching compatibility surface.
    #[serde(default = "default_gateway_model_protocol")]
    pub protocol: String,
    /// The provider-side id this deployment routes to, when it differs from
    /// `id` — a deployment alias such as `us.anthropic.claude-opus-5` behind
    /// the gateway id `anthropic-us-claude-opus-5`. Optional because gateways
    /// older than this field simply omit it.
    #[serde(default)]
    pub upstream_id: Option<String>,
    pub name: String,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

fn default_gateway_model_protocol() -> String {
    "anthropic_messages".to_string()
}

/// One entitled connected app from `/api/v1/cli/apps`: the apps the user
/// reaches through team grants, and the slugs of the MCP endpoints that
/// aggregate each one (the `mcp:<slug>` resources a client may request).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayApp {
    pub id: String,
    pub name: String,
    pub app_kind: String,
    pub enabled: bool,
    pub mcp_endpoint_slugs: Vec<String>,
}

/// One operation a gateway connected app declares, from
/// `/api/v1/cli/apps/{app_id}/operations`.
///
/// Deliberately open (no `deny_unknown_fields`): the gateway's catalog read is
/// additive and still growing — a catalog-document hash is the field most
/// likely to arrive next — and a client that refused an enriched payload would
/// break on the deployment's next release rather than ignoring what it does not
/// yet use.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayOperationSummary {
    /// The operation id a manifest binding pins.
    pub operation_id: String,
    /// The HTTP method the gateway executes it with, for display.
    pub method: String,
    /// The gateway's own one-line description, when it has one.
    #[serde(default)]
    pub summary: Option<String>,
}

/// What one shared-app invoke relay came back as.
///
/// Three outcomes, because the frame has three different things to do with
/// them: render the response, prompt the viewer to connect the bound app at
/// the gateway, or report a refusal. Nothing else is representable — a
/// transport or protocol failure is the `Err` half, and a `404` on the route
/// is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayInvokeOutcome {
    /// The gateway executed the operation. Status, content type, and body are
    /// the upstream API's, passed through: the body is base64 so binary
    /// responses survive JSON, exactly as the local REST path returns them.
    Executed {
        status: u16,
        content_type: Option<String>,
        body_base64: String,
    },
    /// The viewer has no credential the gateway could resolve for the bound
    /// app — its typed `authorization_required`. The viewer must connect the
    /// app at the gateway; no local action can supply it.
    AuthorizationRequired { message: String },
    /// The gateway holds no consent for this shared app — its typed
    /// `consent_required`. Unlike the two above, the *host* can resolve this
    /// without the viewer: the local grant ladder has already run, so the
    /// relay re-states the author's consent to the gateway and calls again.
    ConsentRequired { message: String },
    /// The gateway refused the call for any other typed reason (an
    /// unconsented shared app, a manifest the operation is not in, a
    /// credential resolution failure). The message is the gateway's own.
    Refused { message: String },
}

/// What one shared-app registration call — a create or a revision append —
/// came back as.
///
/// Closed on purpose: the registry turns each of these into a durable
/// decision (persist the mapping, retry once under a suffixed slug, or give
/// up honestly), and a fourth reading would need its own decision rather than
/// a free-form message. A `404` is the `Ok(None)` half, as everywhere else on
/// this client: a deployment that does not serve registration yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayRegistrationOutcome {
    /// The gateway holds the shared app at `shared_app_id`, currently serving
    /// revision `revision_id`.
    Registered {
        shared_app_id: String,
        revision_id: String,
    },
    /// The requested slug is already taken at this deployment — the one
    /// failure the caller can resolve by asking again differently. The
    /// message is the gateway's own, so a caller that cannot resolve it can
    /// still report it in the gateway's words.
    SlugTaken { message: String },
    /// The gateway refused for any other typed reason (a disabled app, a
    /// manifest it will not accept). The message is the gateway's own.
    Refused { message: String },
}

/// What one shared-app consent relay came back as.
///
/// `RevisionMoved` is the only outcome the caller acts on beyond success: the
/// pinned revision is no longer the one the gateway serves, so the pin has to
/// be re-established before consent can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayConsentOutcome {
    /// The gateway recorded consent for the named revision.
    Consented,
    /// The pinned revision is stale; the gateway serves another one.
    RevisionMoved,
    /// The gateway refused for any other typed reason, in its own words.
    Refused { message: String },
}

/// One team the authenticated user belongs to, from `/api/v1/cli/teams`.
///
/// Exactly the set a publish may name: the gateway accepts a publish to a
/// team the caller is a member of and refuses every other one, so this read
/// is what a publish affordance offers rather than a broader directory.
/// `enabled` is carried because a disabled team is still a membership — it is
/// shown and refused rather than hidden, so the author is never left
/// wondering where a team went.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GatewayTeam {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub enabled: bool,
}

/// What one shared-app publish came back as.
///
/// `AppDisabled` is separated from the rest because it is the one refusal
/// that is not about the bundle or the team: the shared app itself is
/// switched off at the deployment, which an author resolves with an operator
/// rather than by editing anything. Every other typed refusal — a bundle
/// naming host-local bridge verbs the gateway cannot serve, a team the
/// caller does not belong to — carries the gateway's own code and message,
/// because the gateway is the only thing that knows what it objected to. A
/// `404` is the `Ok(None)` half, as everywhere else on this client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayPublishOutcome {
    /// The gateway published the app's current revision to the named team.
    Published,
    /// The shared app is disabled at this deployment.
    AppDisabled { message: String },
    /// The gateway refused for any other typed reason, in its own words.
    Refused { code: String, message: String },
}

/// One minted access/refresh pair, resource-bound.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_unix: u64,
    pub scope: String,
    pub resource: String,
    pub installation_id: String,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TokenSet")
            .field("resource", &self.resource)
            .field("scope", &self.scope)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("installation_id", &self.installation_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedAccessToken {
    token: String,
    expires_at_unix: u64,
    scope: String,
}

impl CachedAccessToken {
    fn is_fresh(&self) -> bool {
        self.expires_at_unix > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS)
    }
}

/// The durable signed-in state, serialized as one keychain secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayCredentials {
    pub base_url: String,
    pub installation_id: String,
    pub user_id: String,
    pub account_hint: Option<String>,
    refresh_token: String,
    access_tokens: BTreeMap<String, CachedAccessToken>,
}

impl GatewayCredentials {
    /// Whether these credentials were minted against `base_url`. Trailing
    /// slashes are normalized the same way [`GatewayAuth::endpoint`] does, so
    /// a subpath deployment retyped with or without one never reads as a
    /// different gateway. A base URL that does not parse matches nothing.
    #[must_use]
    pub fn matches_base_url(&self, base_url: &str) -> bool {
        fn normalized(url: &reqwest::Url) -> String {
            let mut base = url.to_string();
            if !base.ends_with('/') {
                base.push('/');
            }
            base
        }
        match (
            reqwest::Url::parse(&self.base_url),
            reqwest::Url::parse(base_url),
        ) {
            (Ok(stored), Ok(expected)) => normalized(&stored) == normalized(&expected),
            _ => false,
        }
    }
}

impl std::fmt::Debug for GatewayCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GatewayCredentials")
            .field("base_url", &self.base_url)
            .field("installation_id", &self.installation_id)
            .field("user_id", &self.user_id)
            .field("account_hint", &self.account_hint)
            .field(
                "cached_resources",
                &self.access_tokens.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

/// Whether a gateway session is stored, without constructing a vault.
///
/// For readiness surfaces (`has_credential`-style projections) that hold only
/// a borrowed [`SecretProvider`].
pub async fn has_stored_credentials(secrets: &dyn SecretProvider) -> bool {
    matches!(
        secrets.get_secret(SECRET_KEY).await,
        Ok(Some(raw)) if serde_json::from_str::<GatewayCredentials>(&raw).is_ok()
    )
}

/// Whether a gateway session minted against `base_url` is stored.
///
/// The deployment-matched form of [`has_stored_credentials`], for readiness
/// surfaces on a managed profile: a session left by a superseded deployment
/// must not read as a credential for the policy's current one — every route
/// and token path already refuses it.
pub async fn has_stored_credentials_for(secrets: &dyn SecretProvider, base_url: &str) -> bool {
    matches!(
        secrets.get_secret(SECRET_KEY).await,
        Ok(Some(raw)) if serde_json::from_str::<GatewayCredentials>(&raw)
            .is_ok_and(|credentials| credentials.matches_base_url(base_url))
    )
}

/// Keychain-backed storage for [`GatewayCredentials`].
#[derive(Clone)]
pub struct CredentialVault {
    secrets: Arc<dyn SecretProvider>,
}

impl CredentialVault {
    #[must_use]
    pub fn new(secrets: Arc<dyn SecretProvider>) -> Self {
        Self { secrets }
    }

    pub async fn load(&self) -> Result<Option<GatewayCredentials>> {
        let Some(raw) = self.secrets.get_secret(SECRET_KEY).await? else {
            return Ok(None);
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|_| gateway_error("credentials", "stored credentials are unreadable"))
    }

    pub async fn save(&self, credentials: &GatewayCredentials) -> Result<()> {
        let raw = serde_json::to_string(credentials)?;
        self.secrets.set_secret(SECRET_KEY, &raw).await
    }

    pub async fn clear(&self) -> Result<()> {
        self.secrets.delete_secret(SECRET_KEY).await
    }
}

/// Stateless HTTP client for the gateway's OAuth and CLI endpoints.
#[derive(Clone)]
pub struct GatewayAuth {
    config: GatewayAuthConfig,
    http: reqwest::Client,
}

impl GatewayAuth {
    pub fn new(config: GatewayAuthConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| gateway_error("client", error))?;
        Ok(Self { config, http })
    }

    /// Fetch the deployment identity used for installation pinning.
    pub async fn meta(&self) -> Result<GatewayMeta> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/meta")?)
            .send()
            .await
            .map_err(|error| gateway_error("metadata request", error.without_url()))?;
        decode_json(response, "metadata request").await
    }

    /// Bind the loopback listener and produce the URL the user's browser
    /// must visit. The listener stays alive inside the returned
    /// [`PendingSignIn`] until [`PendingSignIn::finish`] resolves.
    pub async fn start_sign_in(&self) -> Result<PendingSignIn> {
        let meta = self.meta().await?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| gateway_error("sign-in", error))?;
        let port = listener
            .local_addr()
            .map_err(|error| gateway_error("sign-in", error))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let verifier = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let state = random_token();

        let mut authorization_url = self.endpoint("/oauth/authorize")?;
        authorization_url
            .query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("scope", SCOPE);

        Ok(PendingSignIn {
            auth: self.clone(),
            meta,
            listener,
            authorization_url: authorization_url.to_string(),
            redirect_uri,
            verifier,
            state,
        })
    }

    /// Exchange the refresh token for a token bound to `resource`, rotating
    /// the refresh token. `invalid_grant` — an expired, revoked, or reused
    /// token — maps to a sign-in-required error.
    ///
    /// `attestation_context_id` is a client-minted UUID the gateway creates
    /// on first use and pins to the session: an inference call with a token
    /// minted inside a context records tool-call observations, and a
    /// gateway-attested MCP endpoint only accepts calls whose token carries
    /// the matching context. The gateway rejects a context pinned to another
    /// session as `invalid_target`.
    pub async fn refresh(
        &self,
        refresh_token: &str,
        resource: &str,
        expected_installation: &str,
        attestation_context_id: Option<&str>,
    ) -> Result<TokenSet> {
        let mut form: Vec<(&str, &str)> = vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("resource", resource),
        ];
        if let Some(client_name) = self.config.client_name.as_deref() {
            form.push(("client_name", client_name));
        }
        if let Some(context) = attestation_context_id {
            form.push(("attestation_context_id", context));
        }
        let token = self.request_token(&form).await?;
        if token.resource != resource || token.installation_id != expected_installation {
            return Err(gateway_error(
                "token request",
                "token was minted for an unexpected installation or resource",
            ));
        }
        Ok(token)
    }

    /// Revoke a refresh token and its session family.
    pub async fn revoke(&self, refresh_token: &str) -> Result<()> {
        let response = self
            .http
            .post(self.endpoint("/oauth/revoke")?)
            .form(&[("token", refresh_token)])
            .send()
            .await
            .map_err(|error| gateway_error("revocation request", error.without_url()))?;
        if !response.status().is_success() {
            return Err(gateway_error(
                "revocation request",
                format!("HTTP status {}", response.status().as_u16()),
            ));
        }
        Ok(())
    }

    /// The authenticated identity behind a `control` token.
    pub async fn whoami(&self, access_token: &str) -> Result<GatewayIdentity> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/cli/me")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("identity request", error.without_url()))?;
        decode_json(response, "identity request").await
    }

    /// The models the authenticated user may invoke, optionally filtered to
    /// one inference protocol (`anthropic_messages` /
    /// `openai_chat_completions`).
    pub async fn models(
        &self,
        access_token: &str,
        protocol: Option<&str>,
    ) -> Result<Vec<GatewayModel>> {
        let mut url = self.endpoint("/api/v1/cli/models")?;
        if let Some(protocol) = protocol {
            url.query_pairs_mut().append_pair("protocol", protocol);
        }
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("model request", error.without_url()))?;
        let list: ModelListResponse = decode_json(response, "model request").await?;
        Ok(list.models)
    }

    /// The connected apps the authenticated user is entitled to, or `None`
    /// against a gateway that predates the JSON apps surface.
    pub async fn apps(&self, access_token: &str) -> Result<Option<Vec<GatewayApp>>> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/cli/apps")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("app request", error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let list: AppListResponse = decode_json(response, "app request").await?;
        Ok(Some(list.apps))
    }

    /// The teams the authenticated user belongs to, or `None` against a
    /// gateway that predates the teams read.
    pub async fn teams(&self, access_token: &str) -> Result<Option<Vec<GatewayTeam>>> {
        let response = self
            .http
            .get(self.endpoint("/api/v1/cli/teams")?)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("team request", error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let list: TeamListResponse = decode_json(response, "team request").await?;
        Ok(Some(list.teams))
    }

    /// The operations one entitled connected app declares, or `None` against a
    /// gateway that predates the per-app catalog read.
    ///
    /// The id is the gateway's own and opaque here, so it is validated against
    /// the binding grammar and appended as one percent-encoded path segment: a
    /// value carrying `/` or `..` can never rewrite the request path.
    pub async fn app_operations(
        &self,
        access_token: &str,
        app_id: &str,
    ) -> Result<Option<Vec<GatewayOperationSummary>>> {
        validate_gateway_app_id(app_id)?;
        let mut url = self.endpoint("/api/v1/cli/apps")?;
        url.path_segments_mut()
            .map_err(|()| gateway_error("configuration", "could not build endpoint URL"))?
            .push(app_id)
            .push("operations");
        let response = self
            .http
            .get(url)
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|error| gateway_error("app catalog request", error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let list: OperationListResponse = decode_json(response, "app catalog request").await?;
        Ok(Some(list.into_operations()))
    }

    /// Relay one shared-app operation call to the gateway as the signed-in
    /// user — the data-plane half of a local app's gateway binding.
    ///
    /// `request` is the invoke body verbatim (`connected_app_id`,
    /// `operation_id`, `path_parameters`, `query`, `body`); the caller owns
    /// its assembly, and nothing here interprets the passthrough halves. The
    /// bearer is the ordinary `control`-audience session token — the same one
    /// `/api/v1/cli/*` reads carry — never an attested one: these are
    /// human-initiated frame calls with no model-emitted observation behind
    /// them.
    ///
    /// `Ok(None)` is the route answering `404`: a gateway that does not serve
    /// shared-app invokes yet, or an id it holds no app for. That reads as
    /// "nothing answers this pin", the same degradation
    /// [`apps`](Self::apps) and [`app_operations`](Self::app_operations) use,
    /// rather than a fault.
    pub async fn invoke_shared_app(
        &self,
        access_token: &str,
        shared_app_id: &str,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayInvokeOutcome>> {
        validate_gateway_app_id(shared_app_id)?;
        let mut url = self.endpoint("/api/apps/shared")?;
        url.path_segments_mut()
            .map_err(|()| gateway_error("configuration", "could not build endpoint URL"))?
            .push(shared_app_id)
            .push("invoke");
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(request)
            .send()
            .await
            .map_err(|error| gateway_error("shared app invoke", error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = read_bounded(response, "shared app invoke").await?;
        decode_shared_app_invoke(status, &body).map(Some)
    }

    /// Register one local app at the gateway as a shared app draft — the
    /// control-plane half of a local app's gateway bindings.
    ///
    /// `request` is the registration body verbatim (`name`, optional `slug`,
    /// `manifest`, optional `bundle_base64`, `client_name`); the caller owns
    /// its assembly, including the projection of the local manifest into the
    /// gateway's own manifest vocabulary.
    ///
    /// `Ok(None)` is the route answering `404`: a deployment that does not
    /// serve shared-app registration yet. That reads as "nothing there can
    /// hold this app", the same degradation [`apps`](Self::apps) and
    /// [`invoke_shared_app`](Self::invoke_shared_app) use, rather than a
    /// fault.
    pub async fn create_shared_app(
        &self,
        access_token: &str,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        const OPERATION: &str = "shared app registration";

        let response = self
            .http
            .post(self.endpoint("/api/v1/cli/shared-apps")?)
            .bearer_auth(access_token)
            .json(request)
            .send()
            .await
            .map_err(|error| gateway_error(OPERATION, error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = read_bounded(response, OPERATION).await?;
        decode_shared_app_registration(OPERATION, status, &body, None).map(Some)
    }

    /// Append a revision to an already-registered shared app.
    ///
    /// `Ok(None)` covers both `404` readings — a deployment without the route
    /// and an app this user does not own — because neither leaves anything
    /// here to append to.
    pub async fn create_shared_app_revision(
        &self,
        access_token: &str,
        shared_app_id: &str,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        const OPERATION: &str = "shared app revision";

        validate_gateway_app_id(shared_app_id)?;
        let mut url = self.endpoint("/api/v1/cli/shared-apps")?;
        url.path_segments_mut()
            .map_err(|()| gateway_error("configuration", "could not build endpoint URL"))?
            .push(shared_app_id)
            .push("revision");
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(request)
            .send()
            .await
            .map_err(|error| gateway_error(OPERATION, error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = read_bounded(response, OPERATION).await?;
        decode_shared_app_registration(OPERATION, status, &body, Some(shared_app_id)).map(Some)
    }

    /// Record the author's consent for one shared app, optionally pinned to
    /// the revision the caller believes is current.
    ///
    /// `Ok(None)` is the route answering `404`: nothing at this deployment
    /// holds the app.
    pub async fn consent_shared_app(
        &self,
        access_token: &str,
        shared_app_id: &str,
        revision_id: Option<&str>,
    ) -> Result<Option<GatewayConsentOutcome>> {
        const OPERATION: &str = "shared app consent";

        validate_gateway_app_id(shared_app_id)?;
        let mut url = self.endpoint("/api/apps/shared")?;
        url.path_segments_mut()
            .map_err(|()| gateway_error("configuration", "could not build endpoint URL"))?
            .push(shared_app_id)
            .push("consent");
        let mut request = serde_json::Map::new();
        if let Some(revision_id) = revision_id {
            request.insert(
                "revision_id".into(),
                serde_json::Value::String(revision_id.to_owned()),
            );
        }
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&serde_json::Value::Object(request))
            .send()
            .await
            .map_err(|error| gateway_error(OPERATION, error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = read_bounded(response, OPERATION).await?;
        decode_shared_app_consent(OPERATION, status, &body).map(Some)
    }

    /// Publish one registered shared app to a team the caller belongs to.
    ///
    /// The gateway decides everything that matters here — whether the caller
    /// may publish this app, whether the team is theirs, and whether the
    /// bundle is one it can serve at all (a bundle calling host-local bridge
    /// verbs is refused, because only the OpenWave host answers them). So
    /// nothing is pre-judged locally: the id and the team cross as given and
    /// the answer is read back typed.
    ///
    /// `Ok(None)` is the route answering `404`, which covers both of its
    /// readings — a deployment that does not serve publishing yet, and an app
    /// or team the caller has no authority over, which the gateway collapses
    /// to the same status on purpose. Neither leaves anything here to publish.
    pub async fn publish_shared_app(
        &self,
        access_token: &str,
        shared_app_id: &str,
        team_id: &str,
    ) -> Result<Option<GatewayPublishOutcome>> {
        const OPERATION: &str = "shared app publish";

        validate_gateway_app_id(shared_app_id)?;
        let mut url = self.endpoint("/api/v1/cli/shared-apps")?;
        url.path_segments_mut()
            .map_err(|()| gateway_error("configuration", "could not build endpoint URL"))?
            .push(shared_app_id)
            .push("publish");
        let response = self
            .http
            .post(url)
            .bearer_auth(access_token)
            .json(&serde_json::json!({ "team_id": team_id }))
            .send()
            .await
            .map_err(|error| gateway_error(OPERATION, error.without_url()))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status();
        let body = read_bounded(response, OPERATION).await?;
        decode_shared_app_publish(OPERATION, status, &body).map(Some)
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
    ) -> Result<TokenSet> {
        self.request_token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", &self.config.client_id),
            ("code_verifier", verifier),
        ])
        .await
    }

    async fn request_token(&self, form: &[(&str, &str)]) -> Result<TokenSet> {
        let response = self
            .http
            .post(self.endpoint("/oauth/token")?)
            .form(form)
            .send()
            .await
            .map_err(|error| gateway_error("token request", error.without_url()))?;
        if !response.status().is_success() {
            let status = response.status();
            let error: Option<OAuthErrorResponse> = response.json().await.ok();
            if error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_grant")
            {
                return Err(sign_in_required("the gateway session is no longer valid"));
            }
            // Kept distinguishable so an attested mint can tell a rejected
            // context (remintable under a fresh id) from other failures.
            if error
                .as_ref()
                .is_some_and(|error| error.error == "invalid_target")
            {
                let detail = error
                    .and_then(|error| error.error_description)
                    .unwrap_or_else(|| "the requested target is unavailable".to_string());
                return Err(gateway_error(
                    "token request",
                    format!("invalid_target: {detail}"),
                ));
            }
            let detail = error
                .and_then(|error| error.error_description.or(Some(error.error)))
                .unwrap_or_else(|| format!("HTTP status {}", status.as_u16()));
            return Err(gateway_error("token request", detail));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|_| gateway_error("token request", "invalid token response"))?;
        Ok(TokenSet {
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at_unix: unix_time()
                .saturating_add(u64::try_from(token.expires_in).unwrap_or_default()),
            scope: token.scope,
            resource: token.resource,
            installation_id: token.installation_id,
        })
    }

    fn endpoint(&self, path: &str) -> Result<reqwest::Url> {
        // Join below the configured base rather than at its origin, so a
        // gateway deployed under a subpath keeps its prefix.
        let mut base = self.config.base_url.to_string();
        if !base.ends_with('/') {
            base.push('/');
        }
        reqwest::Url::parse(&base)
            .and_then(|base| base.join(path.trim_start_matches('/')))
            .map_err(|_| gateway_error("configuration", "could not build endpoint URL"))
    }
}

/// A sign-in in flight: the loopback listener is bound and the browser URL
/// is ready. Dropping this cancels the sign-in.
pub struct PendingSignIn {
    auth: GatewayAuth,
    meta: GatewayMeta,
    listener: TcpListener,
    authorization_url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
}

/// A completed sign-in: identity plus the initial `control`-resource tokens.
#[derive(Debug, Clone)]
pub struct AuthorizedSession {
    pub meta: GatewayMeta,
    pub identity: GatewayIdentity,
    pub tokens: TokenSet,
}

impl PendingSignIn {
    /// The URL to open in the user's browser.
    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// The deployment identity pinned when the sign-in started.
    #[must_use]
    pub fn meta(&self) -> &GatewayMeta {
        &self.meta
    }

    /// Wait for the browser callback, then exchange and validate the code.
    pub async fn finish(self, timeout: Duration) -> Result<AuthorizedSession> {
        let Self {
            auth,
            meta,
            listener,
            redirect_uri,
            verifier,
            state,
            ..
        } = self;

        let (sender, mut receiver) = mpsc::channel::<CallbackResult>(1);
        let callback_app =
            Router::new()
                .route("/callback", get(callback))
                .with_state(CallbackState {
                    expected_state: state,
                    sender,
                });
        let server = tokio::spawn(async move { axum::serve(listener, callback_app).await });
        let outcome = tokio::time::timeout(timeout, receiver.recv()).await;
        server.abort();
        let _ = server.await;

        let callback = outcome
            .map_err(|_| gateway_error("sign-in", "browser authorization timed out"))?
            .ok_or_else(|| gateway_error("sign-in", "the authorization callback closed"))?;
        if let Some(error) = callback.error {
            return Err(gateway_error(
                "sign-in",
                format!("authorization failed: {error}"),
            ));
        }
        let code = callback
            .code
            .ok_or_else(|| gateway_error("sign-in", "no authorization code was returned"))?;

        let tokens = auth.exchange_code(&code, &redirect_uri, &verifier).await?;
        if tokens.resource != RESOURCE_CONTROL || tokens.installation_id != meta.installation_id {
            return Err(gateway_error(
                "sign-in",
                "token was minted for an unexpected installation or resource",
            ));
        }
        let identity = auth.whoami(&tokens.access_token).await?;
        if identity.installation_id != meta.installation_id {
            return Err(gateway_error(
                "sign-in",
                "gateway identity changed during authorization",
            ));
        }
        Ok(AuthorizedSession {
            meta,
            identity,
            tokens,
        })
    }
}

/// A signed-in gateway connection: vault-backed and refresh-rotating.
///
/// Share the *instance*: token motion is serialized per connection, so one
/// shared `GatewayConnection` can never race itself into the gateway's
/// reuse detection — but two instances (or two processes) over the same
/// keychain entry can. A crash between a successful refresh and the vault
/// write loses the rotated token, which reads as signed-out on the next
/// call; that window is inherent to rotation and recovers via reconnect.
pub struct GatewayConnection {
    auth: GatewayAuth,
    vault: CredentialVault,
    token_motion: Mutex<()>,
    /// Attestation contexts and the access tokens minted inside them.
    /// Memory-only by design: context ids are pinned server-side to the
    /// session they were first used with, so persisting them would carry
    /// stale ids across sign-ins, and the tokens expire within minutes
    /// anyway. Locked only while `token_motion` is held, so the two locks
    /// have a fixed order.
    attested: Mutex<AttestedTokens>,
}

/// The client-minted attestation context id for each caller key, plus the
/// fresh access tokens minted inside each context, keyed by
/// `(resource, context id)`.
#[derive(Default)]
struct AttestedTokens {
    contexts: HashMap<String, String>,
    tokens: HashMap<(String, String), CachedAccessToken>,
}

/// True when a token request failed because the gateway refused the
/// requested target — for an attested mint, a context id pinned to a
/// superseded session. Reminting under a fresh id recovers.
fn is_attestation_context_rejected(error: &AgentError) -> bool {
    matches!(error, AgentError::Config(message) if message.contains("invalid_target"))
}

impl GatewayConnection {
    #[must_use]
    pub fn new(auth: GatewayAuth, vault: CredentialVault) -> Self {
        Self {
            auth,
            vault,
            token_motion: Mutex::new(()),
            attested: Mutex::new(AttestedTokens::default()),
        }
    }

    /// The underlying auth client, for flows that operate before a session
    /// exists (starting a browser sign-in).
    #[must_use]
    pub fn auth(&self) -> &GatewayAuth {
        &self.auth
    }

    /// Persist a completed sign-in as the connection's stored credentials.
    ///
    /// The vault holds one session, so whatever was stored is superseded the
    /// moment this saves: its refresh token is revoked first (best-effort, at
    /// its own gateway), or it would stay live server-side with no local
    /// state left to ever revoke it.
    pub async fn store_session(&self, session: &AuthorizedSession) -> Result<()> {
        let _guard = self.token_motion.lock().await;
        // Attestation contexts are pinned to the session being replaced.
        *self.attested.lock().await = AttestedTokens::default();
        // An unreadable stored blob is skipped, as in sign-out: it carries no
        // usable refresh token to revoke.
        if let Ok(Some(superseded)) = self.vault.load().await {
            self.revoke_stored(&superseded).await;
        }
        let mut access_tokens = BTreeMap::new();
        access_tokens.insert(
            session.tokens.resource.clone(),
            CachedAccessToken {
                token: session.tokens.access_token.clone(),
                expires_at_unix: session.tokens.expires_at_unix,
                scope: session.tokens.scope.clone(),
            },
        );
        self.vault
            .save(&GatewayCredentials {
                base_url: self.auth.config.base_url.to_string(),
                installation_id: session.meta.installation_id.clone(),
                user_id: session.identity.user_id.clone(),
                account_hint: session
                    .identity
                    .email
                    .clone()
                    .or_else(|| session.identity.display_name.clone()),
                refresh_token: session.tokens.refresh_token.clone(),
                access_tokens,
            })
            .await
    }

    /// Whether stored credentials were minted against this connection's
    /// configured gateway. Trailing slashes are normalized the same way
    /// [`GatewayAuth::endpoint`] does, so a subpath deployment retyped with
    /// or without one never reads as a different gateway.
    fn matches_deployment(&self, credentials: &GatewayCredentials) -> bool {
        credentials.matches_base_url(self.auth.config.base_url.as_str())
    }

    /// Best-effort, bounded revocation of a stored session at its own
    /// gateway.
    ///
    /// The stored credentials name the deployment they were minted against,
    /// which need not be this connection's own (an MDM re-point, a re-pair):
    /// the revoke goes where the token is valid. A gateway that is
    /// unreachable, gone, or whose stored URL no longer parses cannot hold
    /// the caller hostage — the server-side session still dies at
    /// refresh-token expiry.
    async fn revoke_stored(&self, credentials: &GatewayCredentials) {
        const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);
        let auth = if self.matches_deployment(credentials) {
            Some(self.auth.clone())
        } else {
            GatewayAuthConfig::new(&credentials.base_url)
                .and_then(GatewayAuth::new)
                .ok()
        };
        let Some(auth) = auth else { return };
        let _ = tokio::time::timeout(REVOKE_TIMEOUT, auth.revoke(&credentials.refresh_token)).await;
    }

    /// The stored (offline) identity, if signed in to this connection's
    /// configured gateway. A session minted against a different deployment
    /// reads as absent: reporting it would assert an identity that is simply
    /// wrong for the configured base URL.
    pub async fn stored_credentials(&self) -> Result<Option<GatewayCredentials>> {
        Ok(self
            .vault
            .load()
            .await?
            .filter(|credentials| self.matches_deployment(credentials)))
    }

    /// A fresh access token for `resource`, from cache when possible and via
    /// rotating refresh otherwise.
    pub async fn access_token(&self, resource: &str) -> Result<String> {
        let _guard = self.token_motion.lock().await;
        let Some(mut credentials) = self.vault.load().await? else {
            return Err(sign_in_required("no gateway session is stored"));
        };
        // Refuse early rather than let installation pinning fail downstream:
        // "sign-in required" is actionable, a provider 500 is not.
        if !self.matches_deployment(&credentials) {
            return Err(sign_in_required(
                "the stored gateway session belongs to a different gateway deployment",
            ));
        }
        if let Some(cached) = credentials.access_tokens.get(resource) {
            if cached.is_fresh() {
                return Ok(cached.token.clone());
            }
        }
        let token = self
            .auth
            .refresh(
                &credentials.refresh_token,
                resource,
                &credentials.installation_id,
                None,
            )
            .await?;
        credentials.refresh_token = token.refresh_token.clone();
        credentials.access_tokens.insert(
            resource.to_string(),
            CachedAccessToken {
                token: token.access_token.clone(),
                expires_at_unix: token.expires_at_unix,
                scope: token.scope.clone(),
            },
        );
        self.vault.save(&credentials).await?;
        Ok(token.access_token)
    }

    /// A fresh access token for `resource`, minted inside the attestation
    /// context named by `context_key` — a caller-chosen stable key,
    /// typically a chat id. Tokens for the same key share one gateway
    /// context across resources; that shared context is what lets a
    /// gateway-attested MCP endpoint match a tool call against the
    /// observation recorded by the same chat's inference.
    ///
    /// The context id is a client-minted UUID the gateway creates on first
    /// use and pins to the current session. The registry and the tokens
    /// minted inside it live in memory only and reset when the stored
    /// session changes; a context id somehow left over from a superseded
    /// session is rejected by the gateway, and one remint under a fresh id
    /// self-heals that without a sign-out.
    pub async fn attested_access_token(&self, resource: &str, context_key: &str) -> Result<String> {
        let _guard = self.token_motion.lock().await;
        let Some(mut credentials) = self.vault.load().await? else {
            return Err(sign_in_required("no gateway session is stored"));
        };
        if !self.matches_deployment(&credentials) {
            return Err(sign_in_required(
                "the stored gateway session belongs to a different gateway deployment",
            ));
        }
        let mut attested = self.attested.lock().await;
        attested.tokens.retain(|_, cached| cached.is_fresh());
        let context = attested
            .contexts
            .entry(context_key.to_string())
            .or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        if let Some(cached) = attested
            .tokens
            .get(&(resource.to_string(), context.clone()))
        {
            return Ok(cached.token.clone());
        }
        let (context, token) = match self
            .mint_attested(&mut credentials, resource, &context)
            .await
        {
            Ok(token) => (context, token),
            Err(error) if is_attestation_context_rejected(&error) => {
                let fresh = uuid::Uuid::new_v4().to_string();
                attested.tokens.retain(|(_, held), _| held != &context);
                attested
                    .contexts
                    .insert(context_key.to_string(), fresh.clone());
                let token = self
                    .mint_attested(&mut credentials, resource, &fresh)
                    .await?;
                (fresh, token)
            }
            Err(error) => return Err(error),
        };
        attested.tokens.insert(
            (resource.to_string(), context),
            CachedAccessToken {
                token: token.access_token.clone(),
                expires_at_unix: token.expires_at_unix,
                scope: token.scope.clone(),
            },
        );
        Ok(token.access_token)
    }

    /// A fresh access token bound to `mcp:<slug>` inside the attestation
    /// context named by `context_key`; see [`Self::attested_access_token`].
    pub async fn attested_mcp_access_token(&self, slug: &str, context_key: &str) -> Result<String> {
        validate_mcp_endpoint_slug(slug)?;
        self.attested_access_token(&format!("mcp:{slug}"), context_key)
            .await
    }

    /// One rotating refresh bound to `resource` inside `context`. The
    /// rotated refresh token is persisted; the minted access token is not —
    /// attested tokens are per-context and expire within minutes, so
    /// storing them would grow the keychain blob without ever serving a
    /// future process.
    async fn mint_attested(
        &self,
        credentials: &mut GatewayCredentials,
        resource: &str,
        context: &str,
    ) -> Result<TokenSet> {
        let token = self
            .auth
            .refresh(
                &credentials.refresh_token,
                resource,
                &credentials.installation_id,
                Some(context),
            )
            .await?;
        credentials.refresh_token = token.refresh_token.clone();
        self.vault.save(credentials).await?;
        Ok(token)
    }

    /// Live identity check with a `control` token.
    pub async fn identity(&self) -> Result<GatewayIdentity> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.whoami(&token).await
    }

    /// Entitled models with a `control` token.
    pub async fn models(&self, protocol: Option<&str>) -> Result<Vec<GatewayModel>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.models(&token, protocol).await
    }

    /// Entitled connected apps with a `control` token; `None` when the
    /// gateway predates the apps surface.
    pub async fn apps(&self) -> Result<Option<Vec<GatewayApp>>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.apps(&token).await
    }

    /// The teams the signed-in user belongs to, with a `control` token;
    /// `None` when the gateway predates the teams read.
    pub async fn teams(&self) -> Result<Option<Vec<GatewayTeam>>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.teams(&token).await
    }

    /// Publish a registered shared app to one of the signed-in user's teams
    /// with a `control` token; `None` when nothing at the gateway answers for
    /// the app or the route.
    pub async fn publish_shared_app(
        &self,
        shared_app_id: &str,
        team_id: &str,
    ) -> Result<Option<GatewayPublishOutcome>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth
            .publish_shared_app(&token, shared_app_id, team_id)
            .await
    }

    /// One entitled app's declared operations with a `control` token; `None`
    /// when the gateway predates the per-app catalog read.
    pub async fn app_operations(
        &self,
        app_id: &str,
    ) -> Result<Option<Vec<GatewayOperationSummary>>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.app_operations(&token, app_id).await
    }

    /// Relay one shared-app operation call with a `control` token; `None`
    /// when the gateway does not serve the invoke route or holds no such app.
    pub async fn invoke_shared_app(
        &self,
        shared_app_id: &str,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayInvokeOutcome>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth
            .invoke_shared_app(&token, shared_app_id, request)
            .await
    }

    /// Register one local app as a shared app with a `control` token; `None`
    /// when the gateway does not serve shared-app registration.
    pub async fn create_shared_app(
        &self,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth.create_shared_app(&token, request).await
    }

    /// Append a revision to a registered shared app with a `control` token;
    /// `None` when nothing at the gateway answers for the app.
    pub async fn create_shared_app_revision(
        &self,
        shared_app_id: &str,
        request: &serde_json::Value,
    ) -> Result<Option<GatewayRegistrationOutcome>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth
            .create_shared_app_revision(&token, shared_app_id, request)
            .await
    }

    /// Relay the author's consent for a registered shared app with a
    /// `control` token; `None` when nothing at the gateway answers for it.
    pub async fn consent_shared_app(
        &self,
        shared_app_id: &str,
        revision_id: Option<&str>,
    ) -> Result<Option<GatewayConsentOutcome>> {
        let token = self.access_token(RESOURCE_CONTROL).await?;
        self.auth
            .consent_shared_app(&token, shared_app_id, revision_id)
            .await
    }

    /// The gateway's MCP endpoint URL for `slug`, under the configured base.
    pub fn mcp_endpoint_url(&self, slug: &str) -> Result<String> {
        validate_mcp_endpoint_slug(slug)?;
        self.auth
            .endpoint(&format!("mcp/{slug}"))
            .map(|url| url.to_string())
    }

    /// A fresh access token bound to the `mcp:<slug>` resource, from cache
    /// when possible and via rotating refresh otherwise.
    pub async fn mcp_access_token(&self, slug: &str) -> Result<String> {
        validate_mcp_endpoint_slug(slug)?;
        self.access_token(&format!("mcp:{slug}")).await
    }

    /// Revoke the session at its own gateway (best-effort, bounded) and
    /// always clear the local vault. A gateway that is unreachable cannot
    /// hold local sign-out hostage; its server-side session still dies at
    /// refresh-token expiry.
    pub async fn sign_out(&self) -> Result<()> {
        let _guard = self.token_motion.lock().await;
        *self.attested.lock().await = AttestedTokens::default();
        if let Some(credentials) = self.vault.load().await? {
            self.revoke_stored(&credentials).await;
        }
        self.vault.clear().await
    }
}

async fn callback(
    State(state): State<CallbackState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if query.state.as_deref() != Some(state.expected_state.as_str()) {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Html(super::callback_page::callback_page(
                super::callback_page::CallbackOutcome::Failed,
                "Sign-in failed",
                "The authorization state did not match. Return to OpenWave and try again.",
            )),
        )
            .into_response();
    }
    let success = query.code.is_some() && query.error.is_none();
    let _ = state.sender.try_send(CallbackResult {
        code: query.code,
        error: query.error,
    });
    let (outcome, heading, message) = if success {
        (
            super::callback_page::CallbackOutcome::Success,
            "You're signed in",
            "",
        )
    } else {
        (
            super::callback_page::CallbackOutcome::Denied,
            "Sign-in was denied",
            "Nothing was connected. Return to OpenWave for details, or try again.",
        )
    };
    (
        axum::http::StatusCode::OK,
        Html(super::callback_page::callback_page(
            outcome, heading, message,
        )),
    )
        .into_response()
}

#[derive(Clone)]
struct CallbackState {
    expected_state: String,
    sender: mpsc::Sender<CallbackResult>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

struct CallbackResult {
    code: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: String,
    scope: String,
    resource: String,
    installation_id: String,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct ModelListResponse {
    models: Vec<GatewayModel>,
}

#[derive(Deserialize)]
struct AppListResponse {
    apps: Vec<GatewayApp>,
}

#[derive(Deserialize)]
struct TeamListResponse {
    teams: Vec<GatewayTeam>,
}

/// The per-app catalog read's body, in either shape the gateway may serve:
/// an object keyed by `operations`, or the bare array. Both are accepted
/// because the surface is being built against this client — pinning one shape
/// here would make the two repositories' merge order load-bearing — and the
/// object form leaves room for the catalog-document hash the record expects
/// to arrive later.
#[derive(Deserialize)]
#[serde(untagged)]
enum OperationListResponse {
    Wrapped {
        #[serde(default)]
        operations: Vec<GatewayOperationSummary>,
    },
    Bare(Vec<GatewayOperationSummary>),
}

impl OperationListResponse {
    fn into_operations(self) -> Vec<GatewayOperationSummary> {
        match self {
            Self::Wrapped { operations } | Self::Bare(operations) => operations,
        }
    }
}

/// The gateway app-id contract a request path may carry, mirroring the
/// manifest binding grammar
/// ([`openwave_core::local_app::MAX_GATEWAY_APP_ID_BYTES`]): 1–128 bytes of
/// printable, non-whitespace ASCII. The id is the gateway's and OpenWave never
/// interprets it; this only bounds what may be appended to a URL.
fn validate_gateway_app_id(app_id: &str) -> Result<()> {
    // Pure-dot segments pass the printable-ASCII check but would read as
    // path navigation to any origin that normalizes dot-segments, routing a
    // control bearer at a different route.
    if app_id.is_empty()
        || app_id.len() > openwave_core::local_app::MAX_GATEWAY_APP_ID_BYTES
        || !app_id.bytes().all(|byte| byte.is_ascii_graphic())
        || app_id.bytes().all(|byte| byte == b'.')
    {
        return Err(gateway_error("configuration", "invalid gateway app id"));
    }
    Ok(())
}

/// The gateway's endpoint-slug contract: 1–127 bytes of ASCII alphanumerics,
/// `-`, or `_`. Enforced here so a slug can never rewrite the request path
/// (`/`, `..`) or the token resource string it is embedded into. Public so
/// configuration layers validate against the same contract instead of a
/// second copy of these literals.
pub fn validate_mcp_endpoint_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 127
        || !slug
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(gateway_error("configuration", "invalid MCP endpoint slug"));
    }
    Ok(())
}

/// The most a single gateway response body may occupy in memory. Matches the
/// governed REST executor's response cap, so a relayed call is bounded exactly
/// as a locally executed one is.
const MAX_GATEWAY_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Read a response body with a hard byte bound.
///
/// An honest oversized `Content-Length` is refused before a byte is read; a
/// lying one is caught by the check-before-extend, so neither turns into an
/// unbounded allocation. The same shape [`crate::rest_executor`] uses for the
/// bodies it brings back from third-party APIs.
async fn read_bounded(response: reqwest::Response, operation: &str) -> Result<Vec<u8>> {
    use futures::StreamExt as _;

    let too_large = || gateway_error(operation, "response exceeded its byte budget");
    if response
        .content_length()
        .is_some_and(|length| length > MAX_GATEWAY_RESPONSE_BYTES as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| gateway_error(operation, error.without_url()))?;
        if body.len().saturating_add(chunk.len()) > MAX_GATEWAY_RESPONSE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Read the gateway's shared-app invoke answer, tolerantly — the one place
/// that knows the wire shape.
///
/// **The gateway contract is still settling.** The shared-app invoke route is
/// being built against this client in a separate repository, so pinning one
/// exact envelope here would make the two repositories' merge order
/// load-bearing. Every shape below is accepted instead, and this function is
/// the only thing that has to change when the contract lands:
///
/// - **Typed failures** are read from `{"error": {"code", "message"}}`,
///   `{"error": "<code>"}` with a sibling `message` or `error_description`,
///   or a bare top-level `{"code", "message"}`. The code
///   `authorization_required` becomes [`GatewayInvokeOutcome::AuthorizationRequired`]
///   and `consent_required` becomes [`GatewayInvokeOutcome::ConsentRequired`]
///   — the two outcomes a caller can act on, the first by sending the viewer
///   to the gateway and the second by re-stating the author's consent — and
///   every other code becomes [`GatewayInvokeOutcome::Refused`] carrying the
///   gateway's own message. A
///   failure envelope is looked for on any non-2xx answer, and on a 2xx answer
///   only when the body has an `error` member and no `status` member, so a
///   passthrough response body can never be misread as one.
/// - **Executed responses** come from `{operation_id, status, content_type,
///   headers?, body}` or `{status, content_type, body_base64}`. `status`
///   falls back to the HTTP status, and `content_type` to a `content-type`
///   entry in `headers`. `body_base64` is taken verbatim (it must decode);
///   a `body` string is the response text unless `body_encoding` says
///   `base64`, and a `body` of any other JSON type is re-serialized compact.
///   Guessing whether an arbitrary string "looks like" base64 is deliberately
///   not attempted: it silently corrupts short text bodies.
fn decode_shared_app_invoke(
    status: reqwest::StatusCode,
    body: &[u8],
) -> Result<GatewayInvokeOutcome> {
    const OPERATION: &str = "shared app invoke";

    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
        if status.is_success() {
            return Err(gateway_error(OPERATION, "invalid response body"));
        }
        return Err(gateway_error(
            OPERATION,
            format!("HTTP status {}", status.as_u16()),
        ));
    };
    let object = payload.as_object();
    let looks_like_failure = !status.is_success()
        || object.is_some_and(|payload| {
            payload.contains_key("error") && !payload.contains_key("status")
        });
    if looks_like_failure {
        if let Some((code, message)) = failure_envelope(&payload) {
            let message = message.unwrap_or_else(|| code.clone());
            return Ok(match code.as_str() {
                "authorization_required" => GatewayInvokeOutcome::AuthorizationRequired { message },
                "consent_required" => GatewayInvokeOutcome::ConsentRequired { message },
                _ => GatewayInvokeOutcome::Refused { message },
            });
        }
        if !status.is_success() {
            return Err(gateway_error(
                OPERATION,
                format!("HTTP status {}", status.as_u16()),
            ));
        }
    }
    let Some(payload) = object else {
        return Err(gateway_error(OPERATION, "invalid response body"));
    };
    let executed_status = payload
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or_else(|| status.as_u16());
    let content_type = payload
        .get("content_type")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .get("headers")?
                .as_object()?
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))?
                .1
                .as_str()
                .map(ToOwned::to_owned)
        });
    let body_base64 = if let Some(encoded) = payload.get("body_base64") {
        let encoded = encoded
            .as_str()
            .ok_or_else(|| gateway_error(OPERATION, "invalid response body"))?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| gateway_error(OPERATION, "invalid response body"))?;
        encoded.to_owned()
    } else {
        let base64_encoded = payload
            .get("body_encoding")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("base64"));
        let bytes = match payload.get("body") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::String(text)) if base64_encoded => {
                base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|_| gateway_error(OPERATION, "invalid response body"))?
            }
            Some(serde_json::Value::String(text)) => text.clone().into_bytes(),
            Some(value) => serde_json::to_vec(value)
                .map_err(|_| gateway_error(OPERATION, "invalid response body"))?,
        };
        base64::engine::general_purpose::STANDARD.encode(bytes)
    };
    Ok(GatewayInvokeOutcome::Executed {
        status: executed_status,
        content_type,
        body_base64,
    })
}

/// Read the gateway's answer to a shared-app create or revision append.
///
/// Success must name the revision the gateway now serves; a create must also
/// name the app it minted, while an append answers about an app the caller
/// already knows the id of (`known_id`). Failures are read with the same
/// tolerant envelope reader the invoke route uses, so the one code the caller
/// branches on — `slug_taken` — is recognized in every shape the gateway may
/// carry it.
fn decode_shared_app_registration(
    operation: &'static str,
    status: reqwest::StatusCode,
    body: &[u8],
    known_id: Option<&str>,
) -> Result<GatewayRegistrationOutcome> {
    let payload = serde_json::from_slice::<serde_json::Value>(body).ok();
    if !status.is_success() {
        let Some((code, message)) = payload.as_ref().and_then(failure_envelope) else {
            return Err(gateway_error(
                operation,
                format!("HTTP status {}", status.as_u16()),
            ));
        };
        let message = message.unwrap_or_else(|| code.clone());
        return Ok(if code == "slug_taken" {
            GatewayRegistrationOutcome::SlugTaken { message }
        } else {
            GatewayRegistrationOutcome::Refused { message }
        });
    }
    let invalid = || gateway_error(operation, "invalid response body");
    let payload = payload.ok_or_else(invalid)?;
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let shared_app_id = text("id")
        .or_else(|| known_id.map(ToOwned::to_owned))
        .ok_or_else(invalid)?;
    let revision_id = text("revision_id").ok_or_else(invalid)?;
    Ok(GatewayRegistrationOutcome::Registered {
        shared_app_id,
        revision_id,
    })
}

/// Read the gateway's answer to a shared-app consent relay. The response body
/// echoes the bindings consent covers, which nothing here needs: the gateway
/// computes them server-side from the live revision, and the caller only ever
/// pins which revision it meant.
fn decode_shared_app_consent(
    operation: &'static str,
    status: reqwest::StatusCode,
    body: &[u8],
) -> Result<GatewayConsentOutcome> {
    if status.is_success() {
        return Ok(GatewayConsentOutcome::Consented);
    }
    let Some((code, message)) = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(failure_envelope)
    else {
        return Err(gateway_error(
            operation,
            format!("HTTP status {}", status.as_u16()),
        ));
    };
    Ok(if code == "revision_moved" {
        GatewayConsentOutcome::RevisionMoved
    } else {
        GatewayConsentOutcome::Refused {
            message: message.unwrap_or(code),
        }
    })
}

/// Read the gateway's answer to a shared-app publish.
///
/// A success carries no body worth reading — the gateway answers `204` — so
/// everything here is about the refusals, read with the same tolerant
/// envelope reader the rest of this client uses. The gateway's own code
/// survives rather than being folded into prose: the host branches on
/// `app_disabled`, and a caller that renders the refusal needs the code to
/// tell a bundle it must change (`host_local_bridge_verbs`) from a team it
/// may not publish to (`not_a_team_member`) without matching on wording.
fn decode_shared_app_publish(
    operation: &'static str,
    status: reqwest::StatusCode,
    body: &[u8],
) -> Result<GatewayPublishOutcome> {
    if status.is_success() {
        return Ok(GatewayPublishOutcome::Published);
    }
    let Some((code, message)) = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .as_ref()
        .and_then(failure_envelope)
    else {
        return Err(gateway_error(
            operation,
            format!("HTTP status {}", status.as_u16()),
        ));
    };
    let message = message.unwrap_or_else(|| code.clone());
    Ok(if code == "app_disabled" {
        GatewayPublishOutcome::AppDisabled { message }
    } else {
        GatewayPublishOutcome::Refused { code, message }
    })
}

/// The `(code, message)` of a typed gateway failure, in any of the shapes the
/// route may carry it in — see [`decode_shared_app_invoke`].
fn failure_envelope(payload: &serde_json::Value) -> Option<(String, Option<String>)> {
    let text = |value: Option<&serde_json::Value>| {
        value
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let (holder, code) = match payload.get("error") {
        Some(serde_json::Value::Object(error)) => (
            Some(error),
            text(error.get("code")).or_else(|| text(error.get("error")))?,
        ),
        Some(serde_json::Value::String(code)) => (None, code.clone()),
        _ => (None, text(payload.get("code"))?),
    };
    let message = holder
        .and_then(|error| {
            text(error.get("message"))
                .or_else(|| text(error.get("detail")))
                .or_else(|| text(error.get("description")))
        })
        .or_else(|| text(payload.get("message")))
        .or_else(|| text(payload.get("error_description")));
    Some((code, message))
}

async fn decode_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    operation: &str,
) -> Result<T> {
    if !response.status().is_success() {
        return Err(gateway_error(
            operation,
            format!("HTTP status {}", response.status().as_u16()),
        ));
    }
    response
        .json()
        .await
        .map_err(|_| gateway_error(operation, "invalid response body"))
}

/// URL-safe random material from the OS CSPRNG. Two UUIDv4 values give 244
/// bits of entropy and a 73-byte token, inside PKCE's 43–128 char contract.
fn random_token() -> String {
    format!("{}-{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_rejects_invalid_base_urls() {
        assert!(GatewayAuthConfig::new("http://127.0.0.1:28081").is_ok());
        assert!(GatewayAuthConfig::new("http://localhost:28081").is_ok());
        assert!(GatewayAuthConfig::new("http://[::1]:28081").is_ok());
        assert!(GatewayAuthConfig::new("https://gateway.example").is_ok());
        for url in ["ftp://x", "not a url", "http://user:pw@host"] {
            assert!(GatewayAuthConfig::new(url).is_err(), "{url}");
        }
    }

    /// Cleartext to anything but this machine would put the OAuth code and the
    /// tokens it buys on the wire in the clear, and a provision link naming
    /// the origin is reachable by any webpage.
    #[test]
    fn config_requires_https_off_loopback() {
        for url in [
            "http://gateway.example",
            "http://gateway.example:8080/base",
            // Not loopback: a public address, and a name that merely looks local.
            "http://10.0.0.5:28081",
            "http://localhost.attacker.example",
        ] {
            assert!(GatewayAuthConfig::new(url).is_err(), "{url}");
        }
    }

    #[test]
    fn token_types_redact_secret_material() {
        let tokens = TokenSet {
            access_token: "mg_at_secret".into(),
            refresh_token: "mg_rt_secret".into(),
            expires_at_unix: 0,
            scope: "profile".into(),
            resource: RESOURCE_CONTROL.into(),
            installation_id: "install".into(),
        };
        let debug = format!("{tokens:?}");
        assert!(!debug.contains("mg_at_secret"));
        assert!(!debug.contains("mg_rt_secret"));

        let credentials = GatewayCredentials {
            base_url: "http://127.0.0.1/".into(),
            installation_id: "install".into(),
            user_id: "user".into(),
            account_hint: Some("a@example.com".into()),
            refresh_token: "mg_rt_secret".into(),
            access_tokens: BTreeMap::from([(
                RESOURCE_CONTROL.to_string(),
                CachedAccessToken {
                    token: "mg_at_secret".into(),
                    expires_at_unix: 0,
                    scope: "profile".into(),
                },
            )]),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("mg_at_secret"));
        assert!(!debug.contains("mg_rt_secret"));
        assert!(debug.contains("control"));
    }

    #[test]
    fn random_tokens_fit_the_pkce_contract() {
        let token = random_token();
        assert!((43..=128).contains(&token.len()), "{}", token.len());
        assert!(token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
        assert_ne!(random_token(), random_token());
    }

    /// The two invoke refusals a caller acts on have to survive the wire as
    /// themselves: `authorization_required` is the viewer's to resolve at the
    /// gateway, and `consent_required` is the host's to heal by re-stating
    /// the author's consent. Anything else is the app's to present.
    #[test]
    fn actionable_invoke_refusals_decode_to_their_own_arms() {
        let decode = |code: &str| {
            decode_shared_app_invoke(
                reqwest::StatusCode::FORBIDDEN,
                format!(r#"{{"error":{{"code":"{code}","message":"nope"}}}}"#).as_bytes(),
            )
            .unwrap()
        };
        assert_eq!(
            decode("consent_required"),
            GatewayInvokeOutcome::ConsentRequired {
                message: "nope".into()
            }
        );
        assert_eq!(
            decode("authorization_required"),
            GatewayInvokeOutcome::AuthorizationRequired {
                message: "nope".into()
            }
        );
        assert_eq!(
            decode("app_disabled"),
            GatewayInvokeOutcome::Refused {
                message: "nope".into()
            }
        );
    }

    /// A publish refusal is the only thing an author sees when publishing
    /// fails, so the gateway's own words have to survive intact — the
    /// `host_local_bridge_verbs` message names the verbs the bundle uses, and
    /// nothing on this side can reconstruct that list. The code survives too,
    /// because `app_disabled` is the host's to branch on.
    #[test]
    fn a_publish_refusal_keeps_the_gateway_code_and_its_message() {
        let decode = |status: u16, body: &str| {
            decode_shared_app_publish(
                "shared app publish",
                reqwest::StatusCode::from_u16(status).unwrap(),
                body.as_bytes(),
            )
            .unwrap()
        };
        assert_eq!(
            decode(204, ""),
            GatewayPublishOutcome::Published,
            "the gateway answers a publish with no content"
        );
        let verbs = "this bundle calls fs/read and fs/write, which only the OpenWave \
                     host serves";
        assert_eq!(
            decode(
                422,
                &format!(
                    "{{\"error\":{{\"code\":\"host_local_bridge_verbs\",\
                     \"message\":\"{verbs}\"}}}}"
                ),
            ),
            GatewayPublishOutcome::Refused {
                code: "host_local_bridge_verbs".into(),
                message: verbs.into(),
            }
        );
        assert_eq!(
            decode(
                409,
                r#"{"error":{"code":"app_disabled","message":"this app is off"}}"#
            ),
            GatewayPublishOutcome::AppDisabled {
                message: "this app is off".into()
            }
        );
        assert!(
            decode_shared_app_publish(
                "shared app publish",
                reqwest::StatusCode::BAD_GATEWAY,
                b"<html>nope</html>",
            )
            .is_err(),
            "an answer carrying no typed refusal is a fault, not a decision"
        );
    }

    #[test]
    fn sign_in_required_errors_are_recognizable() {
        assert!(is_sign_in_required(&sign_in_required("test")));
        assert!(!is_sign_in_required(&gateway_error("x", "y")));
    }
}
