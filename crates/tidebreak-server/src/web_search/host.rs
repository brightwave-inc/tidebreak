//! Host-owned configuration and provider selection for web search.
//!
//! A selected provider becomes usable only when the sandbox worker or approved
//! foreground tool explicitly asks [`resolve_provider`] for it. Provider
//! endpoints are fixed in `this module`; this config never accepts an
//! endpoint, a secret reference, or a model-controlled network target.

use std::sync::Arc;
use std::time::Duration;

use super::{
    BraveProvider, ExaProvider, ExtractedPageSink, ExtractedPageSinkError, FirecrawlProvider,
    ModelProviderSearch, NativeExtractor, OutboundOrigin, PageExtractor, ReqwestHttpClient,
    ReqwestPageFetcher, SearchModel, SearxngBaseUrl, SearxngProvider, StoredExtractedPage,
    TavilyProvider, TokioHostResolver, WebExtractFailure, WebExtractRequest, WebExtractResponse,
    WebExtractTool, WebSearchCredential, WebSearchCredentialState, WebSearchCredentials,
    WebSearchProvider, WebSearchProviderKind, WebSearchResolver, WebSearchResolverError,
    WebSearchTool,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tidebreak_core::{
    ApprovalClass, DocumentId, DocumentUpsert, NetworkPolicy, Result, SecretProvider, SessionId,
    Store, Tool, ToolCtx, ToolOutput, ToolSpec, TurnWebSearch, VendorWebSearch,
};

use crate::error::ServerError;
use crate::model_roles::{self, ModelRole};
use crate::resolver::ProviderResolver;

const NETWORK_OFF_ERROR_CODE: &str = "chat_network_off";
const NETWORK_OFF_MESSAGE: &str =
    "Network is off for this chat. Turn it on from the composer's + menu under Network.";

/// Enforce the per-chat network choice before a network-capable tool runs.
///
/// Destination admission remains inside each transport; this gate exists so
/// an intentional offline choice is reported clearly instead of collapsing
/// into an empty search or an opaque fetch failure.
struct ChatNetworkPolicyTool {
    store: Arc<dyn Store>,
    inner: Box<dyn Tool>,
}

impl ChatNetworkPolicyTool {
    fn new(store: Arc<dyn Store>, inner: Box<dyn Tool>) -> Self {
        Self { store, inner }
    }

    fn denied_output() -> ToolOutput {
        ToolOutput::error(NETWORK_OFF_MESSAGE)
            .with_data(serde_json::json!({ "error_code": NETWORK_OFF_ERROR_CODE }))
    }
}

#[async_trait]
impl Tool for ChatNetworkPolicyTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn approval_class(&self) -> ApprovalClass {
        self.inner.approval_class()
    }

    async fn execute(&self, ctx: &ToolCtx, args: serde_json::Value) -> Result<ToolOutput> {
        let chat = self.store.get_chat(ctx.chat_id).await?;
        if chat.is_some_and(|chat| chat.network_policy == NetworkPolicy::Off) {
            return Ok(Self::denied_output());
        }
        self.inner.execute(ctx, args).await
    }
}

/// Store key for the non-secret web-search configuration.
const WEB_SEARCH_SETTING: &str = "web_search";
/// Default end-to-end request timeout for a configured provider.
pub const DEFAULT_TIMEOUT_MS: u64 = 20_000;
/// Lower bound to avoid a configuration that cannot complete a normal TLS
/// request, while still keeping retries and recovery responsive.
pub const MIN_TIMEOUT_MS: u64 = 1_000;
/// Upper bound on one provider request. Long-running work must be expressed as
/// durable worker state rather than an unbounded HTTP call.
pub const MAX_TIMEOUT_MS: u64 = 60_000;

/// The fixed providers this host can hold a credential for. SearXNG is
/// self-hosted and holds none, so it is absent here. Keeping this allow-list
/// here means a local API route can never turn an arbitrary path segment into
/// a keychain key.
const CREDENTIAL_PROVIDERS: [WebSearchProviderKind; 4] = [
    WebSearchProviderKind::Exa,
    WebSearchProviderKind::Tavily,
    WebSearchProviderKind::Brave,
    WebSearchProviderKind::Firecrawl,
];

/// Which search a turn should use, as the operator chose it.
///
/// The choice is about *who runs the search*, not about which engine: a vendor
/// search runs inside the model provider's own infrastructure and never touches
/// this host's providers, credentials, or egress policy. `Automatic` is the
/// default and the value every configuration written before this existed reads
/// back as, so an installation that had web search working keeps exactly the
/// search it had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchMode {
    /// Prefer a configured, usable host provider; fall back to the model's own
    /// search when the model has one and the host has nothing to run.
    #[default]
    Automatic,
    /// The model's own search, or none. Deliberately no host fallback: an
    /// operator who chose the vendor route did so to keep queries off this
    /// host's providers, and quietly reinstating one would defeat the choice.
    Vendor,
    /// The host's configured provider only, exactly as before this setting
    /// existed.
    Host,
    /// No web search at all. The tool is not advertised, so the model is not
    /// offered a capability the operator turned off.
    Off,
}

/// Non-secret host configuration. `provider: None` is the safe default: no
/// credential lookup and no possible outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<WebSearchProviderKind>,
    /// Which search a turn gets. Independent of `provider`: the host provider
    /// stays configured while the vendor route is selected, so switching back
    /// does not ask for the key again.
    #[serde(default)]
    pub mode: WebSearchMode,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Base URL of the operator's self-hosted SearXNG instance.
    ///
    /// This is the one address in the whole surface that is configuration
    /// rather than a constant, because a self-hosted instance has none to pin.
    /// It is host configuration only: it is never a model argument and nothing
    /// in a tool call can reach it. It is validated here at `PUT` time, exactly
    /// as the egress allowlist is, so a malformed value is rejected rather than
    /// silently widening where the transport may dial.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub searxng_base_url: Option<String>,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: None,
            mode: WebSearchMode::Automatic,
            timeout_ms: DEFAULT_TIMEOUT_MS,
            searxng_base_url: None,
        }
    }
}

impl WebSearchConfig {
    fn validate(&self) -> std::result::Result<(), ServerError> {
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(ServerError::bad_request(format!(
                "web search timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
            )));
        }
        if self
            .searxng_base_url
            .as_deref()
            .map(SearxngBaseUrl::parse)
            .is_some_and(|parsed| parsed.is_err())
        {
            return Err(ServerError::bad_request(
                "web search searxng_base_url must be an http or https instance URL with no credentials, query, or fragment",
            ));
        }
        Ok(())
    }

    /// The configured instance URL in canonical form, if it is usable.
    ///
    /// `validate` has already rejected a malformed value, so this reads as a
    /// straightforward "is one configured" without a second error path.
    fn searxng_base_url(&self) -> Option<SearxngBaseUrl> {
        self.searxng_base_url
            .as_deref()
            .and_then(|value| SearxngBaseUrl::parse(value).ok())
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Public state returned by the local API. It intentionally reports only
/// selection, credential presence, and the configured instance URL — key
/// material never crosses the secret boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct WebSearchConfigInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<WebSearchProviderKind>,
    /// Which search a turn gets. Orthogonal to the fields below, which report
    /// only the host provider's readiness: a vendor turn is unaffected by all
    /// of them.
    pub mode: WebSearchMode,
    pub timeout_ms: u64,
    /// Whether a key is stored for the selected provider. Always false for a
    /// credential-free provider, which has no key slot at all — read
    /// [`Self::available`] to know whether search will actually run.
    pub has_credential: bool,
    /// Whether the selected provider has everything it needs to be invoked.
    ///
    /// A key for the credentialed providers, an instance URL for SearXNG.
    pub available: bool,
    /// The configured SearXNG instance URL, in the canonical form the host
    /// stored. It is safe to return: validation forbids embedded credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub searxng_base_url: Option<String>,
}

/// Credential readiness for one fixed web-search provider. This public shape
/// deliberately carries no secret material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct WebSearchCredentialReadiness {
    pub provider: WebSearchProviderKind,
    pub has_credential: bool,
}

/// Credential readiness for every provider Tidebreak supports locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSearchCredentialsInfo {
    pub credentials: Vec<WebSearchCredentialReadiness>,
}

/// Partial update accepted by `PUT /web-search`. An omitted `provider` leaves
/// selection unchanged; an explicit `null` disables web search.
#[derive(Debug, Deserialize)]
pub struct WebSearchConfigUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub provider: Option<Option<WebSearchProviderKind>>,
    /// An omitted mode leaves the current choice unchanged.
    #[serde(default)]
    pub mode: Option<WebSearchMode>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// An omitted value leaves the instance URL unchanged; an explicit `null`
    /// clears it, which takes SearXNG out of service without discarding the
    /// other providers' keys.
    #[serde(default, deserialize_with = "double_option")]
    pub searxng_base_url: Option<Option<String>>,
}

fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Read configured host policy. Malformed legacy/manual data fails closed.
pub async fn read_config(store: &dyn Store) -> Result<WebSearchConfig> {
    let config: WebSearchConfig = store
        .get_setting(WEB_SEARCH_SETTING)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    // Store contents may be hand-edited or left by an interrupted development
    // build. Invalid timeout policy must not leave a selected provider usable.
    if config.validate().is_err() {
        return Ok(WebSearchConfig::default());
    }
    Ok(config)
}

async fn write_config(store: &dyn Store, config: &WebSearchConfig) -> Result<()> {
    store
        .set_setting(WEB_SEARCH_SETTING, &serde_json::to_value(config)?)
        .await
}

/// Return the safe public representation of the current configuration.
pub async fn config_info(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<WebSearchConfigInfo> {
    let config = read_config(store).await?;
    let has_credential = host_has_credential(&config, secrets).await;
    let available = host_is_available(&config, has_credential);
    Ok(WebSearchConfigInfo {
        provider: config.provider,
        mode: config.mode,
        timeout_ms: config.timeout_ms,
        has_credential,
        available,
        searxng_base_url: config
            .searxng_base_url()
            .map(|base| base.as_str().to_owned()),
    })
}

/// Whether a key is stored for the selected provider, without returning it.
async fn host_has_credential(config: &WebSearchConfig, secrets: &dyn SecretProvider) -> bool {
    match config.provider {
        Some(provider) => matches!(
            WebSearchCredentials::resolve(secrets, provider).await,
            Ok(WebSearchCredentialState::Present(_))
        ),
        None => false,
    }
}

/// Whether the selected host provider has everything it needs to be invoked
/// on a turn that routes host search here.
///
/// Mode is part of that answer: `off` and `vendor` never invoke the host
/// adapter, so a stored key must not make settings report "available" while
/// the turn surface withholds search. Credential presence stays on
/// [`WebSearchConfigInfo::has_credential`] so the operator can still see that
/// a key is saved for when they turn host search back on.
fn host_is_available(config: &WebSearchConfig, has_credential: bool) -> bool {
    if matches!(config.mode, WebSearchMode::Off | WebSearchMode::Vendor) {
        return false;
    }
    match config.provider {
        // Nothing to authenticate with; the instance URL is what it needs.
        Some(WebSearchProviderKind::Searxng) => config.searxng_base_url().is_some(),
        // Not selectable here, so reaching this arm means hand-edited storage.
        Some(WebSearchProviderKind::ModelProvider) => false,
        Some(_) => has_credential,
        None => false,
    }
}

/// Decide which search one turn gets, from host policy and the model's own
/// capabilities.
///
/// `supports_vendor` is the resolved model row's claim that the routing adapter
/// emits a provider-executed search *during the turn* — not that the vendor
/// documents one — so a model reached over a pass-through route resolves as if
/// it had none.
///
/// `supports_subrequest` is the weaker, more widely available claim: the host
/// can search on this model's behalf with one dedicated call to its provider.
/// It resolves to [`TurnWebSearch::Host`], because that is what it is — the
/// model is offered the host's own tool, and the host decides what runs behind
/// it. Where a model has both, the in-turn search wins: it costs no extra
/// round-trip and the model steers it directly.
///
/// Nothing here consults the chat's permission mode. A vendor search is
/// provider-internal: it makes no egress from this host, reaches no new party
/// with the conversation's data, and is already finished by the time Tidebreak
/// sees it, so there is no decision an approval card could still gate. A
/// sub-request reaches the same party the conversation is already with.
pub async fn resolve_turn_web_search(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    supports_vendor: bool,
    supports_subrequest: bool,
) -> Result<TurnWebSearch> {
    let config = read_config(store).await?;
    let vendor = || {
        TurnWebSearch::Vendor(VendorWebSearch {
            max_uses: VendorWebSearch::DEFAULT_MAX_USES,
        })
    };
    Ok(match config.mode {
        WebSearchMode::Off => TurnWebSearch::Off,
        // The host tool as registered: available or not, the model is offered
        // it and an unconfigured host answers the call with the same
        // configuration error it always has.
        WebSearchMode::Host => TurnWebSearch::Host,
        WebSearchMode::Vendor if supports_vendor => vendor(),
        // The model's own provider, reached through the host's tool because
        // that is the only shape its endpoint will accept alongside an agent
        // turn's tools.
        WebSearchMode::Vendor if supports_subrequest => TurnWebSearch::Host,
        WebSearchMode::Vendor => TurnWebSearch::Off,
        WebSearchMode::Automatic => {
            let has_credential = host_has_credential(&config, secrets).await;
            if host_is_available(&config, has_credential) {
                TurnWebSearch::Host
            } else if supports_vendor {
                vendor()
            } else {
                TurnWebSearch::Host
            }
        }
    })
}

/// Return readiness for every fixed provider without reading or returning any
/// key material. Storage errors are projected to one generic server error so
/// keychain implementation details cannot cross the local API boundary.
pub async fn credentials_info(
    secrets: &dyn SecretProvider,
) -> std::result::Result<WebSearchCredentialsInfo, ServerError> {
    let mut credentials = Vec::with_capacity(CREDENTIAL_PROVIDERS.len());
    for provider in CREDENTIAL_PROVIDERS {
        let has_credential = matches!(
            WebSearchCredentials::resolve(secrets, provider)
                .await
                .map_err(|_| ServerError::internal(
                    "web search credential storage is unavailable"
                ))?,
            WebSearchCredentialState::Present(_)
        );
        credentials.push(WebSearchCredentialReadiness {
            provider,
            has_credential,
        });
    }
    Ok(WebSearchCredentialsInfo { credentials })
}

/// Resolve a local API path segment to a provider that has a fixed credential
/// slot. Deriving this from the allow-list keeps the set of addressable keychain
/// entries in one place.
pub fn credential_provider(value: &str) -> std::result::Result<WebSearchProviderKind, ServerError> {
    CREDENTIAL_PROVIDERS
        .into_iter()
        .find(|provider| provider.as_str() == value)
        .ok_or_else(|| ServerError::not_found(format!("unknown web search provider kind: {value}")))
}

/// The fixed keychain name for a provider that takes a key.
///
/// A credential-free provider has no slot to address, and asking for one is a
/// routing mistake rather than a storage failure — [`credential_provider`]
/// already refuses to resolve one from a path segment.
fn credential_key(
    provider: WebSearchProviderKind,
) -> std::result::Result<&'static str, ServerError> {
    provider.credential_key().ok_or_else(|| {
        ServerError::not_found(format!(
            "web search provider {provider} stores no credential"
        ))
    })
}

/// Store a non-empty, already validated credential under the provider's fixed
/// key. The provider kind is an enum rather than caller-controlled storage
/// input, so this cannot address other application secrets.
pub async fn write_credential(
    secrets: &dyn SecretProvider,
    provider: WebSearchProviderKind,
    api_key: &str,
) -> std::result::Result<WebSearchCredentialReadiness, ServerError> {
    secrets
        .set_secret(credential_key(provider)?, api_key)
        .await
        .map_err(|error| {
            ServerError::credential_storage(error, "web search credential storage is unavailable")
        })?;
    Ok(WebSearchCredentialReadiness {
        provider,
        has_credential: true,
    })
}

/// Delete only the selected provider's fixed credential key.
pub async fn delete_credential(
    secrets: &dyn SecretProvider,
    provider: WebSearchProviderKind,
) -> std::result::Result<WebSearchCredentialReadiness, ServerError> {
    secrets
        .delete_secret(credential_key(provider)?)
        .await
        .map_err(|error| {
            ServerError::credential_storage(error, "web search credential storage is unavailable")
        })?;
    Ok(WebSearchCredentialReadiness {
        provider,
        has_credential: false,
    })
}

/// Apply a non-secret host-policy update and return its public view.
///
/// An explicit `provider: null` is the documented disable path (`PUT` body or
/// `settings web-search select off`). It clears the host selection **and**
/// turns search off for the turn surface, so the model is not left with a
/// vendor fallback under a still-`automatic` mode while settings report
/// "no provider / unavailable". A simultaneous `mode` field still wins, so a
/// caller can clear the host provider while deliberately choosing automatic or
/// vendor. Selecting a provider again while mode is `off` restores
/// `automatic` unless the caller sets mode in the same update.
pub async fn update_config(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    update: WebSearchConfigUpdate,
) -> std::result::Result<WebSearchConfigInfo, ServerError> {
    let mut config = read_config(store).await?;
    if let Some(provider) = update.provider {
        config.provider = provider;
        if update.mode.is_none() {
            if provider.is_none() {
                // Documented "null disables web search": not merely "no host
                // engine while automatic still offers the vendor tool".
                config.mode = WebSearchMode::Off;
            } else if config.mode == WebSearchMode::Off {
                // Re-selecting a provider after disable must make search usable
                // again without a second round-trip to flip mode.
                config.mode = WebSearchMode::Automatic;
            }
        }
    }
    if let Some(mode) = update.mode {
        config.mode = mode;
    }
    if let Some(timeout_ms) = update.timeout_ms {
        config.timeout_ms = timeout_ms;
    }
    if let Some(searxng_base_url) = update.searxng_base_url {
        // Store the canonical form the crate produced, not the raw text, so
        // there is one spelling of an instance URL in the store and in every
        // later comparison.
        config.searxng_base_url = match searxng_base_url {
            Some(value) => Some(
                SearxngBaseUrl::parse(value)
                    .map_err(|_| {
                        ServerError::bad_request(
                            "web search searxng_base_url must be an http or https instance URL with no credentials, query, or fragment",
                        )
                    })?
                    .as_str()
                    .to_owned(),
            ),
            None => None,
        };
    }
    config.validate()?;
    write_config(store, &config).await?;
    config_info(store, secrets).await.map_err(Into::into)
}

/// One opaque failure for everything about resolving host configuration, so
/// keychain and transport details cannot escape through logs or local API
/// responses.
fn unavailable() -> ServerError {
    ServerError::internal("web search configuration is unavailable")
}

/// A transport bound to exactly one origin under the current timeout policy.
fn bound_client(
    origin: OutboundOrigin,
    timeout_ms: u64,
) -> std::result::Result<ReqwestHttpClient, ServerError> {
    ReqwestHttpClient::with_timeout(origin, Duration::from_millis(timeout_ms))
        .map_err(|_| unavailable())
}

/// The stored key for a provider that requires one, or `None` to fail closed.
///
/// Only providers that take a key reach this. A credential-free provider is
/// routed before it, so `NotRequired` here would mean a routing mistake, and
/// answering `None` keeps that mistake a refusal rather than an unauthenticated
/// request.
async fn required_credential(
    secrets: &dyn SecretProvider,
    kind: WebSearchProviderKind,
) -> std::result::Result<Option<WebSearchCredential>, ServerError> {
    match WebSearchCredentials::resolve(secrets, kind).await {
        Ok(WebSearchCredentialState::Present(credential)) => Ok(Some(credential)),
        Ok(WebSearchCredentialState::Missing | WebSearchCredentialState::NotRequired) => Ok(None),
        Err(_) => Err(unavailable()),
    }
}

/// The model one chat would search through, when its provider can serve a
/// dedicated search sub-request.
///
/// `None` covers every reason a chat has no such route: an unreadable or
/// unregistered selection, a pass-through endpoint that cannot promise the
/// vendor's request shape, and a provider with no hosted search at all
/// (Anthropic, whose rows carry a native in-turn search instead).
///
/// The selection is read the way a new execution reads it — the chat's own
/// override, then the global `chat` role selection, then the process default —
/// so the search runs on the model the reader believes they are talking to.
async fn chat_search_model(
    store: &dyn Store,
    chat: SessionId,
    default_model: &str,
) -> Option<SearchModel> {
    let selected = match store
        .get_chat(chat)
        .await
        .ok()
        .flatten()
        .and_then(|chat| chat.model)
    {
        Some(model) => model,
        None => model_roles::read_selection(store, ModelRole::Chat)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| default_model.to_owned()),
    };
    // Hosted callers' gateway selections resolve to nothing here: the search
    // sub-request rides the deployment-resolved provider, which a hosted
    // machine does not have. Decision 62 names this gap and leaves it open.
    let policy = crate::providers::resolve_model_policy(store, &selected, true, None)
        .await
        .ok()
        .flatten()?;
    if !policy.supports_search_subrequest {
        return None;
    }
    Some(SearchModel {
        provider: Some(tidebreak_core::ProviderId::new(policy.provider.as_str())),
        model: policy.route_model.clone(),
        reasoning_model: policy.supports_reasoning,
        reasoning_efforts: policy.reasoning_efforts.clone(),
    })
}

/// Resolve the provider that will run one chat's host-side search. The returned
/// provider is inert until its `search` method is called.
///
/// Two backends can answer, and which one does is the operator's mode:
///
/// - `host` takes the configured engine and nothing else. An operator who named
///   Brave meant Brave, and quietly searching through their model provider
///   instead would send queries somewhere they did not choose.
/// - `vendor` takes the chat's model provider and nothing else, by the mirror of
///   the same argument.
/// - `automatic` prefers the configured engine, because it is the deliberate
///   choice, and falls back to the model provider so a host with no key still
///   searches.
///
/// Every path fails closed as `Ok(None)`: `off`, a missing key for the engines
/// that need one, a missing instance URL for the one that does not, and a chat
/// whose model has no search sub-request. Credentials left in the keychain after
/// search is turned off must not keep host search live.
pub async fn resolve_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    chat: Option<SessionId>,
    providers: Option<&Arc<dyn ProviderResolver>>,
    default_model: &str,
) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, ServerError> {
    let config = read_config(store).await?;
    config.validate()?;
    if matches!(config.mode, WebSearchMode::Off) {
        return Ok(None);
    }

    // Resolved first so `automatic` can fall through to it, and so `vendor` has
    // its only candidate. Building it makes no request: the model provider is
    // as inert here as a bound HTTP client is.
    let model_backed = async {
        if matches!(config.mode, WebSearchMode::Host) {
            return None;
        }
        let providers = providers?;
        let model = chat_search_model(store, chat?, default_model).await?;
        let backend: Arc<dyn WebSearchProvider> =
            Arc::new(ModelProviderSearch::new(providers.resolve().await, model));
        Some(backend)
    };

    if matches!(config.mode, WebSearchMode::Vendor) {
        return Ok(model_backed.await);
    }

    let configured = configured_provider(secrets, &config).await?;
    Ok(match configured {
        Some(provider) => Some(provider),
        None => model_backed.await,
    })
}

/// The engine the operator selected and credentialed, if it is usable.
async fn configured_provider(
    secrets: &dyn SecretProvider,
    config: &WebSearchConfig,
) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, ServerError> {
    let Some(kind) = config.provider else {
        return Ok(None);
    };
    let provider: Arc<dyn WebSearchProvider> = match kind {
        WebSearchProviderKind::Searxng => {
            // The self-hosted case: no credential to resolve, and the address
            // comes from validated host configuration rather than a constant.
            // Without one there is nowhere to dial, which fails closed exactly
            // as a missing key does for the others.
            let Some(base_url) = config.searxng_base_url() else {
                return Ok(None);
            };
            let client = bound_client(base_url.origin(), config.timeout_ms)?;
            Arc::new(SearxngProvider::new(client, base_url))
        }
        // Never an operator selection: it is chosen by mode, above, because the
        // backend follows the chat rather than the configuration.
        WebSearchProviderKind::ModelProvider => return Ok(None),
        kind => {
            let Some(credential) = required_credential(secrets, kind).await? else {
                return Ok(None);
            };
            let origin = OutboundOrigin::fixed(kind).ok_or_else(unavailable)?;
            let client = bound_client(origin, config.timeout_ms)?;
            match kind {
                WebSearchProviderKind::Exa => {
                    Arc::new(ExaProvider::new(client, credential).map_err(|_| unavailable())?)
                }
                WebSearchProviderKind::Tavily => {
                    Arc::new(TavilyProvider::new(client, credential).map_err(|_| unavailable())?)
                }
                WebSearchProviderKind::Brave => {
                    Arc::new(BraveProvider::new(client, credential).map_err(|_| unavailable())?)
                }
                WebSearchProviderKind::Firecrawl => {
                    Arc::new(FirecrawlProvider::new(client, credential).map_err(|_| unavailable())?)
                }
                // Handled above; `OutboundOrigin::fixed` has already refused
                // both.
                WebSearchProviderKind::Searxng | WebSearchProviderKind::ModelProvider => {
                    return Ok(None)
                }
            }
        }
    };
    Ok(Some(provider))
}

struct HostWebSearchResolver {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    /// The model router, when this resolver serves search.
    ///
    /// `None` for extraction, which the model-backed backend cannot do: it
    /// searches by asking a model what it found, and there is no equivalent for
    /// "read exactly this page." Extraction routes to the configured engine or
    /// to the native fetcher, exactly as it did before this backend existed.
    providers: Option<Arc<dyn ProviderResolver>>,
    /// The model this process launched with — the last fallback when neither
    /// the chat nor the global `chat` role names one.
    default_model: String,
}

#[async_trait]
impl WebSearchResolver for HostWebSearchResolver {
    async fn resolve(
        &self,
        chat: Option<SessionId>,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError> {
        resolve_provider(
            &*self.store,
            &*self.secrets,
            chat,
            self.providers.as_ref(),
            &self.default_model,
        )
        .await
        .map_err(|_| WebSearchResolverError)
    }
}

/// Build the inert foreground tool over a live host configuration resolver.
///
/// The registry may keep this object for the server lifetime: each approved
/// call rereads current settings and credentials before any outbound request.
pub fn foreground_tool(
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
    providers: Arc<dyn ProviderResolver>,
    default_model: String,
) -> Box<dyn Tool> {
    Box::new(ChatNetworkPolicyTool::new(
        store.clone(),
        Box::new(WebSearchTool::new(Arc::new(HostWebSearchResolver {
            store,
            secrets,
            providers: Some(providers),
            default_model,
        }))),
    ))
}

/// Native page extraction under live host policy.
///
/// The engine itself is cheap state over stateless transport and resolver
/// values, so each approved call builds one with the timeout the host policy
/// holds *now* — the same read-at-execution rule the provider resolver
/// follows. The timeout is clamped host configuration, never a model argument;
/// an unreadable store falls back to the default rather than failing a fetch
/// over a timeout preference.
struct HostNativePageExtractor {
    store: Arc<dyn Store>,
}

#[async_trait]
impl PageExtractor for HostNativePageExtractor {
    async fn extract_page(
        &self,
        request: &WebExtractRequest,
    ) -> std::result::Result<WebExtractResponse, WebExtractFailure> {
        let timeout_ms = read_config(&*self.store)
            .await
            .map(|config| config.timeout_ms)
            .unwrap_or(DEFAULT_TIMEOUT_MS);
        let extractor = NativeExtractor::new(
            ReqwestPageFetcher,
            TokioHostResolver,
            Duration::from_millis(timeout_ms),
        )
        .map_err(|_| WebExtractFailure::PageUnreachable)?;
        extractor.extract_page(request).await
    }
}

/// Media type a fetched page is stored under.
///
/// Extraction produces readable markdown, and this is a claim about the text of
/// record rather than about the page: the HTML that was fetched is not retained
/// and is not what a citation addresses.
const EXTRACTED_PAGE_MEDIA_TYPE: &str = "text/markdown";
/// Longest title a fetched page may contribute, matching the document ingest
/// route's own ceiling.
const MAX_EXTRACTED_PAGE_TITLE_CHARS: usize = 255;

/// Keep each fetched page as a conversation source.
///
/// A page arrives as canonical text and has no original document to retain.
/// The stored text is therefore the only record of the page, so it is written
/// byte for byte as extracted.
struct HostExtractedPageSink {
    store: Arc<dyn Store>,
}

#[async_trait]
impl ExtractedPageSink for HostExtractedPageSink {
    async fn store_page(
        &self,
        chat_id: SessionId,
        page: &WebExtractResponse,
        fetched_at: DateTime<Utc>,
    ) -> std::result::Result<StoredExtractedPage, ExtractedPageSinkError> {
        let title = page
            .title
            .chars()
            .take(MAX_EXTRACTED_PAGE_TITLE_CHARS)
            .collect::<String>();
        let source = DocumentUpsert {
            // Derived from the conversation and the page URL, so re-reading a
            // page during a long investigation revises the one source rather
            // than accumulating a source per fetch.
            id: DocumentId::derive_for_chat(chat_id, &page.url),
            chat_id: Some(chat_id),
            project_id: None,
            origin_uri: Some(page.url.clone()),
            media_type: EXTRACTED_PAGE_MEDIA_TYPE.into(),
            title: (!title.is_empty()).then_some(title),
            // Keep exactly what the model read so its human-scale locators and
            // quoted prose remain meaningful when the document is reopened.
            canonical_text: page.content.clone(),
            updated_at: fetched_at,
        };
        let record = self
            .store
            .upsert_document(&source)
            .await
            .map_err(|_| ExtractedPageSinkError)?;
        // The stored document must be the same one the model was shown.
        if record.canonical_text != page.content {
            return Err(ExtractedPageSinkError);
        }
        Ok(StoredExtractedPage {
            document_id: record.id,
        })
    }
}

/// Build the inert foreground extraction tool.
///
/// Registered whenever web search is, and usable without any provider: the
/// deterministic route is vendor extraction when the configured provider
/// implements it, the native engine otherwise — including when the provider is
/// search-only or absent. Every page it extracts becomes a citable source of
/// the conversation that asked for it.
pub fn foreground_extract_tool(
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
) -> Box<dyn Tool> {
    let tool = WebExtractTool::new(
        Arc::new(HostWebSearchResolver {
            store: store.clone(),
            secrets,
            // Search-only backend, so extraction never routes to it.
            providers: None,
            default_model: String::new(),
        }),
        Some(Arc::new(HostNativePageExtractor {
            store: store.clone(),
        })),
    )
    .with_page_sink(Arc::new(HostExtractedPageSink {
        store: store.clone(),
    }));
    Box::new(ChatNetworkPolicyTool::new(store, Box::new(tool)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use chrono::Utc;
    use tidebreak_core::{AgentError, Chat, DbStore, ToolCtx};

    use super::*;

    #[derive(Default)]
    struct TestSecrets(Mutex<HashMap<String, String>>);

    #[async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }

        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(key.to_owned(), value.to_owned());
            Ok(())
        }

        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    async fn test_store() -> (DbStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("web-search.db").display()
        ))
        .await
        .unwrap();
        (store, dir)
    }

    /// Resolution with no chat and no model router: the configured engine, or
    /// nothing. This is what `resolve_provider` meant before it learned to
    /// search through a chat's own model provider, and what every test written
    /// against that behaviour still means.
    async fn resolve_configured(
        store: &dyn Store,
        secrets: &dyn SecretProvider,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, ServerError> {
        resolve_provider(store, secrets, None, None, "").await
    }

    /// A resolver standing in for the model router. It is never asked to
    /// stream here — building the backend is all these tests observe — so an
    /// unconfigured route is the honest fake.
    struct TestModelRouter;

    #[async_trait]
    impl ProviderResolver for TestModelRouter {
        async fn resolve(&self) -> Arc<dyn tidebreak_core::ModelProvider> {
            Arc::new(crate::provider::UnconfiguredProvider)
        }
    }

    /// Store a chat pinned to `model`, and return the router to resolve against.
    async fn chat_on(store: &DbStore, model: &str) -> (SessionId, Arc<dyn ProviderResolver>) {
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: None,
            model: Some(model.to_owned()),
            reasoning_effort: None,
            permission_mode: None,
            network_policy: NetworkPolicy::Open,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        (chat.id, Arc::new(TestModelRouter))
    }

    struct UnexpectedNetworkTool;

    #[async_trait]
    impl Tool for UnexpectedNetworkTool {
        fn spec(&self) -> ToolSpec {
            tidebreak_core::web_search_tool_spec()
        }

        fn approval_class(&self) -> ApprovalClass {
            ApprovalClass::Sensitive
        }

        async fn execute(&self, _ctx: &ToolCtx, _args: serde_json::Value) -> Result<ToolOutput> {
            panic!("offline policy must stop the network tool before execution")
        }
    }

    #[tokio::test]
    async fn offline_chat_network_denial_is_actionable_and_structured() {
        let (store, _dir) = test_store().await;
        let chat = Chat {
            id: SessionId::new(),
            project_id: None,
            title: None,
            model: None,
            reasoning_effort: None,
            permission_mode: None,
            network_policy: NetworkPolicy::Off,
            attachment_revision: 0,
            root_attachments: Vec::new(),
            memory_incognito: false,
            created_at: Utc::now(),
        };
        store.create_chat(&chat).await.unwrap();
        let tool = ChatNetworkPolicyTool::new(Arc::new(store), Box::new(UnexpectedNetworkTool));

        let output = tool
            .execute(
                &ToolCtx::without_private_scratch(chat.id, None),
                serde_json::json!({"query": "anything"}),
            )
            .await
            .unwrap();

        assert_eq!(output.content, NETWORK_OFF_MESSAGE);
        assert_eq!(output.data.unwrap()["error_code"], NETWORK_OFF_ERROR_CODE);
    }

    #[test]
    fn default_is_disabled_and_timeout_is_bounded() {
        let config = WebSearchConfig::default();
        assert_eq!(config.provider, None);
        assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
        assert!(config.validate().is_ok());
        assert!(WebSearchConfig {
            timeout_ms: MIN_TIMEOUT_MS - 1,
            provider: Some(WebSearchProviderKind::Exa),
            ..WebSearchConfig::default()
        }
        .validate()
        .is_err());
        assert!(WebSearchConfig {
            timeout_ms: MAX_TIMEOUT_MS + 1,
            provider: Some(WebSearchProviderKind::Tavily),
            ..WebSearchConfig::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn selection_has_no_endpoint_or_secret_reference_field() {
        let json = serde_json::to_value(WebSearchConfig {
            provider: Some(WebSearchProviderKind::Exa),
            ..WebSearchConfig::default()
        })
        .unwrap();
        assert_eq!(json["provider"], "exa");
        assert!(json.get("endpoint").is_none());
        assert!(json.get("credential").is_none());
    }

    #[test]
    fn unsupported_provider_cannot_deserialize_into_selection() {
        let config = serde_json::from_value::<WebSearchConfig>(serde_json::json!({
            "provider": "untrusted_proxy",
            "timeout_ms": DEFAULT_TIMEOUT_MS,
        }));
        assert!(config.is_err());
    }

    #[tokio::test]
    async fn firecrawl_credential_lifecycle_controls_readiness_and_resolution() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();

        let readiness = credentials_info(&secrets).await.unwrap();
        assert_eq!(
            readiness.credentials,
            vec![
                WebSearchCredentialReadiness {
                    provider: WebSearchProviderKind::Exa,
                    has_credential: false,
                },
                WebSearchCredentialReadiness {
                    provider: WebSearchProviderKind::Tavily,
                    has_credential: false,
                },
                WebSearchCredentialReadiness {
                    provider: WebSearchProviderKind::Brave,
                    has_credential: false,
                },
                WebSearchCredentialReadiness {
                    provider: WebSearchProviderKind::Firecrawl,
                    has_credential: false,
                },
            ]
        );

        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Firecrawl)),
                mode: Some(WebSearchMode::Host),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        let stored = write_credential(&secrets, WebSearchProviderKind::Firecrawl, "fc-key")
            .await
            .unwrap();
        assert!(stored.has_credential);

        let info = config_info(&store, &secrets).await.unwrap();
        assert!(info.has_credential);
        assert!(info.available);
        let resolved = resolve_configured(&store, &secrets)
            .await
            .unwrap()
            .expect("a stored Firecrawl key resolves its adapter");
        assert_eq!(resolved.kind(), WebSearchProviderKind::Firecrawl);

        let removed = delete_credential(&secrets, WebSearchProviderKind::Firecrawl)
            .await
            .unwrap();
        assert!(!removed.has_credential);
        assert!(!config_info(&store, &secrets).await.unwrap().available);
        assert!(resolve_configured(&store, &secrets)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn selected_provider_without_a_fixed_key_fails_closed() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let update = update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Exa)),
                mode: None,
                timeout_ms: Some(MIN_TIMEOUT_MS),
                searxng_base_url: None,
            },
        )
        .await;
        let info = match update {
            Ok(info) => info,
            Err(_) => panic!("valid local web-search configuration was rejected"),
        };

        assert_eq!(info.provider, Some(WebSearchProviderKind::Exa));
        assert!(!info.has_credential);
        assert!(!info.available);
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(None)
        ));
        assert!(secrets.0.lock().unwrap().is_empty());
    }

    /// The credential-free provider: selection alone is not enough, an
    /// instance URL is, and no key is ever read or written for it.
    #[tokio::test]
    async fn a_credential_free_provider_turns_on_with_an_instance_url_and_no_key() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let select = |base: Option<Option<String>>| WebSearchConfigUpdate {
            provider: Some(Some(WebSearchProviderKind::Searxng)),
            mode: None,
            timeout_ms: None,
            searxng_base_url: base,
        };

        // Selected but with nowhere to dial: fails closed exactly as a
        // credentialed provider without its key does.
        let info = update_config(&store, &secrets, select(None)).await.unwrap();
        assert_eq!(info.provider, Some(WebSearchProviderKind::Searxng));
        assert!(!info.available);
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(None)
        ));

        // A malformed instance URL is rejected at PUT time rather than
        // silently widening where the transport may dial.
        for invalid in ["not a url", "ftp://localhost:8888", "http://user:pw@host"] {
            assert!(
                update_config(&store, &secrets, select(Some(Some(invalid.into()))))
                    .await
                    .is_err(),
                "{invalid} was accepted as an instance URL"
            );
        }

        // A valid one stores canonically and makes the provider usable, with
        // `has_credential` still false because there is no key slot at all.
        let info = update_config(
            &store,
            &secrets,
            select(Some(Some("http://localhost:8888/".into()))),
        )
        .await
        .unwrap();
        assert_eq!(
            info.searxng_base_url.as_deref(),
            Some("http://localhost:8888")
        );
        assert!(!info.has_credential);
        assert!(info.available);
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(Some(_))
        ));
        // Nothing about a credential-free provider touches the keychain, and
        // it is not addressable as a credential slot either.
        assert!(secrets.0.lock().unwrap().is_empty());
        assert!(credential_provider("searxng").is_err());
    }

    #[tokio::test]
    async fn disabled_configuration_does_not_even_read_a_secret() {
        struct FailingSecrets;

        #[async_trait]
        impl SecretProvider for FailingSecrets {
            async fn get_secret(&self, _key: &str) -> Result<Option<String>> {
                Err(AgentError::Secret("must not be read".into()))
            }

            async fn set_secret(&self, _key: &str, _value: &str) -> Result<()> {
                unreachable!()
            }

            async fn delete_secret(&self, _key: &str) -> Result<()> {
                unreachable!()
            }
        }

        let (store, _dir) = test_store().await;
        let secrets = FailingSecrets;
        let info = config_info(&store, &secrets).await.unwrap();
        assert_eq!(info.provider, None);
        assert!(!info.has_credential);
        assert!(!info.available);
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(None)
        ));
    }

    /// The whole point of the mode: which search a turn gets, decided from
    /// host policy and the model in front of it.
    #[tokio::test]
    async fn turn_resolution_prefers_a_usable_host_and_falls_back_to_the_vendor() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let resolve =
            |supports_vendor| resolve_turn_web_search(&store, &secrets, supports_vendor, false);
        let vendor = TurnWebSearch::Vendor(VendorWebSearch {
            max_uses: VendorWebSearch::DEFAULT_MAX_USES,
        });

        // Nothing configured. A capable model searches on its provider; one
        // with no vendor search — a gateway-served row, say — is left exactly
        // where it was, holding a host tool that will report it needs setting
        // up when it is called.
        assert_eq!(resolve(true).await.unwrap(), vendor);
        assert_eq!(resolve(false).await.unwrap(), TurnWebSearch::Host);

        // A selected provider is not yet a usable one: without its key there
        // is still nothing on this host to run the search.
        let select_exa = |mode| WebSearchConfigUpdate {
            provider: Some(Some(WebSearchProviderKind::Exa)),
            mode: Some(mode),
            timeout_ms: None,
            searxng_base_url: None,
        };
        update_config(&store, &secrets, select_exa(WebSearchMode::Automatic))
            .await
            .unwrap();
        assert_eq!(resolve(true).await.unwrap(), vendor);

        // With the key in place the host provider wins: the operator chose it,
        // and automatic means prefer what is actually configured here.
        write_credential(&secrets, WebSearchProviderKind::Exa, "key")
            .await
            .unwrap();
        assert_eq!(resolve(true).await.unwrap(), TurnWebSearch::Host);

        // The explicit modes override the preference in both directions, and
        // the vendor choice never silently falls back to the host provider it
        // was chosen instead of.
        update_config(&store, &secrets, select_exa(WebSearchMode::Vendor))
            .await
            .unwrap();
        assert_eq!(resolve(true).await.unwrap(), vendor);
        assert_eq!(resolve(false).await.unwrap(), TurnWebSearch::Off);

        update_config(&store, &secrets, select_exa(WebSearchMode::Off))
            .await
            .unwrap();
        assert_eq!(resolve(true).await.unwrap(), TurnWebSearch::Off);
    }

    /// The screenshot this whole path exists to fix: automatic mode, no key
    /// stored, a chat on a model whose provider has no in-turn search. Before
    /// the sub-request backend the turn held a host tool with nothing behind
    /// it, and the reader got "web search needs a provider".
    #[tokio::test]
    async fn a_model_with_only_a_sub_request_still_gets_a_working_host_tool() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let resolve =
            |vendor, subrequest| resolve_turn_web_search(&store, &secrets, vendor, subrequest);

        // Automatic with nothing configured: the tool is offered either way,
        // but now something answers it.
        assert_eq!(resolve(false, true).await.unwrap(), TurnWebSearch::Host);

        // Vendor mode is where the difference is visible. It used to mean
        // "off" for these models, because they claim no in-turn search.
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: None,
                mode: Some(WebSearchMode::Vendor),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(resolve(false, true).await.unwrap(), TurnWebSearch::Host);
        assert_eq!(resolve(false, false).await.unwrap(), TurnWebSearch::Off);

        // A model with both keeps the in-turn search: it costs no extra
        // round-trip and the model steers it directly.
        assert_eq!(
            resolve(true, true).await.unwrap(),
            TurnWebSearch::Vendor(VendorWebSearch {
                max_uses: VendorWebSearch::DEFAULT_MAX_USES,
            })
        );
    }

    /// An operator who named an engine meant that engine. Falling back to the
    /// chat's model provider would send their queries somewhere they did not
    /// choose, which is the one thing explicit host mode rules out.
    #[tokio::test]
    async fn host_mode_never_resolves_the_model_backed_backend() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let (chat, router) = chat_on(&store, "openai::gpt-5.6-sol").await;

        for mode in [WebSearchMode::Host, WebSearchMode::Off] {
            update_config(
                &store,
                &secrets,
                WebSearchConfigUpdate {
                    provider: None,
                    mode: Some(mode),
                    timeout_ms: None,
                    searxng_base_url: None,
                },
            )
            .await
            .unwrap();

            let resolved = resolve_provider(&store, &secrets, Some(chat), Some(&router), "")
                .await
                .unwrap();
            assert!(resolved.is_none(), "{mode:?} resolved a backend");
        }

        // Vendor mode is the same chat and the same router, and it does
        // resolve — so the refusals above are the mode, not a broken fixture.
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: None,
                mode: Some(WebSearchMode::Vendor),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        let resolved = resolve_provider(&store, &secrets, Some(chat), Some(&router), "")
            .await
            .unwrap()
            .expect("vendor mode resolves the chat's own model provider");
        assert_eq!(resolved.kind(), WebSearchProviderKind::ModelProvider);
    }

    /// Anthropic rows carry a native in-turn search, so they need nothing here
    /// — and a chat on one must not quietly acquire a second, worse search
    /// path built on an extra round-trip.
    #[tokio::test]
    async fn a_chat_whose_model_has_no_sub_request_resolves_nothing() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let (chat, router) = chat_on(&store, "anthropic::claude-opus-5").await;
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: None,
                mode: Some(WebSearchMode::Vendor),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();

        let resolved = resolve_provider(&store, &secrets, Some(chat), Some(&router), "")
            .await
            .unwrap();

        assert!(resolved.is_none());
    }

    /// Automatic prefers what the operator configured, and only falls through
    /// to the model provider when this host has nothing to run.
    #[tokio::test]
    async fn automatic_prefers_a_usable_engine_over_the_model_provider() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let (chat, router) = chat_on(&store, "openai::gpt-5.6-sol").await;
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Exa)),
                mode: Some(WebSearchMode::Automatic),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();

        // Selected but unusable: no key, so the model provider answers.
        let resolved = resolve_provider(&store, &secrets, Some(chat), Some(&router), "")
            .await
            .unwrap()
            .expect("an unusable engine falls through");
        assert_eq!(resolved.kind(), WebSearchProviderKind::ModelProvider);

        // With the key in place the operator's choice wins again.
        write_credential(&secrets, WebSearchProviderKind::Exa, "key")
            .await
            .unwrap();
        let resolved = resolve_provider(&store, &secrets, Some(chat), Some(&router), "")
            .await
            .unwrap()
            .expect("a credentialed engine resolves");
        assert_eq!(resolved.kind(), WebSearchProviderKind::Exa);
    }

    /// `PUT { "provider": null }` is the documented disable and what
    /// `settings web-search select off` sends. It must turn search off for the
    /// turn surface, not leave mode automatic so a vendor-capable model still
    /// gets a working `web_search` while settings report unavailable.
    #[tokio::test]
    async fn clearing_the_provider_turns_search_off_for_the_turn_surface() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        write_credential(&secrets, WebSearchProviderKind::Tavily, "still-present")
            .await
            .unwrap();
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Tavily)),
                mode: Some(WebSearchMode::Automatic),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resolve_turn_web_search(&store, &secrets, true, false)
                .await
                .unwrap(),
            TurnWebSearch::Host
        );

        // Same body the CLI disable path writes: provider null, mode omitted.
        let info = update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(None),
                mode: None,
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(info.provider, None);
        assert_eq!(info.mode, WebSearchMode::Off);
        assert!(!info.available);
        assert_eq!(
            resolve_turn_web_search(&store, &secrets, true, false)
                .await
                .unwrap(),
            TurnWebSearch::Off
        );
        assert_eq!(
            resolve_turn_web_search(&store, &secrets, false, false)
                .await
                .unwrap(),
            TurnWebSearch::Off
        );
        // A leftover key must not keep host search live under Off.
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(None)
        ));

        // Selecting a provider again without an explicit mode restores a
        // usable default rather than leaving Off stuck under a selection.
        let info = update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Tavily)),
                mode: None,
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(info.mode, WebSearchMode::Automatic);
        assert_eq!(
            resolve_turn_web_search(&store, &secrets, true, false)
                .await
                .unwrap(),
            TurnWebSearch::Host
        );
    }

    /// Mode Off must refuse host adapters even when a provider and key remain
    /// configured (the UI Off path keeps the selection so turning search back
    /// on does not re-ask for the key).
    #[tokio::test]
    async fn mode_off_refuses_host_provider_resolution_with_key_still_present() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        write_credential(&secrets, WebSearchProviderKind::Exa, "key")
            .await
            .unwrap();
        update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Exa)),
                mode: Some(WebSearchMode::Off),
                timeout_ms: None,
                searxng_base_url: None,
            },
        )
        .await
        .unwrap();

        let info = config_info(&store, &secrets).await.unwrap();
        assert_eq!(info.mode, WebSearchMode::Off);
        assert_eq!(info.provider, Some(WebSearchProviderKind::Exa));
        // Key remains for when search is turned back on, but available must
        // agree with the turn surface: Off means no host search will run.
        assert!(info.has_credential);
        assert!(!info.available);
        assert_eq!(
            resolve_turn_web_search(&store, &secrets, true, false)
                .await
                .unwrap(),
            TurnWebSearch::Off
        );
        assert!(matches!(
            resolve_configured(&store, &secrets).await,
            Ok(None)
        ));
    }

    /// A configuration written before the mode existed keeps the behavior it
    /// had, rather than reading back as whichever variant sorts first.
    #[tokio::test]
    async fn a_stored_configuration_without_a_mode_reads_back_as_automatic() {
        let (store, _dir) = test_store().await;
        store
            .set_setting(
                WEB_SEARCH_SETTING,
                &serde_json::json!({ "provider": "tavily", "timeout_ms": DEFAULT_TIMEOUT_MS }),
            )
            .await
            .unwrap();

        assert_eq!(
            read_config(&store).await.unwrap().mode,
            WebSearchMode::Automatic
        );
    }

    /// Persisted policy may be hand-edited or left by an interrupted build.
    /// Neither an out-of-range timeout nor an instance URL that would widen
    /// egress may leave a selected provider usable.
    #[tokio::test]
    async fn invalid_persisted_policy_reverts_to_disabled() {
        for invalid in [
            serde_json::json!({ "provider": "tavily", "timeout_ms": MAX_TIMEOUT_MS + 1 }),
            serde_json::json!({
                "provider": "searxng",
                "timeout_ms": DEFAULT_TIMEOUT_MS,
                "searxng_base_url": "http://operator:secret@localhost:8888",
            }),
        ] {
            let (store, _dir) = test_store().await;
            store
                .set_setting(WEB_SEARCH_SETTING, &invalid)
                .await
                .unwrap();

            assert_eq!(
                read_config(&store).await.unwrap(),
                WebSearchConfig::default(),
                "{invalid} did not fail closed"
            );
        }
    }
}
