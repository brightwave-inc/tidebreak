//! Configurable model providers.
//!
//! A provider is `{ kind, enabled, base_url? }` plus a credential held in the
//! [`SecretProvider`](openwave_core::SecretProvider) (never the DB). Credentials
//! are a typed JSON blob (`api_key`, `oauth`, or `service_account`) so new auth
//! shapes don't force a schema redesign.
//!
//! Non-secret config lives in the [`Store`](openwave_core::Store) under
//! `provider.<kind>`. The legacy `provider.anthropic.api_key` secret and the
//! `PUT /settings/api-key` shim still work — they read/write the anthropic
//! credential in the new blob shape.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use openwave_core::{AgentConfig, ProviderId, ReasoningEffort, Result, SecretProvider, Store};

use crate::error::ServerError;
use crate::model_registry::{self, InputModality, ModelSpec};

/// Setting-key prefix for per-provider non-secret config (`provider.<kind>`).
const PROVIDER_SETTING_PREFIX: &str = "provider.";

/// Secret-key suffix for the typed credential blob (`provider.<kind>.credential`).
const CREDENTIAL_SUFFIX: &str = ".credential";

/// Legacy Anthropic API-key secret (pre-providers). Still read as a fallback.
pub const LEGACY_ANTHROPIC_API_KEY: &str = "provider.anthropic.api_key";

/// Serializes every writer of the ModelGateway provider row.
///
/// Pairing, model sync, sign-out, and `PUT /providers/model_gateway` each
/// read `provider.model_gateway`, mutate part of it, and write the whole row
/// back, and the [`Store`] API has no cross-call transaction. Unserialized,
/// two read-modify-writes can interleave and resurrect fields the other just
/// replaced — a model sync racing a pairing could stamp the old gateway's
/// base URL or stale model list over the row the pairing wrote while policy
/// already points at the new gateway. Process-local for the same reason as
/// pairing's own mutex: the server's instance lock guarantees one process
/// owns the store, and a static cannot be accidentally wired into two
/// instances that no longer exclude each other. Lock order: acquired after
/// the pairing mutex and after the gateway runtime's sign-in state lock,
/// never before either, and never held across a network call.
pub(crate) static GATEWAY_ROW_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The known provider kinds. `#[non_exhaustive]` so new kinds can land without
/// breaking wire clients that match on the string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderKind {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions (api.openai.com).
    Openai,
    /// Google Gemini Developer API (generativelanguage.googleapis.com).
    Gemini,
    /// Any OpenAI-compatible Chat Completions gateway (OpenRouter, vLLM, …).
    OpenaiCompatible,
    /// A signed-in model-gateway deployment: entitled models synced from the
    /// gateway, inference through its Anthropic-compatible surface with
    /// short-lived OAuth tokens.
    ModelGateway,
}

impl ProviderKind {
    /// All kinds the server knows about, in display order.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::Anthropic,
        ProviderKind::Openai,
        ProviderKind::Gemini,
        ProviderKind::OpenaiCompatible,
        ProviderKind::ModelGateway,
    ];

    /// Wire/path form (`anthropic`, `openai`, `openai_compatible`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::OpenaiCompatible => "openai_compatible",
            ProviderKind::ModelGateway => "model_gateway",
        }
    }

    /// Parse a path segment into a kind.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "gemini" => Some(Self::Gemini),
            "openai_compatible" => Some(Self::OpenaiCompatible),
            "model_gateway" => Some(Self::ModelGateway),
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

    /// Whether this provider can use `credential` today.
    fn accepts_credential(self, credential: &ProviderCredential) -> bool {
        matches!(credential, ProviderCredential::ApiKey { .. })
            || (self == ProviderKind::Gemini
                && matches!(credential, ProviderCredential::ServiceAccount { .. }))
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A credential stored in the keychain. Tagged so new auth types (`oauth`, …)
/// are additive.
///
/// Every field in every variant is secret material. This type intentionally
/// does not implement `Display`, and its `Debug` implementation redacts each
/// payload before it can reach a log or error message.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderCredential {
    /// A bearer / API key.
    ApiKey {
        /// The secret key material.
        key: String,
    },
    /// An OAuth-backed provider. A marker, not the tokens: the token set lives
    /// in its own keychain entry managed by `openwave-connectors`, so the
    /// rotating material never rides the provider settings surface.
    Oauth {},
    /// A service-account key file, kept verbatim for the future token exchange.
    ServiceAccount {
        /// The raw JSON key-file contents.
        json: String,
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
            Self::Oauth {} => None,
            Self::ServiceAccount { .. } => None,
        }
    }

    /// The raw service-account key file, if this credential carries one.
    fn as_service_account(&self) -> Option<&str> {
        match self {
            Self::ServiceAccount { json } => Some(json),
            Self::ApiKey { .. } | Self::Oauth {} => None,
        }
    }

    /// Validate a credential before it is written to the keychain.
    ///
    /// Error messages name only malformed fields, never their values: a
    /// service-account file contains its private key.
    pub fn validate(&self) -> std::result::Result<(), ServerError> {
        match self {
            Self::ApiKey { key } if key.is_empty() => {
                Err(ServerError::bad_request("credential key must not be empty"))
            }
            Self::ApiKey { .. } | Self::Oauth {} => Ok(()),
            Self::ServiceAccount { json } => validate_service_account(json),
        }
    }
}

// Redact key material in Debug.
impl std::fmt::Debug for ProviderCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey { .. } => f.debug_struct("ApiKey").field("key", &"***").finish(),
            Self::Oauth {} => f.debug_struct("Oauth").finish(),
            Self::ServiceAccount { .. } => f
                .debug_struct("ServiceAccount")
                .field("json", &"***")
                .finish(),
        }
    }
}

/// The required, non-secret shape of a service-account key file.
///
/// This is only a validation view. The original JSON is what is stored, so a
/// future token exchange can use fields introduced after this version.
#[derive(Deserialize)]
struct ServiceAccountKey {
    #[serde(rename = "type")]
    key_type: String,
    client_email: String,
    private_key: String,
    project_id: String,
}

fn validate_service_account(json: &str) -> std::result::Result<(), ServerError> {
    let key: ServiceAccountKey = serde_json::from_str(json).map_err(|_| {
        // serde's parse error can quote the input, which includes the private key.
        ServerError::bad_request(
            "service account key must be JSON with `type`, `client_email`, `private_key`, and `project_id`",
        )
    })?;
    if key.key_type != "service_account" {
        return Err(ServerError::bad_request(
            "service account key `type` must be `service_account`",
        ));
    }
    for (field, value) in [
        ("client_email", &key.client_email),
        ("private_key", &key.private_key),
        ("project_id", &key.project_id),
    ] {
        if value.trim().is_empty() {
            return Err(ServerError::bad_request(format!(
                "service account key `{field}` must not be empty"
            )));
        }
    }
    Ok(())
}

/// Non-secret provider configuration persisted in the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Whether this provider is a routing candidate.
    pub enabled: bool,
    /// Optional base URL override (required for `openai_compatible`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Vertex AI location used only when Gemini has a service-account
    /// credential. An absent value defaults to `global`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_location: Option<String>,
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
            vertex_location: None,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
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
    /// The reasoning-effort levels this model accepts, ascending. Empty when
    /// the model takes no effort control.
    pub reasoning_efforts: Vec<ReasoningEffort>,
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
            reasoning_efforts: spec.reasoning_efforts.to_vec(),
        }
    }

    fn custom_for(provider: ProviderKind, model: &CustomModelConfig) -> Self {
        Self {
            key: model_registry::selection_key(provider, &model.id),
            id: model.id.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model_registry::display_name_for(&model.id)),
            provider,
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            input_modalities: vec![InputModality::Text],
            // Unknown endpoints get deliberately conservative request shaping.
            // Capability editing can be added later without changing the key.
            supports_reasoning: false,
            reasoning_efforts: Vec::new(),
        }
    }

    fn legacy_custom(id: &str) -> Self {
        Self::custom_for(
            ProviderKind::OpenaiCompatible,
            &CustomModelConfig {
                id: id.to_owned(),
                display_name: None,
                context_window: default_custom_context_window(),
                max_output_tokens: default_custom_max_output_tokens(),
            },
        )
    }
}

/// Apply one resolved registry row to the provider-neutral agent config.
///
/// The stored selection key never reaches an adapter: requests carry the raw
/// model id plus the exact provider hint. This is the one place a chat's stored
/// reasoning effort is reconciled with the model actually about to run, so a
/// level the model does not accept can never reach an adapter: it degrades to
/// the closest level the model does take, or is dropped when the model exposes
/// no effort control at all.
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
    config.reasoning_effort =
        reasoning_effort.and_then(|effort| effort.clamp_to(&policy.reasoning_efforts));
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub struct ProviderInfo {
    /// Provider kind.
    pub kind: ProviderKind,
    /// Whether the provider is enabled for routing.
    pub enabled: bool,
    /// Configured base URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
    /// Vertex AI location. Never includes the project id from the credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vertex_location: Option<String>,
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
/// explicit `null` clears a nullable config field; `credential` replaces the
/// stored blob when present (omit to leave the credential alone).
#[derive(Debug, Deserialize)]
pub struct ProviderUpdate {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    pub base_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub vertex_location: Option<Option<String>>,
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

/// Store a validated typed credential for `kind`.
pub async fn write_credential(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    credential: &ProviderCredential,
) -> std::result::Result<(), ServerError> {
    credential.validate()?;
    let raw = serde_json::to_string(credential)
        .map_err(|_| ServerError::internal("failed to serialize provider credential"))?;
    secrets
        .set_secret(&kind.credential_key(), &raw)
        .await
        .map_err(ServerError::from)
}

/// Delete the credential for `kind` (new key + legacy Anthropic key).
pub async fn delete_credential(secrets: &dyn SecretProvider, kind: ProviderKind) -> Result<()> {
    secrets.delete_secret(&kind.credential_key()).await?;
    if kind == ProviderKind::Anthropic {
        secrets.delete_secret(LEGACY_ANTHROPIC_API_KEY).await?;
    }
    Ok(())
}

/// Whether `kind` has a usable credential — stored, or (for direct API-key providers)
/// the matching env fallback the resolver also honors.
pub async fn has_credential(secrets: &dyn SecretProvider, kind: ProviderKind) -> bool {
    match read_credential(secrets, kind).await {
        Ok(Some(credential)) => {
            return credential.as_api_key().is_some_and(|key| !key.is_empty())
                || (kind == ProviderKind::Gemini
                    && credential.as_service_account().is_some_and(|json| {
                        openwave_router::GoogleServiceAccount::from_json(json).is_ok()
                    }));
        }
        Err(_) => return false,
        Ok(None) => {}
    }
    if kind == ProviderKind::ModelGateway {
        // The gateway's credential is its stored OAuth session, not a key.
        return openwave_connectors::has_stored_credentials(secrets).await;
    }
    env_api_key(kind).is_some()
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
            vertex_location: config.vertex_location.clone(),
            has_credential: has_credential(secrets, kind).await,
            models: config.models,
        });
    }
    Ok(out)
}

/// The refusal every managed-lockdown write path answers with. One stable
/// `kind` so clients can branch on it wherever the lock surfaces.
pub(crate) fn managed_profile_refusal(message: impl Into<String>) -> ServerError {
    ServerError::conflict_kind("managed_profile", message)
}

/// Apply a [`ProviderUpdate`] and return the resulting [`ProviderInfo`].
///
/// On a managed profile, BYOK providers are locked: credential and base-URL
/// writes for any non-gateway kind are refused, and the gateway's own base
/// URL only accepts a value that normalizes to the policy's locked gateway
/// URL (which keeps the stored row in sync — routing itself reads the policy
/// URL directly, see [`collect_routes`]).
pub async fn update_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    mut update: ProviderUpdate,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
) -> std::result::Result<ProviderInfo, ServerError> {
    // The gateway row is also written by pairing, model sync, and sign-out;
    // serialize this read-modify-write with them. The policy is resolved
    // under the same lock so a pairing that lands first is seen as managed
    // here rather than bypassed with a stale resolution. Other kinds have no
    // concurrent writers and take no lock.
    let row_lock = match kind {
        ProviderKind::ModelGateway => Some(GATEWAY_ROW_WRITES.lock().await),
        _ => None,
    };
    let policy = crate::managed_policy::resolve(store, os_policy).await?;
    if policy.managed {
        if kind != ProviderKind::ModelGateway
            && (update.credential.is_some() || update.base_url.is_some())
        {
            return Err(managed_profile_refusal(format!(
                "this profile is managed by a model gateway; {kind} credentials and endpoints are locked"
            )));
        }
        if kind == ProviderKind::ModelGateway {
            if let Some(base_url) = update.base_url.take() {
                // Structurally fail closed: a managed policy without a URL
                // (which resolution today never produces) locks the field
                // outright rather than comparing against an empty string.
                let Some(locked) = policy.gateway_url.as_deref() else {
                    return Err(managed_profile_refusal(
                        "this profile is managed; the model gateway base URL is locked",
                    ));
                };
                let matches_locked = base_url.as_deref().is_some_and(|url| {
                    crate::managed_policy::validated_gateway_url(url)
                        .ok()
                        .as_deref()
                        == Some(locked)
                });
                if !matches_locked {
                    return Err(managed_profile_refusal(format!(
                        "this profile is managed; the model gateway base URL is locked to {locked}"
                    )));
                }
                // Store the normalized contract form, not the raw input: the
                // guard compared normalized, and downstream scheme checks
                // must see the same shape (`HTTPS://…` would otherwise pass
                // here and fail them).
                update.base_url = Some(Some(locked.to_owned()));
            }
        }
    }

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
            config.base_url = Some(if kind == ProviderKind::ModelGateway {
                // Store the normalized contract form on every profile, not
                // only managed ones: pairing and policy store the normalized
                // shape, and both the sync recheck and the connection cache
                // compare exact strings — a verbatim `https://gw` beside a
                // normalized `https://gw/` would refuse a same-deployment
                // sync and split the connection cache for one gateway.
                crate::managed_policy::validated_gateway_url(&url)
                    .map_err(|error| ServerError::bad_request(error.to_string()))?
            } else {
                url
            });
        }
    }
    match update.vertex_location {
        None => {}
        Some(_) if kind != ProviderKind::Gemini => {
            return Err(ServerError::bad_request(
                "vertex_location is only supported by gemini",
            ));
        }
        Some(None) => config.vertex_location = None,
        Some(Some(location)) => {
            if location.trim() != location || !openwave_router::valid_vertex_location(&location) {
                return Err(ServerError::bad_request(
                    "vertex_location must be a valid Google Cloud location",
                ));
            }
            config.vertex_location = Some(location);
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
        // The gateway's only credential is its OAuth session, managed by the
        // sign-in flow; accepting a pasted key here would light up
        // has_credential with nothing the router can use.
        if kind == ProviderKind::ModelGateway
            && matches!(credential, ProviderCredential::ApiKey { .. })
        {
            return Err(ServerError::bad_request(
                "model_gateway signs in with OAuth; api keys are not accepted",
            ));
        }
        if let Some(json) = credential.as_service_account() {
            if !kind.accepts_credential(&credential) {
                return Err(ServerError::bad_request(format!(
                    "{kind} does not support service_account credentials"
                )));
            }
            openwave_router::GoogleServiceAccount::from_json(json).map_err(|_| {
                ServerError::bad_request("invalid Google service-account credential")
            })?;
        }
        write_credential(secrets, kind, &credential).await?;
    }

    write_config(store, kind, &config).await?;
    // The response's has_credential is keychain I/O; the row lock exists to
    // serialize the row write, so drop it before building the response.
    drop(row_lock);

    Ok(ProviderInfo {
        kind,
        enabled: config.enabled,
        base_url: config.base_url.clone(),
        vertex_location: config.vertex_location.clone(),
        has_credential: has_credential(secrets, kind).await,
        models: config.models,
    })
}

pub(crate) fn validate_custom_models(
    models: &[CustomModelConfig],
) -> std::result::Result<(), ServerError> {
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
    match read_credential(secrets, kind).await {
        Ok(Some(credential)) => {
            return credential
                .as_api_key()
                .filter(|key| !key.is_empty())
                .map(str::to_owned);
        }
        Err(_) => return None,
        Ok(None) => {}
    }
    env_api_key(kind)
}

fn env_api_key(kind: ProviderKind) -> Option<String> {
    match kind {
        ProviderKind::Anthropic => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Openai => std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Gemini => std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::OpenaiCompatible => None,
        // Gateway tokens rotate; they are supplied per request by the route's
        // token source, never resolved into a static key.
        ProviderKind::ModelGateway => None,
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
        ProviderKind::Gemini => openwave_router::RouteKind::Gemini,
        ProviderKind::OpenaiCompatible => openwave_router::RouteKind::OpenaiCompatible,
        ProviderKind::ModelGateway => openwave_router::RouteKind::ModelGateway,
    }
}

/// Collect enabled, credentialed routes for the composite router.
///
/// A kind with no usable credential is skipped. Gemini service accounts become
/// Vertex routes; API keys remain Developer API routes. Store-read failures for
/// a single kind skip that kind (fail closed for it) rather than aborting the
/// whole list.
///
/// On a managed profile only the gateway route is offered: BYOK kinds are
/// skipped before any credential is read, so stored keys and the env-var
/// fallbacks are inert without being deleted, and the gateway's bearer target
/// comes from the policy's locked URL — stored config can never redirect it.
pub async fn collect_routes(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    gateway_tokens: Option<std::sync::Arc<dyn openwave_router::BearerTokenSource>>,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Vec<openwave_router::Route> {
    let mut routes = Vec::new();
    for &kind in ProviderKind::ALL {
        if policy.managed && kind != ProviderKind::ModelGateway {
            continue;
        }
        let config = match read_config(store, kind).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        // A managed profile's gateway presence comes from policy, not the
        // stored row: a pure-MDM profile may have no row at all (disabled
        // default), and the row must not be able to turn the gateway off.
        if !config.enabled && !policy.managed {
            continue;
        }
        if kind == ProviderKind::ModelGateway {
            // The gateway route rides its live token source; without a signed-in
            // session there is nothing to route to.
            let Some(source) = gateway_tokens.clone() else {
                continue;
            };
            let Some(base) = (if policy.managed {
                policy.gateway_url.as_deref()
            } else {
                config.base_url.as_deref()
            }) else {
                continue;
            };
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                continue;
            }
            routes.push(openwave_router::Route {
                kind: route_kind(kind),
                api_key: String::new(),
                base_url: Some(format!("{}/compat/anthropic", base.trim_end_matches('/'))),
                curated_models: config.models.into_iter().map(|model| model.id).collect(),
                token_source: Some(source),
                vertex: None,
            });
            continue;
        }

        let stored_credential = match read_credential(secrets, kind).await {
            Ok(credential) => credential,
            Err(_) => continue,
        };
        if kind == ProviderKind::Gemini {
            if let Some(ProviderCredential::ServiceAccount { json }) = stored_credential.as_ref() {
                let Ok(account) = openwave_router::GoogleServiceAccount::from_json(json) else {
                    continue;
                };
                let location = config.vertex_location.as_deref().unwrap_or("global");
                if !openwave_router::valid_vertex_location(location) {
                    continue;
                }
                let project_id = account.project_id().to_owned();
                let credential_fingerprint: [u8; 32] = Sha256::digest(json.as_bytes()).into();
                let source = std::sync::Arc::new(
                    openwave_router::GoogleServiceAccountTokenSource::new(account),
                );
                routes.push(openwave_router::Route {
                    kind: route_kind(kind),
                    api_key: String::new(),
                    // Production Vertex hosts are derived by the adapter.
                    // Never let stored config redirect a bearer token.
                    base_url: None,
                    curated_models: model_registry::models_for(kind)
                        .map(|spec| spec.id.to_string())
                        .collect(),
                    token_source: Some(source),
                    vertex: Some(openwave_router::VertexRoute::new(
                        project_id,
                        location,
                        credential_fingerprint,
                    )),
                });
                continue;
            }
        }
        let api_key = match stored_credential {
            Some(ProviderCredential::ApiKey { key }) if !key.is_empty() => Some(key),
            Some(_) => None,
            None => env_api_key(kind),
        };
        let Some(api_key) = api_key else {
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
            // Gemini's curated Developer API endpoint is fixed in production.
            base_url: (kind != ProviderKind::Gemini)
                .then_some(config.base_url)
                .flatten(),
            curated_models: model_registry::models_for(kind)
                .map(|spec| spec.id.to_string())
                .chain(config.models.into_iter().map(|model| model.id))
                .collect(),
            token_source: None,
            vertex: None,
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
                vertex_location: None,
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
        if !matches!(
            provider,
            ProviderKind::OpenaiCompatible | ProviderKind::ModelGateway
        ) {
            return Ok(None);
        }
        let config = read_config(store, provider).await?;
        return Ok(config
            .models
            .iter()
            .find(|model| model.id == id)
            .map(|model| ResolvedModelPolicy::custom_for(provider, model)));
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
            .map(|model| ResolvedModelPolicy::custom_for(ProviderKind::OpenaiCompatible, model)),
    );
    match owners.len() {
        1 => return Ok(owners.pop()),
        count if count > 1 => return Ok(None),
        _ => {}
    }
    Ok(allow_legacy_custom.then(|| ResolvedModelPolicy::legacy_custom(value)))
}

/// Whether the provider can accept a new turn right now.
///
/// On a managed profile the gateway's presence is derived from policy — the
/// stored row is display/session-cache only and may not exist at all — so it
/// is usable exactly when a session is stored; BYOK kinds are never usable.
pub async fn provider_is_usable(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<bool> {
    if policy.managed {
        if kind != ProviderKind::ModelGateway {
            return Ok(false);
        }
        return Ok(has_credential(secrets, kind).await);
    }
    let config = read_config(store, kind).await?;
    if !config.enabled || !has_credential(secrets, kind).await {
        return Ok(false);
    }
    if kind == ProviderKind::Gemini
        && config
            .vertex_location
            .as_deref()
            .is_some_and(|location| !openwave_router::valid_vertex_location(location))
    {
        return Ok(false);
    }
    if matches!(
        kind,
        ProviderKind::OpenaiCompatible | ProviderKind::ModelGateway
    ) {
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
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Vec<CatalogModel>> {
    let mut models = Vec::new();
    for &kind in ProviderKind::ALL {
        let config = read_config(store, kind).await?;
        let available = provider_is_usable(store, secrets, kind, policy).await?;
        models.extend(model_registry::models_for(kind).map(|spec| CatalogModel {
            policy: ResolvedModelPolicy::curated(spec),
            available,
        }));
        if matches!(
            kind,
            ProviderKind::OpenaiCompatible | ProviderKind::ModelGateway
        ) {
            models.extend(config.models.iter().map(|model| CatalogModel {
                policy: ResolvedModelPolicy::custom_for(kind, model),
                available,
            }));
        }
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct TestSecrets(Mutex<HashMap<String, String>>);

    #[async_trait::async_trait]
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

    #[test]
    fn api_key_credential_roundtrips_and_redacts_debug() {
        let cred = ProviderCredential::api_key("sk-secret");
        let json = serde_json::to_string(&cred).unwrap();
        assert!(json.contains("api_key"));
        assert!(json.contains("sk-secret"));
        let back: ProviderCredential = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_api_key(), Some("sk-secret"));
        assert!(!format!("{cred:?}").contains("sk-secret"));

        // The OAuth marker is additive on the same tagged wire and carries no
        // key material of its own.
        let oauth: ProviderCredential = serde_json::from_str(r#"{"type":"oauth"}"#).unwrap();
        assert_eq!(oauth, ProviderCredential::Oauth {});
        assert_eq!(oauth.as_api_key(), None);
        assert_eq!(
            serde_json::to_string(&oauth).unwrap(),
            r#"{"type":"oauth"}"#
        );
    }

    #[test]
    fn service_account_credential_validates_roundtrips_and_redacts_debug() {
        let key_file = r#"{
            "type": "service_account",
            "project_id": "test-project",
            "private_key": "test-private-key",
            "client_email": "service-account@example.test"
        }"#;
        let cred = ProviderCredential::ServiceAccount {
            json: key_file.to_owned(),
        };

        cred.validate().expect("a complete key file is valid");
        let stored = serde_json::to_string(&cred).unwrap();
        let roundtrip: ProviderCredential = serde_json::from_str(&stored).unwrap();
        assert_eq!(roundtrip, cred);

        let debug = format!("{cred:?}");
        assert!(!debug.contains("test-private-key"));
        assert!(!debug.contains("service-account@example.test"));
    }

    #[test]
    fn service_account_validation_never_echoes_key_material() {
        let key_file = r#"{
            "type": "service_account",
            "client_email": "service-account@example.test",
            "private_key": "test-private-key"
        }"#;
        let error = ProviderCredential::ServiceAccount {
            json: key_file.to_owned(),
        }
        .validate()
        .expect_err("a key file missing project_id is invalid");

        assert!(!format!("{error:?}").contains("test-private-key"));
        assert!(!format!("{error:?}").contains("service-account@example.test"));
    }

    #[tokio::test]
    async fn credentials_roundtrip_through_secret_storage() {
        let secrets = TestSecrets::default();
        let credentials = [
            (
                ProviderKind::Anthropic,
                ProviderCredential::api_key("existing-api-key"),
            ),
            (
                ProviderKind::Openai,
                ProviderCredential::ServiceAccount {
                    json: r#"{"type":"service_account","client_email":"service-account@example.test","private_key":"test-private-key","project_id":"test-project"}"#.to_owned(),
                },
            ),
        ];

        for (kind, credential) in credentials {
            write_credential(&secrets, kind, &credential)
                .await
                .expect("a valid credential writes to secret storage");
            assert_eq!(
                read_credential(&secrets, kind).await.unwrap(),
                Some(credential)
            );
        }
    }

    #[tokio::test]
    async fn existing_api_key_blob_deserializes_unchanged() {
        let secrets = TestSecrets::default();
        secrets
            .set_secret(
                &ProviderKind::Openai.credential_key(),
                r#"{"type":"api_key","key":"existing-api-key"}"#,
            )
            .await
            .unwrap();

        assert_eq!(
            read_credential(&secrets, ProviderKind::Openai)
                .await
                .unwrap()
                .unwrap()
                .as_api_key(),
            Some("existing-api-key")
        );
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
        apply_model_policy(&mut config, &opus, Some(ReasoningEffort::XHigh)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("anthropic")));
        assert_eq!(config.model, "claude-opus-4-8");
        assert_eq!(config.context_window, 1_000_000);
        assert_eq!(config.max_tokens, Some(128_000));
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::XHigh));
        assert_eq!(config.temperature, None);

        // Haiku 4.5 reasons but rejects the effort control, so the request is
        // shaped without it rather than with something the model would refuse.
        let mut config = AgentConfig::default();
        let haiku = ResolvedModelPolicy::curated(
            model_registry::find_for(ProviderKind::Anthropic, "claude-haiku-4-5-20251001").unwrap(),
        );
        apply_model_policy(&mut config, &haiku, Some(ReasoningEffort::High)).unwrap();
        assert!(config.reasoning_model);
        assert_eq!(config.reasoning_effort, None);

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
        let custom = ResolvedModelPolicy::custom_for(
            ProviderKind::OpenaiCompatible,
            &CustomModelConfig {
                id: "local-model".into(),
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
            },
        );
        apply_model_policy(&mut config, &custom, Some(ReasoningEffort::High)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("openai_compatible")));
        assert_eq!(config.context_window, 32_768);
        assert_eq!(config.max_tokens, Some(4_096));
        assert!(!config.reasoning_model);
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn a_level_a_model_does_not_accept_never_survives_policy_application() {
        let apply = |id: &str, provider: ProviderKind, effort: ReasoningEffort| {
            let mut config = AgentConfig::default();
            let policy =
                ResolvedModelPolicy::curated(model_registry::find_for(provider, id).unwrap());
            apply_model_policy(&mut config, &policy, Some(effort)).unwrap();
            config.reasoning_effort
        };

        // `max` arrived with GPT-5.6; on 5.5 the same stored choice degrades to
        // the top level that generation takes rather than failing the turn.
        assert_eq!(
            apply("gpt-5.6-sol", ProviderKind::Openai, ReasoningEffort::Max),
            Some(ReasoningEffort::Max)
        );
        assert_eq!(
            apply("gpt-5.5", ProviderKind::Openai, ReasoningEffort::Max),
            Some(ReasoningEffort::XHigh)
        );
        // Anthropic has no "don't reason" level, so `none` comes up to `low`.
        assert_eq!(
            apply(
                "claude-opus-5",
                ProviderKind::Anthropic,
                ReasoningEffort::None
            ),
            Some(ReasoningEffort::Low)
        );
        assert_eq!(
            apply(
                "claude-opus-5",
                ProviderKind::Anthropic,
                ReasoningEffort::XHigh
            ),
            Some(ReasoningEffort::XHigh)
        );
        // Haiku 4.5 errors on the parameter, so it is dropped outright.
        for effort in ReasoningEffort::ALL {
            assert_eq!(
                apply(
                    "claude-haiku-4-5-20251001",
                    ProviderKind::Anthropic,
                    *effort
                ),
                None
            );
        }
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
                    ReasoningEffort::Low.clamp_to(spec.reasoning_efforts)
                );
                assert_eq!(
                    config.temperature,
                    (!spec.supports_reasoning).then_some(0.7)
                );
            }
        }
    }
}
