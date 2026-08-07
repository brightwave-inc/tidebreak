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
use std::collections::BTreeMap;

use openwave_core::{AgentConfig, ProviderId, ReasoningEffort, Result, SecretProvider, Store};

use crate::error::ServerError;
use crate::model_registry::{self, InputModality, ModelSpec, VerificationTier};

/// Setting-key prefix for per-provider non-secret config (`provider.<kind>`).
const PROVIDER_SETTING_PREFIX: &str = "provider.";

/// Secret-key suffix for the typed credential blob (`provider.<kind>.credential`).
const CREDENTIAL_SUFFIX: &str = ".credential";

/// Legacy Anthropic API-key secret (pre-providers). Still read as a fallback.
pub const LEGACY_ANTHROPIC_API_KEY: &str = "provider.anthropic.api_key";

/// Serializes the writers of the entitled-model snapshot: model sync and
/// sign-out.
///
/// Both read the resolved policy, decide what the snapshot should say, and
/// write it, and the [`Store`] API has no cross-call transaction — so
/// unserialized, a sign-out's clear could land inside a sync's
/// recheck-and-write and be overwritten by the models it was clearing.
/// Pairing deliberately does not take this lock: it writes only policy, and
/// the snapshot's deployment stamp already makes a racing sync's write
/// inert. Process-local for the same reason as pairing's own mutex: the
/// server's instance lock guarantees one process owns the store, and a
/// static cannot be accidentally wired into two instances that no longer
/// exclude each other. Lock order: acquired after the gateway runtime's
/// sign-in state lock, never before it, and never held across a network
/// call.
pub(crate) static GATEWAY_STATE_WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Setting key for the entitled-model snapshot synced from the managed
/// gateway. This replaces the retired `provider.model_gateway` row's model
/// list: policy names the gateway, this key caches what it entitles, and no
/// other stored state describes the gateway.
const GATEWAY_MODELS_KEY: &str = "gateway.models_v1";

/// The persisted snapshot of the managed gateway's entitled models, stamped
/// with the deployment it was synced from. The stamp is what keeps the cache
/// honest without coordinated clears: a snapshot synced from one gateway is
/// simply never honored while policy names another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GatewayModelSnapshot {
    /// The normalized gateway base URL the models were fetched from.
    pub(crate) gateway_url: String,
    pub(crate) models: Vec<CustomModelConfig>,
    /// Per-model inference protocol. An absent map is a snapshot written by a
    /// gateway generation that served only Anthropic Messages, so lookup
    /// defaults to that protocol for backward compatibility.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) model_protocols: BTreeMap<String, GatewayModelProtocol>,
}

/// The compatibility protocol a managed gateway uses for one entitled model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayModelProtocol {
    /// Anthropic Messages at `/compat/anthropic/v1/messages`.
    #[default]
    AnthropicMessages,
    /// OpenAI Chat Completions at `/compat/openai/v1/chat/completions`.
    OpenaiChatCompletions,
}

impl GatewayModelProtocol {
    /// Accept the canonical gateway values plus the short names older
    /// deployments used for the model-list filter.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" | "anthropic_messages" => Some(Self::AnthropicMessages),
            "openai" | "openai_compatible" | "openai_chat_completions" => {
                Some(Self::OpenaiChatCompletions)
            }
            _ => None,
        }
    }
}

/// Read the stored snapshot regardless of provenance. Callers that offer
/// models to anything must use [`gateway_models`] instead; this raw read
/// exists for selection-key resolution, where usability is enforced
/// separately by [`provider_is_usable`] and route collection.
pub(crate) async fn read_gateway_snapshot(
    store: &dyn Store,
) -> Result<Option<GatewayModelSnapshot>> {
    Ok(store
        .get_setting(GATEWAY_MODELS_KEY)
        .await?
        .and_then(|value| serde_json::from_value(value).ok()))
}

/// Persist the entitled-model snapshot. Callers hold
/// [`GATEWAY_STATE_WRITES`] across their policy recheck and this write.
pub(crate) async fn write_gateway_snapshot(
    store: &dyn Store,
    snapshot: &GatewayModelSnapshot,
) -> Result<()> {
    store
        .set_setting(GATEWAY_MODELS_KEY, &serde_json::to_value(snapshot)?)
        .await
}

/// The entitled models of the gateway the resolved policy names: empty for
/// an unmanaged profile, for a misconfigured policy, and for a snapshot
/// stamped by a different deployment.
pub(crate) async fn gateway_models(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Vec<CustomModelConfig>> {
    Ok(gateway_snapshot_for_policy(store, policy)
        .await?
        .map(|snapshot| snapshot.models)
        .unwrap_or_default())
}

async fn gateway_snapshot_for_policy(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Option<GatewayModelSnapshot>> {
    let Some(gateway_url) = policy.gateway_url.as_deref().filter(|_| policy.managed) else {
        return Ok(None);
    };
    Ok(read_gateway_snapshot(store)
        .await?
        .filter(|snapshot| snapshot.gateway_url == gateway_url))
}

/// One-shot boot cutover for the retired additive gateway configuration.
///
/// The `provider.model_gateway` row is no longer read anywhere: policy is
/// the only gateway source. Two things still have to happen once, on the
/// boot after the upgrade.
///
/// A managed profile keeps its models. Profiles paired from a gateway's own
/// page carry the entitled set the pairing synced, and nothing re-syncs it
/// for a reader who is already signed in — so without this their picker goes
/// empty until they find the refresh button. The row's list is carried into
/// [`GATEWAY_MODELS_KEY`] when it names the policy's own deployment.
///
/// An unmanaged profile's gateway vanishes, by decision: that mode is
/// retired, and the row is never read as identity, so nothing here can
/// convert a profile to managed — lockdown must not be imposed without the
/// pairing consent flow. One warning names the remedy.
///
/// Either way the row is dropped once it has been dealt with, which is what
/// makes "the provider row is retired" true of the store and not only of the
/// read paths — and keeps the warning a one-time upgrade notice.
pub(crate) async fn retire_legacy_gateway_row(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<()> {
    let Some(row) = store
        .get_setting(&ProviderKind::ModelGateway.setting_key())
        .await?
        .and_then(|value| serde_json::from_value::<ProviderConfig>(value).ok())
    else {
        return Ok(());
    };
    if row.base_url.is_none() && row.models.is_empty() {
        // Already retired (or never configured).
        return Ok(());
    }
    if !policy.managed {
        if row.base_url.is_some() {
            tracing::warn!(
                "this profile carries a legacy additive model-gateway configuration; \
                 that mode is retired and the stored configuration is ignored — \
                 pair via your gateway's page to reconnect"
            );
        }
        return clear_legacy_gateway_row(store).await;
    }
    let Some(gateway_url) = policy.gateway_url.clone() else {
        // A misconfigured managed policy names no deployment to attribute the
        // row to. Leave it untouched: the authority is repairable, and the
        // next boot can still carry the snapshot forward.
        return Ok(());
    };
    if row.models.is_empty() || read_gateway_snapshot(store).await?.is_some() {
        return clear_legacy_gateway_row(store).await;
    }
    // Compare deployments as URLs, not as strings. Pairing wrote the
    // normalized form, but a row written through the old provider route
    // before that path normalized (#935) holds whatever was typed — so a
    // profile that became managed by MDM over such a row has
    // `https://corp.gateway` beside a policy's `https://corp.gateway/`. An
    // unparseable legacy value can be attributed to no deployment and is
    // treated as a mismatch.
    let row_url = row
        .base_url
        .as_deref()
        .and_then(|url| crate::managed_policy::validated_gateway_url(url).ok());
    if row_url.as_deref() != Some(gateway_url.as_str()) {
        // A row from a gateway this profile is no longer managed by: its
        // models describe another deployment's entitlements. Rare, which is
        // exactly when the reason for an empty picker is worth a log line.
        tracing::warn!(
            "the legacy model-gateway row names a different deployment than the \
             managed policy ({:?} vs {gateway_url}); its synced models are \
             discarded — refresh the model list to resync the entitled set",
            row.base_url
        );
        return clear_legacy_gateway_row(store).await;
    }
    write_gateway_snapshot(
        store,
        &GatewayModelSnapshot {
            gateway_url,
            models: row.models,
            model_protocols: BTreeMap::new(),
        },
    )
    .await?;
    clear_legacy_gateway_row(store).await
}

/// Drop the retired row. Written as the disabled default rather than deleted
/// outright: [`Store`] exposes no setting delete, and the default is exactly
/// what an absent row already reads as everywhere.
async fn clear_legacy_gateway_row(store: &dyn Store) -> Result<()> {
    write_config(
        store,
        ProviderKind::ModelGateway,
        &ProviderConfig::disabled(),
    )
    .await
}

/// The known provider kinds. `#[non_exhaustive]` so new kinds can land without
/// breaking wire clients that match on the string form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderKind {
    /// Anthropic Messages API.
    Anthropic,
    /// Native OpenAI Responses API (api.openai.com).
    Openai,
    /// Native xAI Responses API (api.x.ai).
    Xai,
    /// Google Gemini Developer API (generativelanguage.googleapis.com).
    Gemini,
    /// Google Vertex AI with native Gemini and Anthropic Claude protocols.
    Vertex,
    /// Any OpenAI-compatible Chat Completions gateway (OpenRouter, vLLM, …).
    OpenaiCompatible,
    /// A signed-in model-gateway deployment: entitled models synced from the
    /// gateway, inference through each model's Anthropic- or OpenAI-compatible
    /// surface with short-lived OAuth tokens.
    ModelGateway,
}

impl ProviderKind {
    /// All kinds the server knows about, in display order.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::Anthropic,
        ProviderKind::Openai,
        ProviderKind::Xai,
        ProviderKind::Gemini,
        ProviderKind::Vertex,
        ProviderKind::OpenaiCompatible,
        ProviderKind::ModelGateway,
    ];

    /// Wire/path form (`anthropic`, `openai`, `openai_compatible`).
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::Xai => "xai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Vertex => "vertex",
            ProviderKind::OpenaiCompatible => "openai_compatible",
            ProviderKind::ModelGateway => "model_gateway",
        }
    }

    /// Parse a path segment into a kind.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" => Some(Self::Openai),
            "xai" => Some(Self::Xai),
            "gemini" => Some(Self::Gemini),
            "vertex" => Some(Self::Vertex),
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
        match credential {
            ProviderCredential::ApiKey { .. } => matches!(
                self,
                ProviderKind::Anthropic
                    | ProviderKind::Openai
                    | ProviderKind::Gemini
                    | ProviderKind::OpenaiCompatible
            ),
            ProviderCredential::Oauth {} => self == ProviderKind::Openai,
            ProviderCredential::ServiceAccount { .. } => {
                matches!(self, ProviderKind::Gemini | ProviderKind::Vertex)
            }
        }
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
    pub(crate) fn as_service_account(&self) -> Option<&str> {
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
    /// Vertex AI location. The first-class Vertex provider accepts only
    /// `global`; regional values are retained only for legacy Gemini
    /// service-account configurations. An absent value defaults to `global`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertex_location: Option<String>,
    /// Explicit model rows served by a configurable endpoint.
    ///
    /// OpenAI-compatible and xAI catalogs can change independently of an
    /// OpenWave release, so their configured rows live beside the endpoint.
    /// Other curated providers ignore this field and obtain their models from
    /// the host registry.
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

fn default_custom_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text]
}

/// User-inspectable routing limits and capabilities for one configured model.
///
/// OpenAI-compatible rows are validated to the conservative text-only shape.
/// xAI rows may opt into the capabilities its first-party Responses adapter
/// actually carries end to end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct CustomModelConfig {
    /// Exact model id sent to the endpoint.
    pub id: String,
    /// Optional human-facing label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The provider-side id a managed gateway routes this model to, when the
    /// gateway reports one that differs from `id`.
    ///
    /// Populated only by gateway model sync — a user-entered custom model
    /// leaves it unset, and nothing in the settings UI offers it. It exists so
    /// a deployment-aliased id can still be recognized as a curated model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_id: Option<String>,
    /// Context limit used by OpenWave's reducer.
    #[serde(default = "default_custom_context_window")]
    pub context_window: u32,
    /// Maximum output sent to the endpoint.
    #[serde(default = "default_custom_max_output_tokens")]
    pub max_output_tokens: u32,
    /// Inputs OpenWave may place on this model's request.
    #[serde(default = "default_custom_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    /// Whether the model uses xAI's reasoning request shape.
    #[serde(default)]
    pub supports_reasoning: bool,
    /// Reasoning-effort levels accepted by the model, ascending.
    #[serde(default)]
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

impl Default for CustomModelConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: None,
            upstream_id: None,
            context_window: default_custom_context_window(),
            max_output_tokens: default_custom_max_output_tokens(),
            input_modalities: default_custom_input_modalities(),
            supports_reasoning: false,
            reasoning_efforts: Vec::new(),
        }
    }
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
    /// The vendor whose curated row this model matched, when the routing
    /// provider is not itself that vendor — a gateway-served model that is
    /// exactly a curated id. Presentation only (icon and branding); routing
    /// always uses `provider`.
    pub vendor: Option<ProviderKind>,
    /// How thoroughly this exact provider/model combination has been exercised.
    pub verification: VerificationTier,
    /// Runtime context reduction limit.
    pub context_window: u32,
    /// Runtime output cap.
    pub max_output_tokens: u32,
    /// End-to-end supported input modalities.
    pub input_modalities: Vec<InputModality>,
    /// Whether the provider request uses a reasoning-model shape.
    pub supports_reasoning: bool,
    /// Whether a turn on this model may run the provider's own server-side web
    /// search instead of the host's. Asserts that the routing adapter emits the
    /// vendor tool, so it is never inherited by a pass-through route.
    pub supports_vendor_web_search: bool,
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
            vendor: spec.vendor(),
            verification: spec.verification,
            context_window: spec.context_window,
            max_output_tokens: spec.max_output_tokens,
            input_modalities: spec.input_modalities.to_vec(),
            supports_reasoning: spec.supports_reasoning,
            supports_vendor_web_search: spec.supports_vendor_web_search,
            reasoning_efforts: spec.reasoning_efforts.to_vec(),
        }
    }

    /// Resolve one gateway-entitled model.
    ///
    /// A managed gateway serves models by their upstream ids, so an id that is
    /// exactly a curated one is that curated model reached over a different
    /// route. Inheriting the curated row's capabilities keeps it from being
    /// presented as an anonymous unverified endpoint that has lost image input
    /// and reasoning. What is *not* inherited is the routing provider — the
    /// gateway still serves the request — nor the limits: the deployment's own
    /// reported context and output caps are authoritative for that deployment,
    /// which may be narrower than the upstream model's.
    ///
    /// Deployments also alias their ids (`anthropic-us-claude-opus-5` for an
    /// upstream `us.anthropic.claude-opus-5`), which no exact match can see.
    /// When the gateway reports the upstream id, it is tried next — exactly,
    /// then with the deployment decoration stripped by
    /// [`curated_id_candidate`].
    ///
    /// An id that matches no curated row either way keeps the conservative
    /// custom treatment.
    fn gateway_for(model: &CustomModelConfig) -> Self {
        let mut policy = Self::custom_for(ProviderKind::ModelGateway, model);
        let Some(spec) = model_registry::find(&model.id).or_else(|| {
            let upstream = model.upstream_id.as_deref()?;
            model_registry::find(upstream)
                .or_else(|| model_registry::find(&curated_id_candidate(upstream)))
        }) else {
            return policy;
        };
        policy.display_name = spec.display_name.to_owned();
        policy.vendor = Some(spec.provider);
        policy.verification = spec.verification;
        policy.input_modalities = spec.input_modalities.to_vec();
        policy.supports_reasoning = spec.supports_reasoning;
        policy.reasoning_efforts = spec.reasoning_efforts.to_vec();
        policy
    }

    fn custom_for(provider: ProviderKind, model: &CustomModelConfig) -> Self {
        let first_party_xai = provider == ProviderKind::Xai;
        Self {
            key: model_registry::selection_key(provider, &model.id),
            id: model.id.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model_registry::display_name_for(&model.id)),
            provider,
            vendor: None,
            verification: VerificationTier::Unverified,
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            input_modalities: if first_party_xai {
                model.input_modalities.clone()
            } else {
                vec![InputModality::Text]
            },
            supports_reasoning: first_party_xai && model.supports_reasoning,
            // A pass-through endpoint cannot promise a provider-executed
            // search, whatever upstream model it is serving — the request
            // shape that would enable one is the vendor's own, and this route
            // does not speak it. Deliberately not inherited by `gateway_for`.
            supports_vendor_web_search: false,
            reasoning_efforts: if first_party_xai {
                model.reasoning_efforts.clone()
            } else {
                Vec::new()
            },
        }
    }

    fn legacy_custom(id: &str) -> Self {
        Self::custom_for(
            ProviderKind::OpenaiCompatible,
            &CustomModelConfig {
                id: id.to_owned(),
                display_name: None,
                upstream_id: None,
                context_window: default_custom_context_window(),
                max_output_tokens: default_custom_max_output_tokens(),
                input_modalities: default_custom_input_modalities(),
                supports_reasoning: false,
                reasoning_efforts: Vec::new(),
            },
        )
    }
}

/// Strip the deployment decoration a hosted provider adds to a curated model
/// id, so `us.anthropic.claude-opus-5` can be recognized as `claude-opus-5`.
///
/// Deliberately narrow: one leading region prefix, then one leading vendor
/// prefix, then one trailing version suffix — nothing else. Anything broader
/// would start collapsing distinct curated ids into each other, and a wrong
/// match here silently attributes another model's capabilities. The result is
/// only ever *offered* to the registry; a miss keeps the conservative
/// treatment.
fn curated_id_candidate(upstream_id: &str) -> String {
    const REGION_PREFIXES: [&str; 5] = ["us.", "eu.", "apac.", "jp.", "global."];
    const VENDOR_PREFIXES: [&str; 1] = ["anthropic."];

    let mut candidate = upstream_id;
    for prefix in REGION_PREFIXES {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest;
            break;
        }
    }
    for prefix in VENDOR_PREFIXES {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest;
            break;
        }
    }
    strip_version_suffix(candidate).to_owned()
}

/// Drop a trailing `-v<digits>` or `-v<digits>:<digits>` — Bedrock's revision
/// marker. Left alone when the tail is not exactly that shape.
fn strip_version_suffix(id: &str) -> &str {
    let Some((head, tail)) = id.rsplit_once("-v") else {
        return id;
    };
    if head.is_empty() {
        return id;
    }
    let numeric = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit());
    let matches = match tail.split_once(':') {
        Some((major, minor)) => numeric(major) && numeric(minor),
        None => numeric(tail),
    };
    if matches {
        head
    } else {
        id
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
    config.image_input = policy.input_modalities.contains(&InputModality::Image);
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

/// Resolve a selection against the curated registry alone — no store reads and
/// no legacy free-form fallback.
///
/// A selection key names its provider outright; a bare id resolves only when
/// exactly one curated row answers to it, so an ambiguous name is left to the
/// caller rather than routed by guesswork.
pub fn curated_model_policy(value: &str) -> Option<ResolvedModelPolicy> {
    if let Some((provider, id)) = model_registry::parse_selection_key(value) {
        return model_registry::find_for(provider, id).map(ResolvedModelPolicy::curated);
    }
    model_registry::find(value).map(ResolvedModelPolicy::curated)
}

/// Configure a turn whose selection did not resolve through the host registry.
///
/// Embedders that inject a provider keep their free-form model contract, but a
/// model the registry already owns still runs under its own policy: without the
/// provider hint the router may serve it from any OpenAI-compatible route, and
/// without the policy's reasoning flags a stored effort is shaped into a request
/// the endpoint never agreed to take. An id nothing in the registry claims keeps
/// the raw model, and its effort stays off the wire the way it already does —
/// every adapter sends the parameter only for a config that claims a reasoning
/// model, which no unregistered selection ever sets on its own.
pub fn apply_free_form_model(
    config: &mut AgentConfig,
    model: String,
    reasoning_effort: Option<ReasoningEffort>,
) -> Result<()> {
    if let Some(policy) = curated_model_policy(&model) {
        return apply_model_policy(config, &policy, reasoning_effort);
    }
    config.model = model;
    config.reasoning_effort = reasoning_effort;
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
    /// Vertex AI location. The first-class Vertex provider is global-only;
    /// legacy Gemini service-account configurations may retain a region. Never
    /// includes the project id from the credential.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vertex_location: Option<String>,
    /// Whether a credential is stored (never the credential itself).
    pub has_credential: bool,
    /// How OpenAI (or similarly dual-mode providers) is authenticated.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth_mode: Option<ProviderAuthMode>,
    /// Explicit configured model entries for this endpoint.
    pub models: Vec<CustomModelConfig>,
}

/// How a provider's credential was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuthMode {
    /// Pasted / env API key.
    ApiKey,
    /// ChatGPT subscription OAuth.
    Chatgpt,
    /// Uploaded Google Cloud service-account key file.
    ServiceAccount,
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
    /// Replacement configured-model list. Valid for `openai_compatible` and
    /// first-party xAI.
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
    if !kind.accepts_credential(credential) {
        return Err(ServerError::bad_request(format!(
            "{kind} does not support this credential type"
        )));
    }
    // Mutual exclusivity for OpenAI: an API key replaces ChatGPT OAuth.
    if kind == ProviderKind::Openai && matches!(credential, ProviderCredential::ApiKey { .. }) {
        let _ = secrets
            .delete_secret(openwave_connectors::CHATGPT_SECRET_KEY)
            .await;
    }
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
    if kind == ProviderKind::Openai {
        let _ = secrets
            .delete_secret(openwave_connectors::CHATGPT_SECRET_KEY)
            .await;
    }
    Ok(())
}

/// Whether `kind` has a usable credential — stored, or (for direct API-key providers)
/// the matching env fallback the resolver also honors.
pub async fn has_credential(secrets: &dyn SecretProvider, kind: ProviderKind) -> bool {
    match read_credential(secrets, kind).await {
        Ok(Some(credential)) => {
            if credential.as_api_key().is_some_and(|key| !key.is_empty()) {
                return true;
            }
            if matches!(kind, ProviderKind::Gemini | ProviderKind::Vertex)
                && credential.as_service_account().is_some_and(|json| {
                    openwave_router::GoogleServiceAccount::from_json(json).is_ok()
                })
            {
                return true;
            }
            if kind == ProviderKind::Openai && matches!(credential, ProviderCredential::Oauth {}) {
                return openwave_connectors::has_stored_chatgpt_credentials(secrets).await;
            }
            return false;
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

async fn auth_mode_for(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> Option<ProviderAuthMode> {
    match read_credential(secrets, kind).await.ok().flatten() {
        Some(ProviderCredential::Oauth {})
            if openwave_connectors::has_stored_chatgpt_credentials(secrets).await =>
        {
            Some(ProviderAuthMode::Chatgpt)
        }
        Some(ProviderCredential::ApiKey { key })
            if kind == ProviderKind::Openai && !key.is_empty() =>
        {
            Some(ProviderAuthMode::ApiKey)
        }
        Some(ProviderCredential::ServiceAccount { json })
            if matches!(kind, ProviderKind::Gemini | ProviderKind::Vertex)
                && openwave_router::GoogleServiceAccount::from_json(&json).is_ok() =>
        {
            Some(ProviderAuthMode::ServiceAccount)
        }
        None if kind == ProviderKind::Openai && env_api_key(kind).is_some() => {
            Some(ProviderAuthMode::ApiKey)
        }
        _ => None,
    }
}

/// Build the public [`ProviderInfo`] for every known kind.
///
/// The gateway is not an additive provider: an unmanaged profile has no
/// gateway entry at all, and a managed profile's entry is a projection of
/// the resolved policy plus the synced snapshot — never a stored row.
pub async fn list_providers(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<Vec<ProviderInfo>> {
    let mut out = Vec::with_capacity(ProviderKind::ALL.len());
    for &kind in ProviderKind::ALL {
        if kind == ProviderKind::ModelGateway {
            if !policy.managed {
                continue;
            }
            out.push(ProviderInfo {
                kind,
                enabled: policy.gateway_url.is_some(),
                base_url: policy.gateway_url.clone(),
                vertex_location: None,
                // Deployment-matched: a session a re-point superseded must
                // not read as this deployment's credential.
                has_credential: match policy.gateway_url.as_deref() {
                    Some(gateway_url) => {
                        openwave_connectors::has_stored_credentials_for(secrets, gateway_url).await
                    }
                    None => false,
                },
                auth_mode: None,
                models: gateway_models(store, policy).await?,
            });
            continue;
        }
        let config = read_config(store, kind).await?;
        out.push(ProviderInfo {
            kind,
            enabled: config.enabled,
            base_url: config.base_url.clone(),
            vertex_location: config.vertex_location.clone(),
            has_credential: has_credential(secrets, kind).await,
            auth_mode: auth_mode_for(secrets, kind).await,
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
/// The ModelGateway kind refuses every write: policy is the only gateway
/// source, so the profile connects through MDM policy or deep-link pairing
/// and there is no user-writable gateway configuration in any state. On a
/// managed profile, BYOK providers are locked: credential and base-URL
/// writes for any other kind are refused.
pub async fn update_provider(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    update: ProviderUpdate,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
) -> std::result::Result<ProviderInfo, ServerError> {
    if kind == ProviderKind::ModelGateway {
        // Refused wholesale, managed or not — before any read, so no lock is
        // needed here and nothing this route does can race the snapshot
        // writers. The stable kind lets clients branch on it, exactly as
        // `managed_profile` does for the BYOK lockdown.
        return Err(ServerError::conflict_kind(
            "gateway_policy",
            "the model gateway is configured by policy, not provider settings; \
             pair via your gateway's page to connect",
        ));
    }
    let policy = crate::managed_policy::resolve(store, os_policy).await?;
    if policy.managed && (update.credential.is_some() || update.base_url.is_some()) {
        return Err(managed_profile_refusal(format!(
            "this profile is managed by a model gateway; {kind} credentials and endpoints are locked"
        )));
    }

    let mut config = read_config(store, kind).await?;

    if let Some(enabled) = update.enabled {
        config.enabled = enabled;
    }
    match update.base_url {
        None => {}
        Some(_) if kind == ProviderKind::Xai => {
            return Err(ServerError::bad_request(
                "xai uses its fixed first-party API endpoint",
            ));
        }
        Some(_) if kind == ProviderKind::Vertex => {
            return Err(ServerError::bad_request(
                "Vertex AI uses fixed Google endpoints and does not support base_url",
            ));
        }
        Some(None) => config.base_url = None,
        Some(Some(url)) => {
            if url.is_empty() {
                return Err(ServerError::bad_request("base_url must not be empty"));
            }
            config.base_url = Some(url);
        }
    }
    match update.vertex_location {
        None => {}
        Some(_) if !matches!(kind, ProviderKind::Gemini | ProviderKind::Vertex) => {
            return Err(ServerError::bad_request(
                "vertex_location is only supported by vertex and legacy gemini service accounts",
            ));
        }
        Some(None) => config.vertex_location = None,
        Some(Some(location)) => {
            if kind == ProviderKind::Vertex && location != "global" {
                return Err(ServerError::bad_request(
                    "the first-class Vertex provider supports only vertex_location `global`",
                ));
            }
            if location.trim() != location || !openwave_router::valid_vertex_location(&location) {
                return Err(ServerError::bad_request(
                    "vertex_location must be a valid Google Cloud location",
                ));
            }
            config.vertex_location = Some(location);
        }
    }
    if kind == ProviderKind::Vertex
        && config.enabled
        && !configured_vertex_location_is_usable(kind, config.vertex_location.as_deref())
    {
        return Err(ServerError::bad_request(
            "the first-class Vertex provider supports only vertex_location `global`; update the stored location before enabling it",
        ));
    }
    if let Some(models) = update.models {
        if !matches!(kind, ProviderKind::OpenaiCompatible | ProviderKind::Xai) {
            return Err(ServerError::bad_request(
                "configured models are only supported by openai_compatible and xai",
            ));
        }
        validate_configured_models(kind, &models)?;
        config.models = models;
    }

    // openai_compatible needs a base URL to be useful when enabled.
    if kind == ProviderKind::OpenaiCompatible && config.enabled && config.base_url.is_none() {
        return Err(ServerError::bad_request(
            "openai_compatible requires a base_url when enabled",
        ));
    }
    if kind == ProviderKind::Xai && config.enabled && config.models.is_empty() {
        return Err(ServerError::bad_request(
            "xai requires at least one configured model when enabled",
        ));
    }

    if let Some(credential) = update.credential {
        if let Some(json) = credential.as_service_account() {
            openwave_router::GoogleServiceAccount::from_json(json).map_err(|_| {
                ServerError::bad_request("invalid Google service-account credential")
            })?;
        }
        write_credential(secrets, kind, &credential).await?;
    }

    write_config(store, kind, &config).await?;

    Ok(ProviderInfo {
        kind,
        enabled: config.enabled,
        base_url: config.base_url.clone(),
        vertex_location: config.vertex_location.clone(),
        has_credential: has_credential(secrets, kind).await,
        auth_mode: auth_mode_for(secrets, kind).await,
        models: config.models,
    })
}

pub(crate) fn validate_custom_models(
    models: &[CustomModelConfig],
) -> std::result::Result<(), ServerError> {
    validate_configured_models(ProviderKind::OpenaiCompatible, models)
}

fn validate_configured_models(
    kind: ProviderKind,
    models: &[CustomModelConfig],
) -> std::result::Result<(), ServerError> {
    validate_configured_models_against(kind, models, |id| {
        model_registry::find_for(kind, id).is_some()
    })
}

fn validate_configured_models_against(
    kind: ProviderKind,
    models: &[CustomModelConfig],
    is_curated: impl Fn(&str) -> bool,
) -> std::result::Result<(), ServerError> {
    const MAX_CUSTOM_MODELS: usize = 64;
    const MAX_MODEL_ID_CHARS: usize = 240;
    const MAX_DISPLAY_NAME_CHARS: usize = 120;
    const MAX_CONTEXT_WINDOW: u32 = 4_000_000;

    if models.len() > MAX_CUSTOM_MODELS {
        return Err(ServerError::bad_request(format!(
            "{kind} supports at most {MAX_CUSTOM_MODELS} configured models"
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
                "configured model id must be non-empty, bounded, and contain no whitespace or control characters",
            ));
        }
        if id != model.id {
            return Err(ServerError::bad_request(
                "configured model id must not have leading or trailing whitespace",
            ));
        }
        if !ids.insert(id) {
            return Err(ServerError::bad_request(format!(
                "duplicate configured model id `{id}`"
            )));
        }
        if is_curated(id) {
            return Err(ServerError::bad_request(format!(
                "configured model id `{id}` conflicts with a curated {kind} model"
            )));
        }
        if model.display_name.as_ref().is_some_and(|name| {
            name.trim().is_empty()
                || name.trim() != name
                || name.chars().count() > MAX_DISPLAY_NAME_CHARS
                || name.chars().any(char::is_control)
        }) {
            return Err(ServerError::bad_request(
                "configured model display_name must be non-empty, bounded, and contain no control characters",
            ));
        }
        if !(1_024..=MAX_CONTEXT_WINDOW).contains(&model.context_window) {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` context_window must be between 1024 and {MAX_CONTEXT_WINDOW}"
            )));
        }
        if model.max_output_tokens == 0 || model.max_output_tokens > model.context_window {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` max_output_tokens must be positive and not exceed context_window"
            )));
        }
        if model.input_modalities.is_empty()
            || !model.input_modalities.contains(&InputModality::Text)
            || model
                .input_modalities
                .iter()
                .enumerate()
                .any(|(index, modality)| model.input_modalities[..index].contains(modality))
        {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` input_modalities must contain text exactly once and no duplicates"
            )));
        }
        if kind != ProviderKind::Xai && model.input_modalities != [InputModality::Text] {
            return Err(ServerError::bad_request(
                "only xai configured models may enable image input",
            ));
        }
        if !model.supports_reasoning && !model.reasoning_efforts.is_empty() {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` cannot list reasoning_efforts when supports_reasoning is false"
            )));
        }
        if kind != ProviderKind::Xai
            && (model.supports_reasoning || !model.reasoning_efforts.is_empty())
        {
            return Err(ServerError::bad_request(
                "only xai configured models may enable reasoning",
            ));
        }
        if model
            .reasoning_efforts
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` reasoning_efforts must be unique and ascending"
            )));
        }
        if model.reasoning_efforts.iter().any(|effort| {
            !matches!(
                effort,
                ReasoningEffort::None
                    | ReasoningEffort::Low
                    | ReasoningEffort::Medium
                    | ReasoningEffort::High
                    | ReasoningEffort::XHigh
            )
        }) {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` uses a reasoning effort xai does not support"
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
        ProviderKind::Xai => std::env::var("XAI_API_KEY").ok().filter(|k| !k.is_empty()),
        ProviderKind::Gemini => std::env::var("GEMINI_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        // Vertex credentials are explicit service-account material in the
        // keychain. Ambient ADC and API-key modes are not implied.
        ProviderKind::Vertex => None,
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
        ProviderKind::Xai => openwave_router::RouteKind::Xai,
        ProviderKind::Gemini => openwave_router::RouteKind::Gemini,
        ProviderKind::Vertex => openwave_router::RouteKind::Vertex,
        ProviderKind::OpenaiCompatible => openwave_router::RouteKind::OpenaiCompatible,
        ProviderKind::ModelGateway => openwave_router::RouteKind::ModelGateway,
    }
}

/// Whether the configured Vertex location belongs to the provider surface.
///
/// First-class Vertex is global-only because every curated row on that surface
/// is global-only today. Legacy Gemini service-account configurations keep
/// accepting validated regions; their adapter continues to move Gemini 3
/// requests to the global endpoint while preserving older regional behavior.
fn configured_vertex_location_is_usable(kind: ProviderKind, vertex_location: Option<&str>) -> bool {
    match kind {
        ProviderKind::Vertex => vertex_location.unwrap_or("global") == "global",
        ProviderKind::Gemini => vertex_location
            .map(openwave_router::valid_vertex_location)
            .unwrap_or(true),
        _ => true,
    }
}

/// Collect enabled, credentialed routes for the composite router.
///
/// A kind with no usable credential is skipped. Gemini service accounts become
/// Vertex routes; API keys remain Developer API routes. OpenAI ChatGPT OAuth
/// becomes a Codex-backend route via `chatgpt`. Store-read failures for a
/// single kind skip that kind (fail closed for it) rather than aborting the
/// whole list.
///
/// On a managed profile only the gateway route is offered: BYOK kinds are
/// skipped before any credential is read, so stored keys and the env-var
/// fallbacks are inert without being deleted, and the gateway's bearer target
/// is the policy URL. On an unmanaged profile there is no gateway route at
/// all: policy is the only gateway source, and a legacy stored row is never
/// read.
pub async fn collect_routes(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    gateway_tokens: Option<std::sync::Arc<dyn openwave_router::BearerTokenSource>>,
    chatgpt: Option<(
        std::sync::Arc<dyn openwave_router::BearerTokenSource>,
        String,
    )>,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Vec<openwave_router::Route> {
    let mut routes = Vec::new();
    for &kind in ProviderKind::ALL {
        if kind == ProviderKind::ModelGateway {
            if !policy.managed {
                continue;
            }
            // The gateway route rides its live token source; without a signed-in
            // session there is nothing to route to.
            let Some(source) = gateway_tokens.clone() else {
                continue;
            };
            let Some(base) = policy.gateway_url.as_deref() else {
                continue;
            };
            if !(base.starts_with("https://") || base.starts_with("http://")) {
                continue;
            }
            let (models, model_protocols) = match gateway_snapshot_for_policy(store, policy)
                .await
                .unwrap_or_default()
            {
                Some(GatewayModelSnapshot {
                    models,
                    model_protocols,
                    ..
                }) => (models, model_protocols),
                None => (Vec::new(), BTreeMap::new()),
            };
            let mut anthropic_models = Vec::new();
            let mut openai_models = Vec::new();
            for model in models {
                match model_protocols.get(&model.id).copied().unwrap_or_default() {
                    GatewayModelProtocol::AnthropicMessages => anthropic_models.push(model.id),
                    GatewayModelProtocol::OpenaiChatCompletions => openai_models.push(model.id),
                }
            }
            let base = base.trim_end_matches('/');
            // Preserve the pre-protocol route shape before the first sync (or
            // for an empty entitlement set): it still selects no model, but a
            // signed-in managed profile has one inert gateway adapter rather
            // than looking indistinguishable from missing credentials.
            if anthropic_models.is_empty() && openai_models.is_empty() {
                routes.push(openwave_router::Route {
                    kind: route_kind(kind),
                    api_key: String::new(),
                    base_url: Some(format!("{base}/compat/anthropic")),
                    curated_models: Vec::new(),
                    token_source: Some(source),
                    vertex: None,
                    chatgpt_account_id: None,
                });
            } else {
                if !anthropic_models.is_empty() {
                    routes.push(openwave_router::Route {
                        kind: route_kind(kind),
                        api_key: String::new(),
                        base_url: Some(format!("{base}/compat/anthropic")),
                        curated_models: anthropic_models,
                        token_source: Some(source.clone()),
                        vertex: None,
                        chatgpt_account_id: None,
                    });
                }
                if !openai_models.is_empty() {
                    routes.push(openwave_router::Route {
                        kind: openwave_router::RouteKind::ModelGatewayOpenai,
                        api_key: String::new(),
                        base_url: Some(format!("{base}/compat/openai/v1")),
                        curated_models: openai_models,
                        token_source: Some(source),
                        vertex: None,
                        chatgpt_account_id: None,
                    });
                }
            }
            continue;
        }
        if policy.managed {
            continue;
        }
        let config = match read_config(store, kind).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !config.enabled {
            continue;
        }
        if !configured_vertex_location_is_usable(kind, config.vertex_location.as_deref()) {
            continue;
        }

        let stored_credential = match read_credential(secrets, kind).await {
            Ok(credential) => credential,
            Err(_) => continue,
        };
        if kind == ProviderKind::Openai
            && matches!(stored_credential, Some(ProviderCredential::Oauth {}))
        {
            let Some((source, account_id)) = chatgpt.clone() else {
                continue;
            };
            routes.push(openwave_router::Route {
                kind: route_kind(kind),
                api_key: String::new(),
                base_url: Some(openwave_connectors::CODEX_BASE_URL.to_string()),
                curated_models: model_registry::models_for(kind)
                    .map(|spec| spec.id.to_string())
                    .collect(),
                token_source: Some(source),
                vertex: None,
                chatgpt_account_id: Some(account_id),
            });
            continue;
        }
        if matches!(kind, ProviderKind::Gemini | ProviderKind::Vertex) {
            if let Some(ProviderCredential::ServiceAccount { json }) = stored_credential.as_ref() {
                let Ok(account) = openwave_router::GoogleServiceAccount::from_json(json) else {
                    continue;
                };
                let location = config.vertex_location.as_deref().unwrap_or("global");
                let project_id = account.project_id().to_owned();
                let credential_fingerprint: [u8; 32] = Sha256::digest(json.as_bytes()).into();
                let source = std::sync::Arc::new(
                    openwave_router::GoogleServiceAccountTokenSource::new(account),
                );
                let curated_models = model_registry::models_for(kind).collect::<Vec<_>>();
                let vertex =
                    openwave_router::VertexRoute::new(project_id, location, credential_fingerprint);
                let vertex = if kind == ProviderKind::Vertex {
                    vertex.with_model_families(curated_models.iter().filter_map(|spec| {
                        let family = match spec.vendor()? {
                            ProviderKind::Gemini => openwave_router::VertexModelFamily::Gemini,
                            ProviderKind::Anthropic => {
                                openwave_router::VertexModelFamily::Anthropic
                            }
                            _ => return None,
                        };
                        Some((spec.id.to_string(), family))
                    }))
                } else {
                    vertex
                };
                routes.push(openwave_router::Route {
                    kind: route_kind(kind),
                    api_key: String::new(),
                    // Production Vertex hosts are derived by the adapter.
                    // Never let stored config redirect a bearer token.
                    base_url: None,
                    curated_models: curated_models
                        .into_iter()
                        .map(|spec| spec.id.to_string())
                        .collect(),
                    token_source: Some(source),
                    vertex: Some(vertex),
                    chatgpt_account_id: None,
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
        if kind == ProviderKind::Xai && config.models.is_empty() {
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
            // Gemini, xAI, and Vertex use fixed first-party endpoints in production.
            // Never let a stale/directly written setting redirect their keys.
            base_url: (!matches!(
                kind,
                ProviderKind::Gemini | ProviderKind::Xai | ProviderKind::Vertex
            ))
                .then_some(config.base_url)
                .flatten(),
            curated_models: model_registry::models_for(kind)
                .map(|spec| spec.id.to_string())
                .chain(config.models.into_iter().map(|model| model.id))
                .collect(),
            token_source: None,
            vertex: None,
            chatgpt_account_id: None,
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
        let models = match provider {
            ProviderKind::OpenaiCompatible | ProviderKind::Xai => {
                read_config(store, provider).await?.models
            }
            // Resolution is not an offer: usability and routing gate the
            // gateway on policy separately, so the raw snapshot read here
            // only keeps stored selections legible.
            ProviderKind::ModelGateway => read_gateway_snapshot(store)
                .await?
                .map(|snapshot| snapshot.models)
                .unwrap_or_default(),
            _ => return Ok(None),
        };
        return Ok(models
            .iter()
            .find(|model| model.id == id)
            .map(|model| match provider {
                ProviderKind::ModelGateway => ResolvedModelPolicy::gateway_for(model),
                _ => ResolvedModelPolicy::custom_for(provider, model),
            }));
    }

    let mut owners = model_registry::find(value)
        .map(ResolvedModelPolicy::curated)
        .into_iter()
        .collect::<Vec<_>>();
    for provider in [ProviderKind::Xai, ProviderKind::OpenaiCompatible] {
        let config = read_config(store, provider).await?;
        owners.extend(
            config
                .models
                .iter()
                .filter(|model| model.id == value)
                .map(|model| ResolvedModelPolicy::custom_for(provider, model)),
        );
    }
    match owners.len() {
        1 => return Ok(owners.pop()),
        count if count > 1 => return Ok(None),
        _ => {}
    }
    Ok(allow_legacy_custom.then(|| ResolvedModelPolicy::legacy_custom(value)))
}

/// Whether the provider can accept a new turn right now.
///
/// The gateway is derived from policy in both directions: on a managed
/// profile it is usable exactly when a session for the policy's own
/// deployment is stored (and BYOK kinds never are), and on an unmanaged
/// profile it is never usable — whatever legacy rows persist in the store.
/// The deployment match matters after an MDM re-point: the superseded
/// session is unroutable (`route_token_source` filters it), so counting it
/// usable would advertise models no route can serve.
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
        let Some(gateway_url) = policy.gateway_url.as_deref() else {
            return Ok(false);
        };
        return Ok(openwave_connectors::has_stored_credentials_for(secrets, gateway_url).await);
    }
    if kind == ProviderKind::ModelGateway {
        return Ok(false);
    }
    let config = read_config(store, kind).await?;
    if !config.enabled
        || !configured_vertex_location_is_usable(kind, config.vertex_location.as_deref())
    {
        return Ok(false);
    }
    if !has_credential(secrets, kind).await {
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
    if kind == ProviderKind::Xai && config.models.is_empty() {
        return Ok(false);
    }
    Ok(true)
}

/// Whether this exact resolved model can run under the provider's current
/// credential and non-secret configuration.
pub async fn model_is_usable(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    model: &ResolvedModelPolicy,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Result<bool> {
    provider_is_usable(store, secrets, model.provider, policy).await
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
        let available = provider_is_usable(store, secrets, kind, policy).await?;
        models.extend(model_registry::models_for(kind).map(|spec| CatalogModel {
            policy: ResolvedModelPolicy::curated(spec),
            available,
        }));
        // Configured model sets: the compatible endpoint's custom entries,
        // and the managed gateway's entitled snapshot — which is empty on an
        // unmanaged profile, so no gateway row ever reaches the catalog there.
        let configured = match kind {
            ProviderKind::OpenaiCompatible | ProviderKind::Xai => {
                read_config(store, kind).await?.models
            }
            ProviderKind::ModelGateway => gateway_models(store, policy).await?,
            _ => Vec::new(),
        };
        models.extend(configured.iter().map(|model| CatalogModel {
            policy: match kind {
                ProviderKind::ModelGateway => ResolvedModelPolicy::gateway_for(model),
                _ => ResolvedModelPolicy::custom_for(kind, model),
            },
            available,
        }));
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
                ProviderKind::Xai,
                ProviderCredential::api_key("existing-xai-api-key"),
            ),
            (ProviderKind::Openai, ProviderCredential::Oauth {}),
            (
                ProviderKind::Gemini,
                ProviderCredential::ServiceAccount {
                    json: r#"{"type":"service_account","client_email":"service-account@example.test","private_key":"test-private-key","project_id":"test-project"}"#.to_owned(),
                },
            ),
            (
                ProviderKind::Vertex,
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
        assert!(
            write_credential(&secrets, ProviderKind::Xai, &ProviderCredential::Oauth {})
                .await
                .is_err()
        );
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
        assert_eq!(ProviderKind::parse("vertex"), Some(ProviderKind::Vertex));
        assert_eq!(
            ProviderKind::Anthropic.credential_key(),
            "provider.anthropic.credential"
        );
        assert_eq!(ProviderKind::Openai.setting_key(), "provider.openai");
        assert_eq!(ProviderKind::parse("xai"), Some(ProviderKind::Xai));
        assert_eq!(
            ProviderKind::Xai.credential_key(),
            "provider.xai.credential"
        );
    }

    #[test]
    fn a_pre_protocol_gateway_snapshot_keeps_its_anthropic_route() {
        let snapshot: GatewayModelSnapshot = serde_json::from_value(serde_json::json!({
            "gateway_url": "https://gateway.example/",
            "models": [{
                "id": "legacy-claude",
                "context_window": 32768,
                "max_output_tokens": 4096
            }]
        }))
        .expect("the persisted shape from before per-model protocols still loads");

        assert!(snapshot.model_protocols.is_empty());
        assert_eq!(
            snapshot
                .model_protocols
                .get("legacy-claude")
                .copied()
                .unwrap_or_default(),
            GatewayModelProtocol::AnthropicMessages
        );
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
            upstream_id: None,
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
            ..Default::default()
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

    /// The candidate the registry is offered for a deployment-aliased upstream
    /// id. The risk here is over-stripping: every extra rule is another way to
    /// hand one model another model's capabilities, and curated ids contain
    /// dots and digits of their own.
    #[test]
    fn upstream_ids_are_normalized_only_by_region_vendor_and_version() {
        assert_eq!(
            curated_id_candidate("us.anthropic.claude-opus-5"),
            "claude-opus-5"
        );
        assert_eq!(
            curated_id_candidate("anthropic.claude-sonnet-4-5-v1:0"),
            "claude-sonnet-4-5"
        );
        // Not a region prefix, not a version suffix: left exactly as reported.
        assert_eq!(curated_id_candidate("gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(curated_id_candidate("acme.llm-v"), "acme.llm-v");
        assert_eq!(
            curated_id_candidate("us.acme.private-model"),
            "acme.private-model"
        );
        // One region prefix, not repeated stripping.
        assert_eq!(curated_id_candidate("us.eu.model"), "eu.model");
    }

    #[test]
    fn custom_model_validation_is_conservative_and_rejects_duplicates() {
        let valid = CustomModelConfig {
            id: "local/model".into(),
            upstream_id: None,
            display_name: Some("Local Model".into()),
            context_window: 32_768,
            max_output_tokens: 4_096,
            ..Default::default()
        };
        assert!(validate_custom_models(std::slice::from_ref(&valid)).is_ok());
        assert!(validate_custom_models(&[valid.clone(), valid.clone()]).is_err());
        assert!(
            validate_configured_models_against(
                ProviderKind::OpenaiCompatible,
                std::slice::from_ref(&valid),
                |id| id == "local/model"
            )
            .is_err(),
            "custom ids must not shadow a curated id under the same provider"
        );
        assert!(validate_custom_models(&[CustomModelConfig {
            id: "bad model".into(),
            upstream_id: None,
            display_name: None,
            context_window: 32_768,
            max_output_tokens: 4_096,
            ..Default::default()
        }])
        .is_err());
        assert!(validate_custom_models(&[CustomModelConfig {
            id: "bad".into(),
            upstream_id: None,
            display_name: None,
            context_window: 1_000,
            max_output_tokens: 4_096,
            ..Default::default()
        }])
        .is_err());
    }

    #[test]
    fn xai_configured_models_carry_only_supported_capabilities() {
        let model = CustomModelConfig {
            id: "grok-account-model".into(),
            display_name: Some("Grok account model".into()),
            context_window: 500_000,
            max_output_tokens: 32_768,
            input_modalities: vec![InputModality::Text, InputModality::Image],
            supports_reasoning: true,
            reasoning_efforts: vec![
                ReasoningEffort::None,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
                ReasoningEffort::XHigh,
            ],
            ..Default::default()
        };
        validate_configured_models(ProviderKind::Xai, std::slice::from_ref(&model)).unwrap();

        let policy = ResolvedModelPolicy::custom_for(ProviderKind::Xai, &model);
        assert_eq!(policy.provider, ProviderKind::Xai);
        assert_eq!(policy.input_modalities, model.input_modalities);
        assert!(policy.supports_reasoning);
        assert_eq!(policy.reasoning_efforts, model.reasoning_efforts);

        let mut unsupported = model.clone();
        unsupported.reasoning_efforts.push(ReasoningEffort::Max);
        assert!(validate_configured_models(ProviderKind::Xai, &[unsupported]).is_err());
        assert!(validate_configured_models(ProviderKind::OpenaiCompatible, &[model]).is_err());
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
        assert!(config.image_input);

        let vertex = ResolvedModelPolicy::curated(
            model_registry::find_for(ProviderKind::Vertex, "claude-opus-5").unwrap(),
        );
        assert_eq!(vertex.provider, ProviderKind::Vertex);
        assert_eq!(vertex.vendor, Some(ProviderKind::Anthropic));
        assert_eq!(vertex.verification, VerificationTier::Unverified);
        assert!(!vertex.supports_vendor_web_search);

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
        assert!(config.image_input);

        // A custom endpoint keeps the conservative shape: no reasoning, and the
        // requested effort is dropped rather than sent to something that would
        // reject it.
        let mut config = AgentConfig::default();
        let custom = ResolvedModelPolicy::custom_for(
            ProviderKind::OpenaiCompatible,
            &CustomModelConfig {
                id: "local-model".into(),
                upstream_id: None,
                display_name: None,
                context_window: 32_768,
                max_output_tokens: 4_096,
                ..Default::default()
            },
        );
        assert_eq!(custom.verification, VerificationTier::Unverified);
        apply_model_policy(&mut config, &custom, Some(ReasoningEffort::High)).unwrap();
        assert!(!config.image_input);
        assert_eq!(config.provider, Some(ProviderId::new("openai_compatible")));
        assert_eq!(config.context_window, 32_768);
        assert_eq!(config.max_tokens, Some(4_096));
        assert!(!config.reasoning_model);
        assert_eq!(config.reasoning_effort, None);
    }

    #[test]
    fn first_class_vertex_is_global_only_while_legacy_gemini_keeps_regions() {
        assert!(configured_vertex_location_is_usable(
            ProviderKind::Vertex,
            None
        ));
        assert!(configured_vertex_location_is_usable(
            ProviderKind::Vertex,
            Some("global")
        ));
        assert!(!configured_vertex_location_is_usable(
            ProviderKind::Vertex,
            Some("us-east5")
        ));
        assert!(configured_vertex_location_is_usable(
            ProviderKind::Gemini,
            Some("us-east5")
        ));
        assert!(!configured_vertex_location_is_usable(
            ProviderKind::Gemini,
            Some("../global")
        ));
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

    /// Repro: a chat pinned to `openai::gpt-5.6-sol` reached the
    /// OpenAI-compatible `/v1/chat/completions` route with `reasoning_effort`
    /// attached and the vendor refused the request. The free-form turn path
    /// assigned the raw model and effort without applying the registry policy,
    /// so the request carried no provider hint and the router was free to serve
    /// a model the registry already owns from any OpenAI-compatible route.
    #[test]
    fn a_registered_model_keeps_its_policy_on_the_free_form_path() {
        let mut config = AgentConfig::default();
        apply_free_form_model(
            &mut config,
            "openai::gpt-5.6-sol".into(),
            Some(ReasoningEffort::Max),
        )
        .unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("openai")));
        assert_eq!(config.model, "gpt-5.6-sol");
        assert!(config.reasoning_model);
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Max));

        // A bare curated id resolves the same way, and the effort is clamped to
        // what that generation actually takes.
        let mut config = AgentConfig::default();
        apply_free_form_model(&mut config, "gpt-5.5".into(), Some(ReasoningEffort::Max)).unwrap();
        assert_eq!(config.provider, Some(ProviderId::new("openai")));
        assert_eq!(config.reasoning_effort, Some(ReasoningEffort::XHigh));

        // An id the registry does not claim keeps the free-form contract, and
        // its effort stays off the wire because nothing declared the model a
        // reasoning model.
        let mut config = AgentConfig::default();
        apply_free_form_model(
            &mut config,
            "local-model".into(),
            Some(ReasoningEffort::High),
        )
        .unwrap();
        assert_eq!(config.provider, None);
        assert_eq!(config.model, "local-model");
        assert!(!config.reasoning_model);
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
