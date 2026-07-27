//! Configurable model providers.
//!
//! A provider is `{ kind, enabled, base_url? }` plus a credential held in the
//! [`SecretProvider`](openwave_core::SecretProvider) (never the DB). Credentials
//! are a typed JSON blob (`{"type":"api_key","key":"…"}` today; `oauth` later)
//! so new auth shapes don't force a schema redesign.
//!
//! Non-secret config lives in the [`Store`](openwave_core::Store) under
//! `provider.<kind>`. The legacy `provider.anthropic.api_key` secret and the
//! `PUT /settings/api-key` shim still work — they read/write the anthropic
//! credential in the new blob shape.

use serde::{Deserialize, Serialize};

use openwave_core::{AgentConfig, ProviderId, ReasoningEffort, Result, SecretProvider, Store};

use crate::error::ServerError;
use crate::model_registry::{self, InputModality, ModelSpec};

/// Setting-key prefix for per-provider non-secret config (`provider.<kind>`).
const PROVIDER_SETTING_PREFIX: &str = "provider.";

/// Secret-key suffix for the typed credential blob (`provider.<kind>.credential`).
const CREDENTIAL_SUFFIX: &str = ".credential";

/// Legacy Anthropic API-key secret (pre-providers). Still read as a fallback.
pub const LEGACY_ANTHROPIC_API_KEY: &str = "provider.anthropic.api_key";

/// The known provider kinds. `#[non_exhaustive]` so new kinds can land without
/// breaking wire clients that match on the string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderKind {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions (api.openai.com).
    Openai,
    /// Any OpenAI-compatible Chat Completions gateway (OpenRouter, vLLM, …).
    OpenaiCompatible,
}

impl ProviderKind {
    /// All kinds the server knows about, in display order.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::Anthropic,
        ProviderKind::Openai,
        ProviderKind::OpenaiCompatible,
    ];

    /// Wire/path form (`anthropic`, `openai`, `openai_compatible`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::OpenaiCompatible => "openai_compatible",
        }
    }

    /// Parse a path segment into a kind.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "openai_compatible" => Some(Self::OpenaiCompatible),
            _ => None,
        }
    }

    /// Store setting key for this kind's non-secret config.
    pub fn setting_key(self) -> String {
        format!("{PROVIDER_SETTING_PREFIX}{}", self.as_str())
    }

    /// SecretProvider key for this kind's credential blob.
    pub fn credential_key(self) -> String {
        format!(
            "{PROVIDER_SETTING_PREFIX}{}{CREDENTIAL_SUFFIX}",
            self.as_str()
        )
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A credential stored in the keychain. Tagged so new auth types (`oauth`, …)
/// are additive.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderCredential {
    /// A bearer / API key.
    ApiKey {
        /// The secret key material.
        key: String,
    },
}

impl ProviderCredential {
    /// Build an API-key credential.
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey { key: key.into() }
    }

    /// The API key, if this is an `api_key` credential.
    pub fn as_api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey { key } => Some(key.as_str()),
        }
    }
}

// Redact key material in Debug.
impl std::fmt::Debug for ProviderCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey { .. } => f.debug_struct("ApiKey").field("key", &"***").finish(),
        }
    }
}

/// Non-secret provider configuration persisted in the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Whether this provider is a routing candidate.
    pub enabled: bool,
    /// Optional base URL override (required for `openai_compatible`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Explicit model ids served by a custom OpenAI-compatible endpoint.
    ///
    /// Curated providers ignore this field and obtain their models from the
    /// host registry. Keeping custom entries beside the endpoint removes the
    /// old ambiguous global free-form model setting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<CustomModelConfig>,
}

impl ProviderConfig {
    /// Default config when nothing is stored: disabled, no base URL.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            base_url: None,
            models: Vec::new(),
        }
    }
}

fn default_custom_context_window() -> u32 {
    32_768
}

fn default_custom_max_output_tokens() -> u32 {
    4_096
}

/// Conservative, user-inspectable capabilities for one model served by an
/// OpenAI-compatible endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomModelConfig {
    /// Exact model id sent to the endpoint.
    pub id: String,
    /// Optional human-facing label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Context limit used by OpenWave's reducer.
    #[serde(default = "default_custom_context_window")]
    pub context_window: u32,
    /// Maximum output sent to the endpoint.
    #[serde(default = "default_custom_max_output_tokens")]
    pub max_output_tokens: u32,
}

/// Owned runtime policy resolved from a provider-qualified selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelPolicy {
    /// Stable provider-qualified selection key.
    pub key: String,
    /// Raw model id sent to the provider.
    pub id: String,
    /// Human-readable label.
    pub display_name: String,
    /// Exact provider route.
    pub provider: ProviderKind,
    /// Runtime context reduction limit.
    pub context_window: u32,
    /// Runtime output cap.
    pub max_output_tokens: u32,
    /// End-to-end supported input modalities.
    pub input_modalities: Vec<InputModality>,
    /// Whether the provider request uses a reasoning-model shape.
    pub supports_reasoning: bool,
    /// Whether the chat may set a reasoning-effort override.
    pub supports_reasoning_effort: bool,
}

impl ResolvedModelPolicy {
    pub(crate) fn curated(spec: &ModelSpec) -> Self {
        Self {
            key: model_registry::selection_key(spec.provider, spec.id),
            id: spec.id.to_owned(),
            display_name: spec.display_name.to_owned(),
            provider: spec.provider,
            context_window: spec.context_window,
            max_output_tokens: spec.max_output_tokens,
            input_modalities: spec.input_modalities.to_vec(),
            supports_reasoning: spec.supports_reasoning,
            supports_reasoning_effort: spec.supports_reasoning_effort,
        }
    }

    fn custom(model: &CustomModelConfig) -> Self {
        Self {
            key: model_registry::selection_key(ProviderKind::OpenaiCompatible, &model.id),
            id: model.id.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model_registry::display_name_for(&model.id)),
            provider: ProviderKind::OpenaiCompatible,
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            input_modalities: vec![InputModality::Text],
            // Unknown endpoints get deliberately conservative request shaping.
            // Capability editing can be added later without changing the key.
            supports_reasoning: false,
            supports_reasoning_effort: false,
        }
    }

    fn legacy_custom(id: &str) -> Self {
        Self::custom(&CustomModelConfig {
            id: id.to_owned(),
            display_name: None,
            context_window: default_custom_context_window(),
            max_output_tokens: default_custom_max_output_tokens(),
        })
    }
}

/// Apply one resolved registry row to the provider-neutral agent config.
///
/// The stored selection key never reaches an adapter: requests carry the raw
/// model id plus the exact provider hint. Unsupported reasoning controls are
/// normalized away before provider dispatch.
pub fn apply_model_policy(
    config: &mut AgentConfig,
    policy: &ResolvedModelPolicy,
    reasoning_effort: Option<ReasoningEffort>,
) -> Result<()> {
    config.provider = Some(ProviderId::new(policy.provider.as_str()));
    config.model = policy.id.clone();
    config.reasoning_model = policy.supports_reasoning;
    config.context_window = usize::try_from(policy.context_window)
        .map_err(|_| openwave_core::AgentError::config("model context window is unsupported"))?;
    config.max_tokens = Some(policy.max_output_tokens);
    config.reasoning_effort = policy
        .supports_reasoning_effort
        .then_some(reasoning_effort)
        .flatten();
    if policy.supports_reasoning {
        config.temperature = None;
    }
    Ok(())
}

/// Public catalog row plus current provider readiness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub policy: ResolvedModelPolicy,
    pub available: bool,
}

/// Public view of a provider — never includes the credential itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderInfo {
    /// Provider kind.
    pub kind: ProviderKind,
    /// Whether the provider is enabled for routing.
    pub enabled: bool,
    /// Configured base URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Whether a credential is stored (never the credential itself).
    pub has_credential: bool,
    /// Explicit custom model entries for this endpoint.
    pub models: Vec<CustomModelConfig>,
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`;
/// `#[serde(default)]` supplies `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// Body of `PUT /providers/{kind}`. Absent fields are left unchanged; an
/// explicit `null` `base_url` clears it; `credential` replaces the stored blob
/// when present (omit to leave the credential alone).
#[derive(Debug, Deserialize)]
pub struct ProviderUpdate {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub base_url: Option<Option<String>>,
    #[serde(default)]
    pub credential: Option<ProviderCredential>,
    /// Replacement custom-model list. Only valid for `openai_compatible`.
    #[serde(default)]
    pub models: Option<Vec<CustomModelConfig>>,
}

/// Read the stored config for `kind`, or the disabled default.
pub async fn read_config(store: &dyn Store, kind: ProviderKind) -> Result<ProviderConfig> {
    Ok(store
        .get_setting(&kind.setting_key())
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_else(ProviderConfig::disabled))
}

/// Persist non-secret config for `kind`.
pub async fn write_config(
    store: &dyn Store,
    kind: ProviderKind,
    config: &ProviderConfig,
) -> Result<()> {
    store
        .set_setting(&kind.setting_key(), &serde_json::to_value(config)?)
        .await
}

/// Read the typed credential for `kind`, if any.
///
/// For Anthropic, falls back to the legacy plain-string
/// `provider.anthropic.api_key` secret so existing installs keep working.
pub async fn read_credential(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> Result<Option<ProviderCredential>> {
    if let Some(raw) = secrets.get_secret(&kind.credential_key()).await? {
        if raw.is_empty() {
            return Ok(None);
        }
        // Prefer the typed blob; a bare string is treated as an api_key for
        // resilience against hand-edited keychain entries.
        if let Ok(cred) = serde_json::from_str::<ProviderCredential>(&raw) {
            return Ok(Some(cred));
        }
        return Ok(Some(ProviderCredential::api_key(raw)));
    }
    if kind == ProviderKind::Anthropic {
        if let Some(key) = secrets.get_secret(LEGACY_ANTHROPIC_API_KEY).await? {
            if !key.is_empty() {
                return Ok(Some(ProviderCredential::api_key(key)));
            }
        }
    }
    Ok(None)
}

/// Store a typed credential for `kind`.
pub async fn write_credential(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    credential: &ProviderCredential,
) -> Result<()> {
    let raw = serde_json::to_string(credential)?;
    secrets.set_secret(&kind.credential_key(), &raw).await
}

/// Delete the credential for `kind` (new key + legacy Anthropic key).
pub async fn delete_credential(secrets: &dyn SecretProvider, kind: ProviderKind) -> Result<()> {
    secrets.delete_secret(&kind.credential_key()).await?;
    if kind == ProviderKind::Anthropic {
        secrets.delete_secret(LEGACY_ANTHROPIC_API_KEY).await?;
    }
    Ok(())
}

/// Whether `kind` has a usable credential — stored, or (for Anthropic/OpenAI)
/// the matching env fallback the resolver also honors.
pub async fn has_credential(secrets: &dyn SecretProvider, kind: ProviderKind) -> bool {
    if matches!(
        read_credential(secrets, kind).await,
        Ok(Some(cred)) if cred.as_api_key().is_some_and(|k| !k.is_empty())
    ) {
        return true;
    }
    match kind {
        ProviderKind::Anthropic => std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()),
        ProviderKind::Openai => std::env::var("OPENAI_API_KEY").is_ok_and(|k| !k.is_empty()),
        ProviderKind::OpenaiCompatible => false,
    }
}

/// Build the public [`ProviderInfo`] for every known kind.
pub async fn list_providers(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<Vec<ProviderInfo>> {
    let mut out = Vec::with_capacity(ProviderKind::ALL.len());
    for &kind in ProviderKind::ALL {
        let config = read_config(store, kind).await?;
        out.push(ProviderInfo {
            kind,
            enabled: config.enabled,
            base_url: config.base_url.clone(),
            has_credential: has_credential(secrets, kind).await,
            models: config.models,
        });
    }
    Ok(out)
}

/// Apply a [`ProviderUpdate`] and return the resulting [`ProviderInfo`].
pub async fn update_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    update: ProviderUpdate,
) -> std::result::Result<ProviderInfo, ServerError> {
    let mut config = read_config(store, kind).await?;

    if let Some(enabled) = update.enabled {
        config.enabled = enabled;
    }
    match update.base_url {
        None => {}
        Some(None) => config.base_url = None,
        Some(Some(url)) => {
            if url.is_empty() {
                return Err(ServerError::bad_request("base_url must not be empty"));
            }
            config.base_url = Some(url);
        }
    }
    if let Some(models) = update.models {
        if kind != ProviderKind::OpenaiCompatible {
            return Err(ServerError::bad_request(
                "custom models are only supported by openai_compatible",
            ));
        }
        validate_custom_models(&models)?;
        config.models = models;
    }

    // openai_compatible needs a base URL to be useful when enabled.
    if kind == ProviderKind::OpenaiCompatible && config.enabled && config.base_url.is_none() {
        return Err(ServerError::bad_request(
            "openai_compatible requires a base_url when enabled",
        ));
    }

    if let Some(credential) = update.credential {
        match &credential {
            ProviderCredential::ApiKey { key } if key.is_empty() => {
                return Err(ServerError::bad_request("credential key must not be empty"));
            }
            _ => {}
        }
        write_credential(secrets, kind, &credential).await?;
    }

    write_config(store, kind, &config).await?;

    Ok(ProviderInfo {
        kind,
        enabled: config.enabled,
        base_url: config.base_url.clone(),
        has_credential: has_credential(secrets, kind).await,
        models: config.models,
    })
}

fn validate_custom_models(models: &[CustomModelConfig]) -> std::result::Result<(), ServerError> {
    validate_custom_models_against(models, |id| {
        model_registry::find_for(ProviderKind::OpenaiCompatible, id).is_some()
    })
}

fn validate_custom_models_against(
    models: &[CustomModelConfig],
    is_curated: impl Fn(&str) -> bool,
) -> std::result::Result<(), ServerError> {
    const MAX_CUSTOM_MODELS: usize = 64;
    const MAX_MODEL_ID_CHARS: usize = 240;
    const MAX_DISPLAY_NAME_CHARS: usize = 120;
    const MAX_CONTEXT_WINDOW: u32 = 4_000_000;

    if models.len() > MAX_CUSTOM_MODELS {
        return Err(ServerError::bad_request(format!(
            "openai_compatible supports at most {MAX_CUSTOM_MODELS} custom models"
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for model in models {
        let id = model.id.trim();
        if id.is_empty()
            || id.chars().count() > MAX_MODEL_ID_CHARS
            || id.chars().any(char::is_whitespace)
            || id.chars().any(char::is_control)
        {
            return Err(ServerError::bad_request(
                "custom model id must be non-empty, bounded, and contain no whitespace or control characters",
            ));
        }
        if id != model.id {
            return Err(ServerError::bad_request(
                "custom model id must not have leading or trailing whitespace",
            ));
        }
        if !ids.insert(id) {
            return Err(ServerError::bad_request(format!(
                "duplicate custom model id `{id}`"
            )));
        }
        if is_curated(id) {
            return Err(ServerError::bad_request(format!(
                "custom model id `{id}` conflicts with a curated openai_compatible model"
            )));
        }
        if model.display_name.as_ref().is_some_and(|name| {
            name.trim().is_empty()
                || name.trim() != name
                || name.chars().count() > MAX_DISPLAY_NAME_CHARS
                || name.chars().any(char::is_control)
        }) {
            return Err(ServerError::bad_request(
                "custom model display_name must be non-empty, bounded, and contain no control characters",
            ));
        }
        if !(1_024..=MAX_CONTEXT_WINDOW).contains(&model.context_window) {
            return Err(ServerError::bad_request(format!(
                "custom model `{id}` context_window must be between 1024 and {MAX_CONTEXT_WINDOW}"
            )));
        }
        if model.max_output_tokens == 0 || model.max_output_tokens > model.context_window {
            return Err(ServerError::bad_request(format!(
                "custom model `{id}` max_output_tokens must be positive and not exceed context_window"
            )));
        }
    }
    Ok(())
}

/// Resolve an API-key credential for `kind`: stored blob (or Anthropic legacy),
/// then the matching env fallback.
pub async fn resolve_api_key(secrets: &dyn SecretProvider, kind: ProviderKind) -> Option<String> {
    if let Ok(Some(cred)) = read_credential(secrets, kind).await {
        if let Some(key) = cred.as_api_key() {
            if !key.is_empty() {
                return Some(key.to_string());
            }
        }
    }
    match kind {
        ProviderKind::Anthropic => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Openai => std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::OpenaiCompatible => None,
    }
}

/// Resolve the Anthropic API key (stored / legacy / env). Thin wrapper kept for
/// call sites that are Anthropic-specific.
pub async fn resolve_anthropic_api_key(secrets: &dyn SecretProvider) -> Option<String> {
    resolve_api_key(secrets, ProviderKind::Anthropic).await
}

/// Map a server [`ProviderKind`] to the router's [`openwave_router::RouteKind`].
pub fn route_kind(kind: ProviderKind) -> openwave_router::RouteKind {
    match kind {
        ProviderKind::Anthropic => openwave_router::RouteKind::Anthropic,
        ProviderKind::Openai => openwave_router::RouteKind::Openai,
        ProviderKind::OpenaiCompatible => openwave_router::RouteKind::OpenaiCompatible,
    }
}

/// Collect enabled, credentialed routes for the composite router.
///
/// A kind with no usable API key is skipped. `openai_compatible` also requires
/// a `base_url`. Store-read failures for a single kind skip that kind (fail
/// closed for it) rather than aborting the whole list.
pub async fn collect_routes(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Vec<openwave_router::Route> {
    let mut routes = Vec::new();
    for &kind in ProviderKind::ALL {
        let config = match read_config(store, kind).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !config.enabled {
            continue;
        }
        let Some(api_key) = resolve_api_key(secrets, kind).await else {
            continue;
        };
        if kind == ProviderKind::OpenaiCompatible && config.base_url.is_none() {
            continue;
        }
        if kind == ProviderKind::OpenaiCompatible {
            let base = config.base_url.as_deref().unwrap_or("");
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                continue;
            }
        }
        routes.push(openwave_router::Route {
            kind: route_kind(kind),
            api_key,
            base_url: config.base_url,
            curated_models: model_registry::models_for(kind)
                .map(|spec| spec.id.to_string())
                .chain(config.models.into_iter().map(|model| model.id))
                .collect(),
        });
    }
    routes
}

/// One-shot boot migration: if Anthropic has no stored config yet but a key is
/// available (legacy secret or `ANTHROPIC_API_KEY` env), enable it.
///
/// Preserves pre-providers behavior where an env key alone was enough to run
/// turns. An explicit `enabled: false` written later wins; this only fills in
/// a missing row.
pub async fn migrate_legacy_anthropic(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<()> {
    if store
        .get_setting(&ProviderKind::Anthropic.setting_key())
        .await?
        .is_some()
    {
        return Ok(());
    }
    if resolve_anthropic_api_key(secrets).await.is_some() {
        write_config(
            store,
            ProviderKind::Anthropic,
            &ProviderConfig {
                enabled: true,
                base_url: None,
                models: Vec::new(),
            },
        )
        .await?;
    }
    Ok(())
}

/// Resolve a stored or wire selection into an authoritative runtime policy.
///
/// Bare curated ids are migrated losslessly to their owning provider. Bare
/// custom ids resolve only when explicitly configured, except when
/// `allow_legacy_custom` is true: that narrow path preserves pre-registry rows
/// which historically routed unknown ids to OpenAI-compatible.
pub async fn resolve_model_policy(
    store: &dyn Store,
    value: &str,
    allow_legacy_custom: bool,
) -> Result<Option<ResolvedModelPolicy>> {
    if let Some((provider, id)) = model_registry::parse_selection_key(value) {
        if let Some(spec) = model_registry::find_for(provider, id) {
            return Ok(Some(ResolvedModelPolicy::curated(spec)));
        }
        if provider != ProviderKind::OpenaiCompatible {
            return Ok(None);
        }
        let config = read_config(store, provider).await?;
        return Ok(config
            .models
            .iter()
            .find(|model| model.id == id)
            .map(ResolvedModelPolicy::custom));
    }

    let config = read_config(store, ProviderKind::OpenaiCompatible).await?;
    let mut owners = model_registry::models_named(value)
        .map(ResolvedModelPolicy::curated)
        .collect::<Vec<_>>();
    owners.extend(
        config
            .models
            .iter()
            .filter(|model| model.id == value)
            .map(ResolvedModelPolicy::custom),
    );
    match owners.len() {
        1 => return Ok(owners.pop()),
        count if count > 1 => return Ok(None),
        _ => {}
    }
    Ok(allow_legacy_custom.then(|| ResolvedModelPolicy::legacy_custom(value)))
}

/// Whether the provider can accept a new turn right now.
pub async fn provider_is_usable(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> Result<bool> {
    let config = read_config(store, kind).await?;
    if !config.enabled || !has_credential(secrets, kind).await {
        return Ok(false);
    }
    if kind == ProviderKind::OpenaiCompatible {
        let Some(base) = config.base_url.as_deref() else {
            return Ok(false);
        };
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Full typed catalog. Unavailable rows remain visible for provider-scoped
/// settings, but clients must not offer them as usable selections.
pub async fn catalog_models(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<Vec<CatalogModel>> {
    let mut models = Vec::new();
    for &kind in ProviderKind::ALL {
        let config = read_config(store, kind).await?;
        let available = provider_is_usable(store, secrets, kind).await?;
        models.extend(model_registry::models_for(kind).map(|spec| CatalogModel {
            policy: ResolvedModelPolicy::curated(spec),
            available,
        }));
        if kind == ProviderKind::OpenaiCompatible {
            models.extend(config.models.iter().map(|model| CatalogModel {
                policy: ResolvedModelPolicy::custom(model),
                available,
            }));
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_roundtrips_and_redacts_debug() {
        let cred = ProviderCredential::api_key("sk-secret");
        let json = serde_json::to_string(&cred).unwrap();
        assert!(json.contains("api_key"));
        assert!(json.contains("sk-secret"));
        let back: ProviderCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_api_key(), Some("sk-secret"));
        assert!(!format!("{cred:?}").contains("sk-secret"));
    }

    #[test]
    fn kind_parse_and_keys() {
        assert_eq!(
            ProviderKind::parse("openai_compatible"),
            Some(ProviderKind::OpenaiCompatible)
        );
        assert_eq!(
            ProviderKind::Anthropic.credential_key(),
            "provider.anthropic.credential"
        );
        assert_eq!(ProviderKind::Openai.setting_key(), "provider.openai");
    }

    /// An unset display name is represented by the key being absent, in both
    /// directions. The desktop used to send an explicit `null` while declaring
    /// the field non-optional, so its type claimed a key the server never sends.
    ///
    /// `deny_unknown_fields` makes the inbound half worth asserting rather than
    /// assuming: the body has to be accepted with the key missing entirely.
    #[test]
    fn an_unset_display_name_is_absent_rather_than_null() {
        let unset = CustomModelConfig {
            id: "local/model".into(),
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
        };
        let json = serde_json::to_value(&unset).expect("a model config serializes");
        assert!(
            json.get("display_name").is_none(),
            "the server should omit an unset display name, not send null: {json}"
        );

        // What the desktop now sends: no key at all.
        let parsed: CustomModelConfig = serde_json::from_str(
            r#"{"id":"local/model","context_window":32768,"max_output_tokens":4096}"#,
        )
        .expect("an absent display name is accepted");
        assert_eq!(parsed, unset);

        // Still accepted, so an older client is not broken by the change.
        let explicit_null: CustomModelConfig = serde_json::from_str(
            r#"{"id":"local/model","display_name":null,"context_window":32768,"max_output_tokens":4096}"#,
        )
        .expect("an explicit null is still accepted");
        assert_eq!(explicit_null, unset);
    }

    #[test]
    fn custom_model_validation_is_conservative_and_rejects_duplicates() {
        let valid = CustomModelConfig {
            id: "local/model".into(),
            display_name: Some("Local Model".into()),
            context_window: 32_768,
            max_output_tokens: 4_096,
        };
        assert!(validate_custom_models(std::slice::from_ref(&valid)).is_ok());
        assert!(validate_custom_models(&[valid.clone(), valid.clone()]).is_err());
        assert!(
            validate_custom_models_against(std::slice::from_ref(&valid), |id| id == "local/model")
                .is_err(),
            "custom ids must not shadow a curated id under the same provider"
        );
        assert!(validate_custom_models(&[CustomModelConfig {
            id: "bad model".into(),
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
        }])
        .is_err());
        assert!(validate_custom_models(&[CustomModelConfig {
            id: "bad".into(),
            display_name: None,
            context_window: 1_000,
            max_output_tokens: 4_096,
        }])
        .is_err());
    }

    #[test]
    fn registry_policy_controls_context_output_provider_and_reasoning() {
        let mut config = AgentConfig {
            temperature: Some(0.7),
            reasoning_effort: Some(ReasoningEffort::High),
            ..AgentConfig::default()
        };
        let opus = ResolvedModelPolicy::curated(
            model_registry::find_for(ProviderKind::Anthropic, "claude-opus-4-8").unwrap(),
        );
        apply_model_policy(&mut config, &opus, Some(ReasoningEffort::High)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("anthropic")));
        assert_eq!(config.model, "claude-opus-4-8");
        assert_eq!(config.context_window, 1_000_000);
        assert_eq!(config.max_tokens, Some(128_000));
        assert_eq!(config.reasoning_effort, None);
        assert_eq!(config.temperature, None);

        let mut config = AgentConfig::default();
        let gpt = ResolvedModelPolicy::curated(
            model_registry::find_for(ProviderKind::Openai, "gpt-5.6-sol").unwrap(),
        );
        apply_model_policy(&mut config, &gpt, Some(ReasoningEffort::Low)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("openai")));
        assert_eq!(config.context_window, 1_050_000);
        assert_eq!(config.max_tokens, Some(128_000));
        assert!(config.reasoning_model);
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Low));

        // A custom endpoint keeps the conservative shape: no reasoning, and the
        // requested effort is dropped rather than sent to something that would
        // reject it.
        let mut config = AgentConfig::default();
        let custom = ResolvedModelPolicy::custom(&CustomModelConfig {
            id: "local-model".into(),
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
        });
        apply_model_policy(&mut config, &custom, Some(ReasoningEffort::High)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("openai_compatible")));
        assert_eq!(config.context_window, 32_768);
        assert_eq!(config.max_tokens, Some(4_096));
        assert!(!config.reasoning_model);
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn every_curated_model_applies_its_exact_runtime_contract() {
        for &provider in ProviderKind::ALL {
            for spec in model_registry::models_for(provider) {
                let mut config = AgentConfig {
                    temperature: Some(0.7),
                    reasoning_effort: Some(ReasoningEffort::High),
                    ..AgentConfig::default()
                };
                let policy = ResolvedModelPolicy::curated(spec);
                apply_model_policy(&mut config, &policy, Some(ReasoningEffort::Low)).unwrap();

                assert_eq!(config.provider, Some(ProviderId::new(provider.as_str())));
                assert_eq!(config.model, spec.id);
                assert_eq!(
                    config.context_window,
                    usize::try_from(spec.context_window).unwrap()
                );
                assert_eq!(config.max_tokens, Some(spec.max_output_tokens));
                assert_eq!(config.reasoning_model, spec.supports_reasoning);
                assert_eq!(
                    config.reasoning_effort,
                    spec.supports_reasoning_effort
                        .then_some(ReasoningEffort::Low)
                );
                assert_eq!(
                    config.temperature,
                    (!spec.supports_reasoning).then_some(0.7)
                );
            }
        }
    }
}
