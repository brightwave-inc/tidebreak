//! Host-owned configuration and provider selection for web search.
//!
//! A selected provider becomes usable only when the sandbox worker or approved
//! foreground tool explicitly asks [`resolve_provider`] for it. Provider
//! endpoints are fixed in `openwave-web-search`; this config never accepts an
//! endpoint, a secret reference, or a model-controlled network target.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use openwave_core::{Result, SecretProvider, Store};
use openwave_web_search::{
    ExaProvider, ReqwestHttpClient, TavilyProvider, WebSearchCredentials, WebSearchProvider,
    WebSearchProviderKind, WebSearchResolver, WebSearchResolverError, WebSearchTool,
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

/// The fixed providers this host can hold a credential for. Keeping this
/// allow-list here means a local API route can never turn an arbitrary path
/// segment into a keychain key.
const CREDENTIAL_PROVIDERS: [WebSearchProviderKind; 2] =
    [WebSearchProviderKind::Exa, WebSearchProviderKind::Tavily];

/// Non-secret host configuration. `provider: None` is the safe default: no
/// credential lookup and no possible outbound request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<WebSearchProviderKind>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
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
        Ok(())
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// Public state returned by the local API. It intentionally reports only
/// selection and credential presence — key material never crosses the secret
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct WebSearchConfigInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<WebSearchProviderKind>,
    pub timeout_ms: u64,
    pub has_credential: bool,
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
        Some(provider) => WebSearchCredentials::load(secrets, provider)
            .await
            .ok()
            .flatten()
            .is_some(),
        None => false,
    };
    Ok(WebSearchConfigInfo {
        provider: config.provider,
        timeout_ms: config.timeout_ms,
        has_credential,
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
        let has_credential = WebSearchCredentials::load(secrets, provider)
            .await
            .map_err(|_| ServerError::internal("web search credential storage is unavailable"))?
            .is_some();
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

/// Store a non-empty, already validated credential under the provider's fixed
/// key. The provider kind is an enum rather than caller-controlled storage
/// input, so this cannot address other application secrets.
pub async fn write_credential(
    secrets: &dyn SecretProvider,
    provider: WebSearchProviderKind,
    api_key: &str,
) -> std::result::Result<WebSearchCredentialReadiness, ServerError> {
    secrets
        .set_secret(provider.credential_key(), api_key)
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
        .delete_secret(provider.credential_key())
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
    config.validate()?;
    write_config(store, &config).await?;
    config_info(store, secrets).await.map_err(Into::into)
}

/// Resolve the explicitly selected, credentialed provider for host execution.
/// The returned provider is inert until its `search` method is called.
///
/// Secret resolution failures are intentionally projected to one generic
/// message so keychain implementation details cannot escape through logs or
/// local API responses. A missing key fails closed as `Ok(None)`.
pub async fn resolve_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> std::result::Result<Option<Arc<dyn WebSearchProvider>>, ServerError> {
    let config = read_config(store).await?;
    config.validate()?;
    let Some(kind) = config.provider else {
        return Ok(None);
    };
    let credential = WebSearchCredentials::load(secrets, kind)
        .await
        .map_err(|_| ServerError::internal("web search configuration is unavailable"))?;
    let Some(credential) = credential else {
        return Ok(None);
    };
    let client = ReqwestHttpClient::with_timeout(kind, Duration::from_millis(config.timeout_ms))
        .map_err(|_| ServerError::internal("web search configuration is unavailable"))?;
    let provider: Arc<dyn WebSearchProvider> = match kind {
        WebSearchProviderKind::Exa => Arc::new(
            ExaProvider::new(client, credential)
                .map_err(|_| ServerError::internal("web search configuration is unavailable"))?,
        ),
        WebSearchProviderKind::Tavily => Arc::new(
            TavilyProvider::new(client, credential)
                .map_err(|_| ServerError::internal("web search configuration is unavailable"))?,
        ),
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
            provider: Some(WebSearchProviderKind::Exa),
            timeout_ms: MIN_TIMEOUT_MS - 1,
        }
        .validate()
        .is_err());
        assert!(WebSearchConfig {
            provider: Some(WebSearchProviderKind::Tavily),
            timeout_ms: MAX_TIMEOUT_MS + 1,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn selection_has_no_endpoint_or_secret_reference_field() {
        let json = serde_json::to_value(WebSearchConfig {
            provider: Some(WebSearchProviderKind::Exa),
            timeout_ms: DEFAULT_TIMEOUT_MS,
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
            },
        )
        .await;
        let info = match update {
            Ok(info) => info,
            Err(_) => panic!("valid local web-search configuration was rejected"),
        };

        assert_eq!(info.provider, Some(WebSearchProviderKind::Exa));
        assert!(!info.has_credential);
        assert!(matches!(resolve_provider(&store, &secrets).await, Ok(None)));
        assert!(secrets.0.lock().unwrap().is_empty());
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
        assert!(matches!(resolve_provider(&store, &secrets).await, Ok(None)));
    }

    #[tokio::test]
    async fn invalid_persisted_timeout_reverts_to_disabled_policy() {
        let (store, _dir) = test_store().await;
        store
            .set_setting(
                WEB_SEARCH_SETTING,
                &serde_json::json!({
                    "provider": "tavily",
                    "timeout_ms": MAX_TIMEOUT_MS + 1,
                }),
            )
            .await
            .unwrap();

        assert_eq!(
            read_config(&store).await.unwrap(),
            WebSearchConfig::default()
        );
    }
}
