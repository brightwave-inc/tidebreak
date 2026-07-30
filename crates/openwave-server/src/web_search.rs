//! Host-owned configuration and provider selection for web search.
//!
//! A selected provider becomes usable only when the sandbox worker or approved
//! foreground tool explicitly asks [`resolve_provider`] for it. Provider
//! endpoints are fixed in `openwave-web-search`; this config never accepts an
//! endpoint, a secret reference, or a model-controlled network target.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use openwave_core::{ChatId, DocumentId, DocumentUpsert, Result, SecretProvider, Store};
use openwave_web_search::{
    BraveProvider, ExaProvider, ExtractedPageSink, ExtractedPageSinkError, NativeExtractor,
    OutboundOrigin, PageExtractor, ReqwestHttpClient, ReqwestPageFetcher, SearxngBaseUrl,
    SearxngProvider, StoredExtractedPage, TavilyProvider, TokioHostResolver, WebExtractFailure,
    WebExtractRequest, WebExtractResponse, WebExtractTool, WebSearchCredential,
    WebSearchCredentialState, WebSearchCredentials, WebSearchProvider, WebSearchProviderKind,
    WebSearchResolver, WebSearchResolverError, WebSearchTool,
};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

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
const CREDENTIAL_PROVIDERS: [WebSearchProviderKind; 3] = [
    WebSearchProviderKind::Exa,
    WebSearchProviderKind::Tavily,
    WebSearchProviderKind::Brave,
];

/// Non-secret host configuration. `provider: None` is the safe default: no
/// credential lookup and no possible outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<WebSearchProviderKind>,
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

/// Credential readiness for every provider OpenWave supports locally.
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
    let has_credential = match config.provider {
        Some(provider) => matches!(
            WebSearchCredentials::resolve(secrets, provider).await,
            Ok(WebSearchCredentialState::Present(_))
        ),
        None => false,
    };
    let available = match config.provider {
        // Nothing to authenticate with; the instance URL is what it needs.
        Some(WebSearchProviderKind::Searxng) => config.searxng_base_url().is_some(),
        Some(_) => has_credential,
        None => false,
    };
    Ok(WebSearchConfigInfo {
        provider: config.provider,
        timeout_ms: config.timeout_ms,
        has_credential,
        available,
        searxng_base_url: config
            .searxng_base_url()
            .map(|base| base.as_str().to_owned()),
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
        .map_err(|_| ServerError::internal("web search credential storage is unavailable"))?;
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
        .map_err(|_| ServerError::internal("web search credential storage is unavailable"))?;
    Ok(WebSearchCredentialReadiness {
        provider,
        has_credential: false,
    })
}

/// Apply a non-secret host-policy update and return its public view.
pub async fn update_config(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    update: WebSearchConfigUpdate,
) -> std::result::Result<WebSearchConfigInfo, ServerError> {
    let mut config = read_config(store).await?;
    if let Some(provider) = update.provider {
        config.provider = provider;
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

/// Resolve the explicitly selected provider for host execution. The returned
/// provider is inert until its `search` method is called.
///
/// Every path fails closed as `Ok(None)`: a missing key for the providers that
/// need one, and a missing instance URL for the one that does not.
pub async fn resolve_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, ServerError> {
    let config = read_config(store).await?;
    config.validate()?;
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
                // Handled above; `OutboundOrigin::fixed` has already refused it.
                WebSearchProviderKind::Searxng => return Ok(None),
            }
        }
    };
    Ok(Some(provider))
}

struct HostWebSearchResolver {
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
}

#[async_trait]
impl WebSearchResolver for HostWebSearchResolver {
    async fn resolve(
        &self,
    ) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, WebSearchResolverError> {
        resolve_provider(&*self.store, &*self.secrets)
            .await
            .map_err(|_| WebSearchResolverError)
    }
}

/// Build the inert foreground tool over a live host configuration resolver.
///
/// The registry may keep this object for the server lifetime: each approved
/// call rereads current settings and credentials before any outbound request.
pub(crate) fn foreground_tool(
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
) -> WebSearchTool {
    WebSearchTool::new(Arc::new(HostWebSearchResolver { store, secrets }))
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
/// The source is written through the canonical-text path rather than the staged
/// blob workflow, because a page arrives already parsed: extraction *is* the
/// parse, and there is no original document to re-derive text from. That has a
/// consequence worth stating — the stored text is the only record of the page,
/// so it is written byte for byte as extracted, and everything that cannot be
/// recovered later travels with it.
struct HostExtractedPageSink {
    store: Arc<dyn Store>,
}

/// Identity of the engine that produced a stored page's text.
///
/// Recorded as the source's canonical fingerprint, which is the column that
/// already answers "what produced this text" for parsed sources. It matters
/// more here than there: a parsed source keeps its original bytes, so its
/// provenance can be recomputed, while a fetched page cannot be fetched again
/// and get the same answer. Whether a cited passage came from a vendor's
/// rendering of the page or from the host's own parse is knowable only if it
/// was written down at the time.
fn extraction_fingerprint(page: &WebExtractResponse) -> String {
    format!("web-extract={}", page.extraction_method)
}

#[async_trait]
impl ExtractedPageSink for HostExtractedPageSink {
    async fn store_page(
        &self,
        chat_id: ChatId,
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
            source_uri: Some(page.url.clone()),
            media_type: EXTRACTED_PAGE_MEDIA_TYPE.into(),
            title: (!title.is_empty()).then_some(title),
            // Keep exactly what the model read so its human-scale locators and
            // quoted prose remain meaningful when the document is reopened.
            canonical_text: page.content.clone(),
            canonical_fingerprint: Some(extraction_fingerprint(page)),
            // A fetched page has no retained page or tree map.
            source_regions: Vec::new(),
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
pub(crate) fn foreground_extract_tool(
    store: Arc<dyn Store>,
    secrets: Arc<dyn SecretProvider>,
) -> WebExtractTool {
    WebExtractTool::new(
        Arc::new(HostWebSearchResolver {
            store: store.clone(),
            secrets,
        }),
        Some(Arc::new(HostNativePageExtractor {
            store: store.clone(),
        })),
    )
    .with_page_sink(Arc::new(HostExtractedPageSink { store }))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use openwave_core::{AgentError, DbStore};

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
    async fn selected_provider_without_a_fixed_key_fails_closed() {
        let (store, _dir) = test_store().await;
        let secrets = TestSecrets::default();
        let update = update_config(
            &store,
            &secrets,
            WebSearchConfigUpdate {
                provider: Some(Some(WebSearchProviderKind::Exa)),
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
        assert!(matches!(resolve_provider(&store, &secrets).await, Ok(None)));
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
            timeout_ms: None,
            searxng_base_url: base,
        };

        // Selected but with nowhere to dial: fails closed exactly as a
        // credentialed provider without its key does.
        let info = update_config(&store, &secrets, select(None)).await.unwrap();
        assert_eq!(info.provider, Some(WebSearchProviderKind::Searxng));
        assert!(!info.available);
        assert!(matches!(resolve_provider(&store, &secrets).await, Ok(None)));

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
            resolve_provider(&store, &secrets).await,
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
        assert!(matches!(resolve_provider(&store, &secrets).await, Ok(None)));
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
