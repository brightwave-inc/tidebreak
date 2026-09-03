//! Configurable model providers.
//!
//! A provider is `{ kind, enabled, base_url? }` plus a credential held in the
//! [`SecretProvider`](tidebreak_core::SecretProvider) (never the DB). Credentials
//! are a typed JSON blob (`api_key` or `oauth`) so new auth shapes don't force
//! a schema redesign.
//!
//! Non-secret config lives in the [`Store`](tidebreak_core::Store) under
//! `provider.<kind>`. The legacy `provider.anthropic.api_key` secret and the
//! `PUT /settings/api-key` shim still work — they read/write the anthropic
//! credential in the new blob shape.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, HashMap};

use tidebreak_core::{AgentConfig, ProviderId, ReasoningEffort, Result, SecretProvider, Store};

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

/// Internal model selector prefix for one admitted gateway route.
///
/// The suffix binds a deployment-local id to the exact deployment and catalog
/// row that admitted it. The router recognizes the selector only while the
/// current policy-matched snapshot produces the same value, then rewrites it
/// to the local id before egress.
const FROZEN_GATEWAY_MODEL_PREFIX: &str = "__tidebreak_gateway_v1.";

pub(crate) fn is_frozen_gateway_route_model(model: &str) -> bool {
    model.starts_with(FROZEN_GATEWAY_MODEL_PREFIX)
}

/// Whether a resolved policy is safe to use for one already-admitted execution.
///
/// A plain Model Gateway catalog key is durable selection intent, not execution
/// identity: its deployment-local id may be reused for another upstream route
/// after the turn or agent run was accepted. Every registry-enforced execution
/// seam therefore requires the frozen selector minted at admission. Direct
/// providers have no gateway catalog indirection and remain valid as resolved.
pub(crate) fn is_valid_execution_policy(policy: &ResolvedModelPolicy) -> bool {
    policy.provider != ProviderKind::ModelGateway
        || is_frozen_gateway_route_model(&policy.route_model)
}

/// The persisted snapshot of the managed gateway's entitled models, stamped
/// with the deployment it was synced from. The stamp is what keeps the cache
/// honest without coordinated clears: a snapshot synced from one gateway is
/// simply never honored while policy names another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GatewayModelSnapshot {
    /// The normalized gateway base URL the models were fetched from.
    pub(crate) gateway_url: String,
    /// Gateway installation that issued this catalog. URL alone is not enough:
    /// an administrator may replace a deployment in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) installation_id: Option<String>,
    pub(crate) models: Vec<CustomModelConfig>,
    /// Per-model inference protocol. An absent map is a snapshot written by a
    /// gateway generation that served only Anthropic Messages, so lookup
    /// defaults to that protocol for backward compatibility.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) model_protocols: BTreeMap<String, GatewayModelProtocol>,
    /// Per-model reasoning ladders stated by the member catalog. Presence is
    /// significant: an empty list means no effort control, while absence
    /// means an older gateway did not state a ladder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) model_reasoning_efforts: BTreeMap<String, Vec<ReasoningEffort>>,
    /// The member-catalog contract revision the last sync read (`"v1"`),
    /// or `None` when the deployment predates `/api/v1/me/catalog` and the
    /// sync degraded to the per-surface CLI reads. Drives the "older
    /// gateway" note in settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) member_catalog: Option<String>,
    /// The catalog `ETag` of this snapshot, sent as `If-None-Match` on the
    /// next sync so an unchanged catalog costs a `304` instead of a body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) catalog_etag: Option<String>,
}

/// The compatibility protocol a managed gateway uses for one entitled model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GatewayModelProtocol {
    /// Anthropic Messages at `/compat/anthropic/v1/messages`.
    #[default]
    AnthropicMessages,
    /// OpenAI Responses at `/compat/openai/v1/responses` — the only OpenAI
    /// surface a gateway serves northbound. Snapshots written before the
    /// route spoke Responses recorded `openai_chat_completions`; accept the
    /// old spelling rather than dropping the model.
    #[serde(alias = "openai_chat_completions")]
    OpenaiResponses,
}

impl GatewayModelProtocol {
    /// Accept the canonical gateway values plus the short names older
    /// deployments used for the model-list filter.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "anthropic" | "anthropic_messages" => Some(Self::AnthropicMessages),
            "openai" | "openai_compatible" | "openai_chat_completions" | "openai_responses" => {
                Some(Self::OpenaiResponses)
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

/// Find the model-specific reasoning ladder for a gateway-backed engine id.
///
/// Modelctl-generated Grok ids prefix the gateway route with the profile that
/// owns it (`model-gateway-<profile>/...`). OpenCode uses a shorter provider
/// prefix. The snapshot keeps the raw gateway ids, so match those wrappers
/// without assuming that the raw id itself contains no slash.
pub(crate) fn gateway_reasoning_efforts_for_model<'a>(
    snapshot: &'a GatewayModelSnapshot,
    selection: &str,
) -> Option<&'a [ReasoningEffort]> {
    snapshot
        .model_reasoning_efforts
        .iter()
        .find(|(id, _)| gateway_selection_matches(selection, id))
        .map(|(_, efforts)| efforts.as_slice())
}

/// Intersect one gateway model's ladder with the harness surface that carries
/// it. A listed row wins over the engine-wide ladder. Hosted picker rows are
/// empty compat listings, so ignore `adapter.list_models` on that path and
/// intersect only when the picker actually used a Codex row. If the harness
/// did not list the gateway-only row and has no engine-wide ladder, use the
/// gateway's ladder rather than treating the missing row as unsupported.
pub(crate) fn effective_gateway_reasoning_efforts(
    hosted: bool,
    listed_model_efforts: Option<&[ReasoningEffort]>,
    engine_efforts: &[ReasoningEffort],
    gateway_efforts: &[ReasoningEffort],
) -> Vec<ReasoningEffort> {
    let listed_model_efforts = if hosted { None } else { listed_model_efforts };
    let harness_efforts = listed_model_efforts.unwrap_or(engine_efforts);
    if harness_efforts.is_empty() && listed_model_efforts.is_none() {
        return gateway_efforts.to_vec();
    }
    harness_efforts
        .iter()
        .copied()
        .filter(|effort| gateway_efforts.contains(effort))
        .collect()
}

fn gateway_selection_matches(selection: &str, id: &str) -> bool {
    selection == id
        || selection
            .strip_prefix("model-gateway/")
            .is_some_and(|raw| raw == id)
        || selection
            .strip_prefix("anthropic/")
            .is_some_and(|raw| raw == id)
        || selection
            .strip_prefix("model-gateway-")
            .and_then(|qualified| qualified.split_once('/'))
            .is_some_and(|(_, raw)| raw == id)
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

/// Convert a member catalog (`/api/v1/me/catalog`) into snapshot rows.
///
/// One conversion for both consumers — the deployment-wide managed sync and
/// the per-caller hosted fetch (decision 62) — so a model a hosted caller is
/// offered is shaped exactly like a model the sync would have stored.
pub(crate) struct MemberCatalogModels {
    pub(crate) models: Vec<CustomModelConfig>,
    pub(crate) model_protocols: BTreeMap<String, GatewayModelProtocol>,
    pub(crate) model_reasoning_efforts: BTreeMap<String, Vec<ReasoningEffort>>,
}

pub(crate) fn member_catalog_models(
    catalog: crate::connectors::GatewayCatalog,
) -> MemberCatalogModels {
    let mut model_protocols = BTreeMap::new();
    let mut model_reasoning_efforts = BTreeMap::new();
    let models = catalog
        .models
        .into_iter()
        .filter_map(|model| {
            let protocols: Vec<_> = model
                .protocols
                .iter()
                .filter_map(|protocol| GatewayModelProtocol::parse(protocol))
                .collect();
            // A dual-protocol model routes through Anthropic Messages, the
            // richer adapter; a model with no protocol this client speaks is
            // skipped, exactly as on the older surface.
            let protocol = if protocols.contains(&GatewayModelProtocol::AnthropicMessages) {
                GatewayModelProtocol::AnthropicMessages
            } else {
                *protocols.first()?
            };
            let id = model.id;
            model_protocols.insert(id.clone(), protocol);
            if let Some(efforts) = model.supported_reasoning_efforts {
                model_reasoning_efforts.insert(id.clone(), efforts.clone());
                for alias in &model.aliases {
                    model_reasoning_efforts.insert(alias.clone(), efforts.clone());
                }
            }
            Some(CustomModelConfig {
                id,
                display_name: Some(model.name),
                // The catalog reports gateway-id aliases instead of the
                // provider-side id; both exist to match a curated row.
                upstream_id: None,
                aliases: model.aliases,
                context_window: clamp_u32(model.context_window, 32_768),
                max_output_tokens: clamp_u32(model.max_output_tokens, 4_096),
                input_modalities: vec![crate::model_registry::InputModality::Text],
                supports_reasoning: false,
                reasoning_efforts: Vec::new(),
            })
        })
        .collect();
    MemberCatalogModels {
        models,
        model_protocols,
        model_reasoning_efforts,
    }
}

/// Clamp a gateway-reported limit into the u32 the config carries; zero and
/// out-of-range values fall back to the default.
pub(crate) fn clamp_u32(value: Option<i64>, default: u32) -> u32 {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

/// The entitled models of the gateway serving the current caller: the managed
/// deployment-wide snapshot, or the caller's own snapshot on a hosted machine
/// (decision 62). Empty for an unmanaged profile with no caller snapshot, a
/// misconfigured policy, and a snapshot stamped by a different deployment.
pub(crate) async fn gateway_models(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<Vec<CustomModelConfig>> {
    Ok(gateway_snapshot_for(store, policy, caller_gateway)
        .await?
        .map(|snapshot| snapshot.models)
        .unwrap_or_default())
}

/// The gateway snapshot the current caller's surfaces read: the managed
/// deployment-wide snapshot, or — on a gateway-authenticated hosted machine —
/// the caller's own entitlements, resolved per request and held only in
/// process memory (decision 62).
pub(crate) async fn gateway_snapshot_for(
    store: &dyn Store,
    policy: &crate::managed_policy::ManagedPolicy,
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<Option<GatewayModelSnapshot>> {
    if policy.managed {
        return gateway_snapshot_for_policy(store, policy).await;
    }
    Ok(caller_gateway.cloned())
}

pub(crate) async fn gateway_snapshot_for_policy(
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
            installation_id: None,
            models: row.models,
            model_protocols: BTreeMap::new(),
            model_reasoning_efforts: BTreeMap::new(),
            member_catalog: None,
            catalog_etag: None,
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
    /// Fireworks AI's hosted OpenAI-compatible Chat Completions API.
    Fireworks,
    /// Together AI's hosted OpenAI-compatible Chat Completions API.
    Together,
    /// OpenRouter's hosted OpenAI-compatible Chat Completions API.
    Openrouter,
    /// A local Ollama daemon over the shared OpenAI-compatible transport.
    Ollama,
    /// Any OpenAI-compatible Chat Completions gateway (vLLM, LM Studio, …).
    OpenaiCompatible,
    /// A signed-in model-gateway deployment: entitled models synced from the
    /// gateway, inference through each model's Anthropic- or OpenAI-compatible
    /// surface with short-lived OAuth tokens.
    ModelGateway,
}

impl ProviderKind {
    /// All kinds the server knows about, in display order.
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::Openai,
        ProviderKind::Anthropic,
        ProviderKind::Xai,
        ProviderKind::Gemini,
        ProviderKind::Fireworks,
        ProviderKind::Together,
        ProviderKind::Openrouter,
        ProviderKind::Ollama,
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
            ProviderKind::Fireworks => "fireworks",
            ProviderKind::Together => "together",
            ProviderKind::Openrouter => "openrouter",
            ProviderKind::Ollama => "ollama",
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
            "fireworks" => Some(Self::Fireworks),
            "together" => Some(Self::Together),
            "openrouter" => Some(Self::Openrouter),
            "ollama" => Some(Self::Ollama),
            "openai_compatible" => Some(Self::OpenaiCompatible),
            "model_gateway" => Some(Self::ModelGateway),
            _ => None,
        }
    }

    /// Default API root. Hosted presets fix this; Ollama uses it when the
    /// reader has not pointed the card at another daemon.
    pub const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Fireworks => Some("https://api.fireworks.ai/inference/v1"),
            Self::Together => Some("https://api.together.ai/v1"),
            Self::Openrouter => Some("https://openrouter.ai/api/v1"),
            Self::Ollama => Some("http://127.0.0.1:11434/v1"),
            _ => None,
        }
    }

    /// Whether [`default_base_url`] is the only legal endpoint.
    pub const fn has_fixed_endpoint(self) -> bool {
        matches!(self, Self::Fireworks | Self::Together | Self::Openrouter)
    }

    /// Whether the reader can register model rows beside the endpoint.
    pub const fn accepts_configured_models(self) -> bool {
        matches!(
            self,
            Self::OpenaiCompatible | Self::Xai | Self::Openrouter | Self::Ollama
        )
    }

    /// Whether a stored or env credential is required before the route is
    /// usable. Local Ollama accepts unauthenticated requests.
    pub const fn requires_credential(self) -> bool {
        !matches!(self, Self::Ollama)
    }

    /// Environment variable consulted for this kind's base URL when the
    /// deployment plane holds none.
    ///
    /// Only kinds that accept a stored base URL have one. Hosted presets and
    /// the first-party Gemini and xAI endpoints are fixed, so no variable can
    /// redirect them.
    pub const fn base_url_env_var(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("ANTHROPIC_BASE_URL"),
            Self::Openai => Some("OPENAI_BASE_URL"),
            Self::Ollama => Some("OLLAMA_BASE_URL"),
            Self::OpenaiCompatible => Some("OPENAI_COMPATIBLE_BASE_URL"),
            Self::Xai
            | Self::Gemini
            | Self::Fireworks
            | Self::Together
            | Self::Openrouter
            | Self::ModelGateway => None,
        }
    }

    /// The URL this kind actually calls: a stored override, else the
    /// environment fallback, else the default, with hosted presets ignoring
    /// both.
    pub fn effective_base_url(self, configured: Option<&str>) -> Option<String> {
        self.effective_base_url_from(configured, |name| std::env::var(name).ok())
    }

    /// [`effective_base_url`](Self::effective_base_url) against an injected
    /// environment, so tests can exercise the fallback without mutating the
    /// process environment.
    fn effective_base_url_from(
        self,
        configured: Option<&str>,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        if self.has_fixed_endpoint() {
            return self.default_base_url().map(str::to_owned);
        }
        configured
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .or_else(|| self.env_base_url_from(lookup))
            .or_else(|| self.default_base_url().map(str::to_owned))
    }

    /// The environment fallback for this kind's base URL.
    ///
    /// A value that is empty, unparseable, or that this kind may not call over
    /// the offered transport is ignored, exactly as an empty API-key variable
    /// is: the deployment falls back to the built-in default rather than
    /// failing the boot or dropping the route.
    fn env_base_url_from(self, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
        let url = lookup(self.base_url_env_var()?)?;
        if url.is_empty() {
            return None;
        }
        // A kind that needs no credential may be reached over loopback HTTP,
        // matching what the deployment plane accepts for a stored value.
        base_url_is_allowed(&url, !self.requires_credential()).then_some(url)
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
                    | ProviderKind::Xai
                    | ProviderKind::Gemini
                    | ProviderKind::Fireworks
                    | ProviderKind::Together
                    | ProviderKind::Openrouter
                    | ProviderKind::Ollama
                    | ProviderKind::OpenaiCompatible
            ),
            ProviderCredential::Oauth {} => self == ProviderKind::Openai,
        }
    }
}

fn base_url_is_allowed(base: &str, allow_credentialless_loopback_http: bool) -> bool {
    let Ok(url) = url::Url::parse(base) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    let allowed = match url.scheme() {
        "https" => true,
        "http" if allow_credentialless_loopback_http => match url.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        },
        _ => false,
    };
    if allowed {
        return true;
    }

    #[cfg(test)]
    return test_loopback_provider_urls()
        .lock()
        .unwrap()
        .contains(url.as_str());

    #[cfg(not(test))]
    false
}

#[cfg(test)]
fn test_loopback_provider_urls() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static URLS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    URLS.get_or_init(Default::default)
}

/// Admit one exact loopback HTTP endpoint used by an in-process provider stub.
///
/// This exists only in crate tests. Production builds cannot register an
/// exception, and provider-update tests exercise the production validator
/// unless they explicitly opt a URL into this narrow seam first.
#[cfg(test)]
pub(crate) fn allow_test_loopback_provider_base_url(base: &str) {
    let url = url::Url::parse(base).expect("test provider base URL must parse");
    assert_eq!(url.scheme(), "http", "test seam is only for cleartext HTTP");
    assert!(
        match url.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        },
        "test seam is only for loopback endpoints"
    );
    assert!(url.username().is_empty() && url.password().is_none() && url.fragment().is_none());
    test_loopback_provider_urls()
        .lock()
        .unwrap()
        .insert(url.to_string());
}

fn validate_base_url_transport(
    kind: ProviderKind,
    base: &str,
    has_reusable_credential: bool,
) -> std::result::Result<(), ServerError> {
    let allow_credentialless_loopback_http =
        !kind.requires_credential() && !has_reusable_credential;
    if base_url_is_allowed(base, allow_credentialless_loopback_http) {
        return Ok(());
    }
    Err(ServerError::bad_request(
        if allow_credentialless_loopback_http {
            "base_url must use HTTPS, or HTTP on a loopback address for a credentialless provider"
        } else {
            "base_url must use HTTPS when provider credentials are present or required"
        },
    ))
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
    /// in its own keychain entry managed by `crate::connectors`, so the
    /// rotating material never rides the provider settings surface.
    Oauth {},
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
        }
    }

    /// Validate a credential before it is written to the keychain.
    ///
    /// Error messages name only malformed fields, never their values.
    pub fn validate(&self) -> std::result::Result<(), ServerError> {
        match self {
            Self::ApiKey { key } if key.is_empty() => {
                Err(ServerError::bad_request("credential key must not be empty"))
            }
            Self::ApiKey { .. } | Self::Oauth {} => Ok(()),
        }
    }
}

// Redact key material in Debug.
impl std::fmt::Debug for ProviderCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApiKey { .. } => f.debug_struct("ApiKey").field("key", &"***").finish(),
            Self::Oauth {} => f.debug_struct("Oauth").finish(),
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
    /// Explicit model rows served by a configurable endpoint.
    ///
    /// OpenAI-compatible endpoints and extra xAI account models can change
    /// independently of an Tidebreak release, so their configured rows live
    /// beside the endpoint. Other curated providers ignore this field and
    /// obtain their models from the host registry.
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
    /// Alternate gateway ids that also resolve to this model — the
    /// deployment-shaped spellings the member catalog reports, offered to
    /// the curated registry the same way `upstream_id` is.
    ///
    /// Populated only by gateway model sync from a catalog-serving gateway;
    /// a user-entered custom model leaves it empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Context limit used by Tidebreak's reducer.
    #[serde(default = "default_custom_context_window")]
    pub context_window: u32,
    /// Maximum output sent to the endpoint.
    #[serde(default = "default_custom_max_output_tokens")]
    pub max_output_tokens: u32,
    /// Inputs Tidebreak may place on this model's request.
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
            aliases: Vec::new(),
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
    /// Host-side model selector placed on the request.
    ///
    /// Usually the same as `id`. A frozen gateway execution uses an internal
    /// selector here so the router can prove the current catalog still names
    /// the admitted route before rewriting it to `id` for the wire.
    pub route_model: String,
    /// Canonical provider model identity used for version-sensitive request
    /// shaping. Gateway-local aliases keep their own [`Self::id`] on the wire
    /// while inheriting this identity only from an unambiguous curated match.
    pub request_shaping_model: String,
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
    /// Whether a picker shows this model without being asked for the full
    /// catalog. Curation, not capability — see [`ModelSpec::recommended`].
    ///
    /// Only the static catalog carries a curation stance. A custom endpoint or
    /// a gateway entitlement is something the reader configured deliberately,
    /// so it is recommended by construction; the curated row's flag is not
    /// inherited over those routes any more than its limits are.
    pub recommended: bool,
    /// Runtime context reduction limit.
    pub context_window: u32,
    /// Runtime output cap.
    pub max_output_tokens: u32,
    /// End-to-end supported input modalities.
    pub input_modalities: Vec<InputModality>,
    /// Whether this exact provider/model route accepts function tools.
    pub supports_tools: bool,
    /// Whether this exact provider/model route can enforce a strict structured
    /// response schema.
    pub supports_structured_output: bool,
    /// Whether the provider request uses a reasoning-model shape.
    pub supports_reasoning: bool,
    /// Whether a turn on this model may run the provider's own server-side web
    /// search instead of the host's. Asserts that the routing adapter emits the
    /// vendor tool, so it is never inherited by a pass-through route.
    pub supports_vendor_web_search: bool,
    /// Whether the host may search on this model's behalf with a dedicated,
    /// tool-free sub-request to its provider.
    ///
    /// Separate from [`Self::supports_vendor_web_search`] because it is a
    /// different call: the host issues it, bounds it by counting its own calls,
    /// and never continues it. See
    /// [`ModelSpec::supports_search_subrequest`](crate::model_registry::ModelSpec::supports_search_subrequest).
    /// Never inherited by a pass-through route, for the same reason the vendor
    /// flag is not — the claim is about the adapter, not the vendor.
    pub supports_search_subrequest: bool,
    /// The reasoning-effort levels this model accepts, ascending. Empty when
    /// the model takes no effort control.
    pub reasoning_efforts: Vec<ReasoningEffort>,
}

impl ResolvedModelPolicy {
    /// Provider-qualified selector persisted for one accepted execution.
    ///
    /// Direct routes return the ordinary catalog key. Gateway routes return a
    /// frozen host selector that the router rewrites to `id` only after the
    /// current deployment proves it still owns the same route.
    pub(crate) fn execution_key(&self) -> String {
        model_registry::selection_key(self.provider, &self.route_model)
    }

    pub(crate) fn curated(spec: &ModelSpec) -> Self {
        Self {
            key: model_registry::selection_key(spec.provider, spec.id),
            id: spec.id.to_owned(),
            route_model: spec.id.to_owned(),
            request_shaping_model: spec.id.to_owned(),
            display_name: spec.display_name.to_owned(),
            provider: spec.provider,
            vendor: spec.vendor(),
            verification: spec.verification,
            recommended: spec.recommended,
            context_window: spec.context_window,
            max_output_tokens: spec.max_output_tokens,
            input_modalities: spec.input_modalities.to_vec(),
            supports_tools: spec.supports_tools(),
            supports_structured_output: spec.supports_structured_output(),
            supports_reasoning: spec.supports_reasoning,
            supports_vendor_web_search: spec.supports_vendor_web_search,
            supports_search_subrequest: spec.supports_search_subrequest(),
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
        let Some(spec) = gateway_curated_spec(model) else {
            return policy;
        };
        policy.display_name = spec.display_name.to_owned();
        policy.request_shaping_model = spec.id.to_owned();
        policy.vendor = Some(spec.provider);
        policy.verification = spec.verification;
        policy.input_modalities = spec.input_modalities.to_vec();
        policy.supports_tools = spec.supports_tools();
        policy.supports_structured_output = spec.supports_structured_output();
        policy.supports_reasoning = spec.supports_reasoning;
        policy.reasoning_efforts = spec.reasoning_efforts.to_vec();
        policy
    }

    fn custom_for(provider: ProviderKind, model: &CustomModelConfig) -> Self {
        let first_party_xai = provider == ProviderKind::Xai;
        Self {
            key: model_registry::selection_key(provider, &model.id),
            id: model.id.clone(),
            route_model: model.id.clone(),
            request_shaping_model: model.id.clone(),
            display_name: model
                .display_name
                .clone()
                .unwrap_or_else(|| model_registry::display_name_for(&model.id)),
            provider,
            vendor: None,
            verification: VerificationTier::Unverified,
            recommended: true,
            context_window: model.context_window,
            max_output_tokens: model.max_output_tokens,
            input_modalities: if first_party_xai {
                model.input_modalities.clone()
            } else {
                vec![InputModality::Text]
            },
            // Preserve the existing custom-compatible contract: users register
            // these endpoints specifically to run Tidebreak's agent loop.
            supports_tools: true,
            // An arbitrary compatible endpoint may accept `response_format`
            // while ignoring its schema. Without an explicit route contract,
            // utility work must not depend on that response being enforced.
            supports_structured_output: false,
            supports_reasoning: first_party_xai && model.supports_reasoning,
            // A pass-through endpoint cannot promise a provider-executed
            // search, whatever upstream model it is serving — the request
            // shape that would enable one is the vendor's own, and this route
            // does not speak it. Deliberately not inherited by `gateway_for`.
            supports_vendor_web_search: false,
            // Same reasoning, same answer: a search sub-request is the vendor's
            // own request shape, which this route does not speak.
            supports_search_subrequest: false,
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
                aliases: Vec::new(),
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

/// The curated direct-vendor model represented by one gateway entitlement.
///
/// Gateways are free to use deployment-local ids, so equality on `model.id`
/// is not enough. Sync supplies upstream aliases for exactly this purpose;
/// the same deliberately narrow normalization that attributes capabilities
/// also identifies a safe route-equivalent migration target.
fn gateway_curated_spec(model: &CustomModelConfig) -> Option<&'static ModelSpec> {
    let stronger: Vec<&str> = model
        .upstream_id
        .as_deref()
        .into_iter()
        .chain(model.aliases.iter().map(String::as_str))
        .collect();
    let mut recognized = stronger.iter().filter_map(|candidate| {
        model_registry::find(candidate)
            .or_else(|| model_registry::find(&curated_id_candidate(candidate)))
    });
    let first = recognized.next();
    if recognized.any(|candidate| Some(candidate) != first) {
        return None;
    }
    if let Some(spec) = first {
        if let Some(local) = model_registry::find(&model.id) {
            if local != spec {
                return None;
            }
        }
        return Some(spec);
    }
    if stronger.is_empty() {
        model_registry::find(&model.id)
    } else {
        None
    }
}

#[derive(Serialize)]
struct GatewayRouteFingerprint<'a> {
    id: &'a str,
    upstream_id: &'a Option<String>,
    aliases: Vec<&'a str>,
    context_window: u32,
    max_output_tokens: u32,
    input_modalities: &'a [InputModality],
    supports_reasoning: bool,
    reasoning_efforts: &'a [ReasoningEffort],
    protocol: GatewayModelProtocol,
    canonical_key: Option<String>,
}

fn gateway_digest(value: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(value))
}

fn gateway_route_digest(model: &CustomModelConfig, protocol: GatewayModelProtocol) -> String {
    let mut aliases: Vec<_> = model.aliases.iter().map(String::as_str).collect();
    aliases.sort_unstable();
    let fingerprint = GatewayRouteFingerprint {
        id: &model.id,
        upstream_id: &model.upstream_id,
        aliases,
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        input_modalities: &model.input_modalities,
        supports_reasoning: model.supports_reasoning,
        reasoning_efforts: &model.reasoning_efforts,
        protocol,
        canonical_key: gateway_curated_spec(model)
            .map(|spec| model_registry::selection_key(spec.provider, spec.id)),
    };
    gateway_digest(&serde_json::to_vec(&fingerprint).expect("gateway fingerprint serializes"))
}

fn frozen_gateway_route_model(
    snapshot: &GatewayModelSnapshot,
    model: &CustomModelConfig,
    protocol: GatewayModelProtocol,
) -> String {
    let deployment = gateway_digest(snapshot.gateway_url.as_bytes());
    let installation = gateway_digest(snapshot.installation_id.as_deref().unwrap_or("").as_bytes());
    let route = gateway_route_digest(model, protocol);
    format!("{FROZEN_GATEWAY_MODEL_PREFIX}{deployment}.{installation}.{route}")
}

fn freeze_gateway_policy(
    snapshot: &GatewayModelSnapshot,
    model: &CustomModelConfig,
) -> ResolvedModelPolicy {
    let protocol = snapshot
        .model_protocols
        .get(&model.id)
        .copied()
        .unwrap_or_default();
    let route_model = frozen_gateway_route_model(snapshot, model, protocol);
    let mut policy = ResolvedModelPolicy::gateway_for(model);
    policy.route_model = route_model;
    policy
}

pub(crate) fn gateway_execution_policy(
    snapshot: &GatewayModelSnapshot,
    selection: &str,
) -> Option<ResolvedModelPolicy> {
    let (provider, id) = model_registry::parse_selection_key(selection)?;
    if provider != ProviderKind::ModelGateway {
        return None;
    }
    if id.starts_with(FROZEN_GATEWAY_MODEL_PREFIX) {
        return resolve_frozen_gateway_policy(snapshot, id);
    }
    snapshot
        .models
        .iter()
        .find(|model| model.id == id)
        .map(|model| freeze_gateway_policy(snapshot, model))
}

fn resolve_frozen_gateway_policy(
    snapshot: &GatewayModelSnapshot,
    route_model: &str,
) -> Option<ResolvedModelPolicy> {
    let suffix = route_model.strip_prefix(FROZEN_GATEWAY_MODEL_PREFIX)?;
    let mut parts = suffix.split('.');
    let deployment = parts.next()?;
    let installation = parts.next()?;
    let route = parts.next()?;
    if parts.next().is_some()
        || deployment != gateway_digest(snapshot.gateway_url.as_bytes())
        || installation
            != gateway_digest(snapshot.installation_id.as_deref().unwrap_or("").as_bytes())
    {
        return None;
    }
    snapshot.models.iter().find_map(|model| {
        let protocol = snapshot
            .model_protocols
            .get(&model.id)
            .copied()
            .unwrap_or_default();
        (route == gateway_route_digest(model, protocol))
            .then(|| freeze_gateway_policy(snapshot, model))
    })
}

/// The one entitled gateway route equivalent to a curated selection.
///
/// `None` means either no equivalent exists or the catalog is ambiguous. A
/// caller must leave the saved selection unresolved in both cases so the
/// reader chooses deliberately instead of Tidebreak guessing between routes.
pub(crate) async fn unique_gateway_equivalent(
    store: &dyn Store,
    snapshot: &GatewayModelSnapshot,
    selection: &str,
) -> Result<Option<ResolvedModelPolicy>> {
    let spec = if let Some((provider, id)) = model_registry::parse_selection_key(selection) {
        if provider == ProviderKind::ModelGateway {
            return Ok(None);
        }
        model_registry::find_for(provider, id)
    } else {
        // Pre-provider-selection builds persisted bare curated ids. Only the
        // unique owner accepted by normal selection resolution is safe to
        // migrate. Configured models participate in that ownership check, so
        // a custom endpoint shadowing a curated id leaves the old selection
        // unresolved instead of silently choosing the curated gateway route.
        let mut owners = bare_model_owners(store, selection).await?;
        if owners.len() != 1 {
            return Ok(None);
        }
        let owner = owners.pop().expect("one owner was checked above");
        model_registry::find_for(owner.provider, &owner.id)
    };
    let Some(spec) = spec else { return Ok(None) };
    let mut matches = snapshot
        .models
        .iter()
        .filter(|model| gateway_curated_spec(model).is_some_and(|candidate| candidate == spec))
        .map(|model| freeze_gateway_policy(snapshot, model));
    let first = matches.next();
    Ok(match (first, matches.next()) {
        (Some(key), None) => Some(key),
        _ => None,
    })
}

/// Resolve one bare model id under the same ownership rule used by the public
/// selection resolver.
///
/// Curated direct-provider rows and configured model providers are all owners.
/// Unknown ids and ids claimed by more than one owner remain unresolved; a
/// caller must not infer a route for either case.
async fn bare_model_owners(store: &dyn Store, value: &str) -> Result<Vec<ResolvedModelPolicy>> {
    let mut owners = model_registry::find(value)
        .map(ResolvedModelPolicy::curated)
        .into_iter()
        .collect::<Vec<_>>();
    for provider in [
        ProviderKind::Xai,
        ProviderKind::Openrouter,
        ProviderKind::Ollama,
        ProviderKind::OpenaiCompatible,
    ] {
        let config = read_config(store, provider).await?;
        owners.extend(
            config
                .models
                .iter()
                .filter(|model| model.id == value)
                .map(|model| ResolvedModelPolicy::custom_for(provider, model)),
        );
    }
    Ok(owners)
}

/// Drop a trailing `-v<digits>` or `-v<digits>:<digits>` — the revision marker
/// some hosted deployments append. Left alone when the tail is not exactly
/// that shape.
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
    config.model = policy.route_model.clone();
    config.reasoning_model = policy.supports_reasoning;
    config.image_input = policy.input_modalities.contains(&InputModality::Image);
    config.tools_supported = policy.supports_tools;
    config.context_window = usize::try_from(policy.context_window)
        .map_err(|_| tidebreak_core::AgentError::config("model context window is unsupported"))?;
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
//
// Also read back by the CLI through [`crate::wire`], so it rejects unknown
// keys the way the renderer's guards do. A plain comment, not a doc comment,
// so the generated `wire.ts` does not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ProviderInfo {
    /// Provider kind.
    pub kind: ProviderKind,
    /// Whether the provider is enabled for routing.
    pub enabled: bool,
    /// Configured base URL, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_url: Option<String>,
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
}

impl ProviderAuthMode {
    /// The wire spelling, for a client that prints the mode without a serde
    /// round trip. Pinned to the serde form by a test in [`crate::wire`].
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderAuthMode::ApiKey => "api_key",
            ProviderAuthMode::Chatgpt => "chatgpt",
        }
    }
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
    #[serde(default)]
    pub credential: Option<ProviderCredential>,
    /// Replacement configured-model list. Valid for `openai_compatible`,
    /// OpenRouter, Ollama, and first-party xAI.
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
        // Prefer the typed blob. Legacy bare API-key values remain readable,
        // but an object/array/JSON value belongs to the typed format and must
        // never be transmitted verbatim when this version cannot decode it.
        if let Ok(cred) = serde_json::from_str::<ProviderCredential>(&raw) {
            return Ok(Some(cred));
        }
        if looks_like_structured_credential(&raw) {
            return Err(tidebreak_core::AgentError::config(format!(
                "stored {kind} credential is unreadable"
            )));
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

fn looks_like_structured_credential(raw: &str) -> bool {
    let trimmed = raw.trim_start();
    matches!(
        trimmed.as_bytes().first(),
        Some(b'{') | Some(b'[') | Some(b'"')
    )
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
            .delete_secret(crate::connectors::CHATGPT_SECRET_KEY)
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
            .delete_secret(crate::connectors::CHATGPT_SECRET_KEY)
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
            if kind == ProviderKind::Openai && matches!(credential, ProviderCredential::Oauth {}) {
                return crate::connectors::has_stored_chatgpt_credentials(secrets).await;
            }
            return false;
        }
        Err(_) => return false,
        Ok(None) => {}
    }
    if kind == ProviderKind::ModelGateway {
        // The gateway's credential is its stored OAuth session, not a key.
        return crate::connectors::has_stored_credentials(secrets).await;
    }
    env_api_key(kind).is_some()
}

async fn auth_mode_for(
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
) -> Option<ProviderAuthMode> {
    match read_credential(secrets, kind).await.ok().flatten() {
        Some(ProviderCredential::Oauth {})
            if kind == ProviderKind::Openai
                && crate::connectors::has_stored_chatgpt_credentials(secrets).await =>
        {
            Some(ProviderAuthMode::Chatgpt)
        }
        Some(ProviderCredential::ApiKey { key })
            if kind == ProviderKind::Openai && !key.is_empty() =>
        {
            Some(ProviderAuthMode::ApiKey)
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
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<Vec<ProviderInfo>> {
    let mut out = Vec::with_capacity(ProviderKind::ALL.len());
    for &kind in ProviderKind::ALL {
        if kind == ProviderKind::ModelGateway {
            if !policy.managed {
                // A hosted machine reports the caller's own gateway: present
                // exactly when their entitlement snapshot resolved, with
                // their models (decision 62). Absent on every other
                // unmanaged profile.
                let Some(snapshot) = caller_gateway else {
                    continue;
                };
                out.push(ProviderInfo {
                    kind,
                    enabled: true,
                    base_url: policy
                        .hosted_gateway_url
                        .clone()
                        .or_else(|| Some(snapshot.gateway_url.clone())),
                    has_credential: true,
                    auth_mode: None,
                    models: snapshot.models.clone(),
                });
                continue;
            }
            out.push(ProviderInfo {
                kind,
                enabled: policy.gateway_url.is_some(),
                base_url: policy.gateway_url.clone(),
                // Deployment-matched: a session a re-point superseded must
                // not read as this deployment's credential.
                has_credential: match policy.gateway_url.as_deref() {
                    Some(gateway_url) => {
                        crate::connectors::has_stored_credentials_for(secrets, gateway_url).await
                    }
                    None => false,
                },
                auth_mode: None,
                models: gateway_models(store, policy, caller_gateway).await?,
            });
            continue;
        }
        let config = read_config(store, kind).await?;
        out.push(ProviderInfo {
            kind,
            enabled: config.enabled,
            base_url: kind.effective_base_url(config.base_url.as_deref()),
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
    provisioned_policy: &dyn crate::managed_policy::ProvisionedPolicySource,
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
    let policy = crate::managed_policy::resolve(provisioned_policy, os_policy)?;
    if policy.managed && (update.credential.is_some() || update.base_url.is_some()) {
        return Err(managed_profile_refusal(format!(
            "this profile is managed by a model gateway; {kind} credentials and endpoints are locked"
        )));
    }

    let base_url_changed = update.base_url.is_some();
    let credential_changed = update.credential.is_some();
    if let Some(credential) = update.credential.as_ref() {
        credential.validate()?;
        if !kind.accepts_credential(credential) {
            return Err(ServerError::bad_request(format!(
                "{kind} does not support this credential type"
            )));
        }
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
        Some(_) if kind.has_fixed_endpoint() => {
            return Err(ServerError::bad_request(format!(
                "{kind} uses a fixed provider endpoint"
            )));
        }
        Some(None) => config.base_url = None,
        Some(Some(url)) => {
            if url.is_empty() {
                return Err(ServerError::bad_request("base_url must not be empty"));
            }
            config.base_url = Some(url);
        }
    }
    if let Some(models) = update.models {
        if !kind.accepts_configured_models() {
            return Err(ServerError::bad_request(format!(
                "configured models are not supported by {kind}"
            )));
        }
        validate_configured_models(kind, &models)?;
        config.models = models;
    }

    // openai_compatible needs a base URL to be useful when enabled. The
    // environment fallback counts, so a deployment that supplies the endpoint
    // through its environment can still enable the provider.
    if kind == ProviderKind::OpenaiCompatible
        && config.enabled
        && kind
            .effective_base_url(config.base_url.as_deref())
            .is_none()
    {
        return Err(ServerError::bad_request(
            "openai_compatible requires a base_url when enabled",
        ));
    }
    if config.enabled || base_url_changed || credential_changed {
        if let Some(base) = kind
            .effective_base_url(config.base_url.as_deref())
            .as_deref()
        {
            let has_reusable_credential =
                update.credential.is_some() || has_credential(secrets, kind).await;
            validate_base_url_transport(kind, base, has_reusable_credential)?;
        }
    }
    if let Some(credential) = update.credential {
        write_credential(secrets, kind, &credential).await?;
        // Storing a credential is an intent to use the provider — same as
        // ChatGPT sign-in completion and the Anthropic legacy key route.
        // An explicit Enabled toggle still turns the provider off afterward
        // without clearing the credential; only a bare enable/disable write
        // leaves `enabled` alone.
        config.enabled = true;
    }

    write_config(store, kind, &config).await?;

    Ok(ProviderInfo {
        kind,
        enabled: config.enabled,
        base_url: kind.effective_base_url(config.base_url.as_deref()),
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
        const MAX_MODEL_ALIASES: usize = 16;
        if model.aliases.len() > MAX_MODEL_ALIASES {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` carries more than {MAX_MODEL_ALIASES} aliases"
            )));
        }
        // Aliases are only ever offered to the curated registry, but they are
        // gateway-supplied, so they are held to the id grammar all the same.
        if model.aliases.iter().any(|alias| {
            alias.is_empty()
                || alias.chars().count() > MAX_MODEL_ID_CHARS
                || alias.chars().any(char::is_whitespace)
                || alias.chars().any(char::is_control)
        }) {
            return Err(ServerError::bad_request(format!(
                "configured model `{id}` carries an alias that is empty, oversized, or contains whitespace or control characters"
            )));
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
        ProviderKind::Fireworks => std::env::var("FIREWORKS_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Together => std::env::var("TOGETHER_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Openrouter => std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|k| !k.is_empty()),
        ProviderKind::Ollama | ProviderKind::OpenaiCompatible => None,
        // Gateway tokens rotate; they are supplied per request by the route's
        // token source, never resolved into a static key.
        ProviderKind::ModelGateway => None,
    }
}

/// Map a server [`ProviderKind`] to the router's [`tidebreak_router::RouteKind`].
pub fn route_kind(kind: ProviderKind) -> tidebreak_router::RouteKind {
    match kind {
        ProviderKind::Anthropic => tidebreak_router::RouteKind::Anthropic,
        ProviderKind::Openai => tidebreak_router::RouteKind::Openai,
        ProviderKind::Xai => tidebreak_router::RouteKind::Xai,
        ProviderKind::Gemini => tidebreak_router::RouteKind::Gemini,
        ProviderKind::Fireworks => tidebreak_router::RouteKind::Fireworks,
        ProviderKind::Together => tidebreak_router::RouteKind::Together,
        ProviderKind::Openrouter => tidebreak_router::RouteKind::Openrouter,
        ProviderKind::Ollama => tidebreak_router::RouteKind::Ollama,
        ProviderKind::OpenaiCompatible => tidebreak_router::RouteKind::OpenaiCompatible,
        ProviderKind::ModelGateway => tidebreak_router::RouteKind::ModelGateway,
    }
}

/// A hosted caller's gateway path: their rotating inference credential, the
/// gateway base it authenticates against, and their own entitlement snapshot
/// (decision 62). Resolved per caller and held only in process memory.
pub struct OnBehalfOfGateway {
    /// The caller's exchanged inference credential (decision 51).
    pub source: std::sync::Arc<dyn tidebreak_router::BearerTokenSource>,
    /// The gateway base URL the compat routes are joined below.
    pub gateway_base_url: String,
    /// The caller's own entitled models, from their member catalog.
    pub snapshot: GatewayModelSnapshot,
}

/// Collect enabled, credentialed routes for the composite router.
///
/// A kind with no usable credential is skipped. OpenAI ChatGPT OAuth
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
///
/// `on_behalf_of` is a hosted machine's per-caller gateway path (decisions 51
/// and 62): the caller's exchanged credential and their own entitlement
/// snapshot become the same per-protocol gateway routes a managed profile
/// gets from its deployment-wide snapshot. No caller means no gateway route,
/// which fails an unnamed turn closed rather than running it as somebody
/// else.
pub async fn collect_routes(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    gateway_tokens: Option<std::sync::Arc<dyn tidebreak_router::BearerTokenSource>>,
    chatgpt: Option<(
        std::sync::Arc<dyn tidebreak_router::BearerTokenSource>,
        String,
    )>,
    on_behalf_of: Option<OnBehalfOfGateway>,
    policy: &crate::managed_policy::ManagedPolicy,
) -> Vec<tidebreak_router::Route> {
    let mut routes = Vec::new();
    for &kind in ProviderKind::ALL {
        if kind == ProviderKind::ModelGateway {
            if !policy.managed {
                // A gateway-authenticated hosted machine routes each caller
                // through their own entitlements and credential (decision
                // 62). The routes are the same shape a managed profile gets —
                // both compat protocols, frozen identities included — built
                // from the caller's snapshot instead of the deployment-wide
                // one. No caller path means no gateway route, which fails an
                // unnamed turn closed rather than running it as somebody
                // else.
                let Some(obo) = on_behalf_of.as_ref() else {
                    continue;
                };
                routes.extend(gateway_snapshot_routes(
                    Some(&obo.snapshot),
                    obo.gateway_base_url.trim_end_matches('/'),
                    obo.source.clone(),
                ));
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
            let snapshot = gateway_snapshot_for_policy(store, policy)
                .await
                .unwrap_or_default()
                .filter(|snapshot| snapshot.installation_id.as_deref() == source.binding_id());
            routes.extend(gateway_snapshot_routes(
                snapshot.as_ref(),
                base.trim_end_matches('/'),
                source,
            ));
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
            routes.push(tidebreak_router::Route {
                kind: route_kind(kind),
                api_key: String::new(),
                base_url: Some(crate::connectors::CODEX_BASE_URL.to_string()),
                // Codex rejects API-only ids; keep the route honest with the
                // same ChatGPT stance the catalog uses for `available`.
                curated_models: model_registry::models_for(kind)
                    .filter(|spec| spec.supports_chatgpt_auth())
                    .map(|spec| spec.id.to_string())
                    .collect(),
                model_rewrites: HashMap::new(),
                token_source: Some(source),
                chatgpt_account_id: Some(account_id),
            });
            continue;
        }
        let api_key = match stored_credential {
            Some(ProviderCredential::ApiKey { key }) if !key.is_empty() => Some(key),
            Some(_) => None,
            None => env_api_key(kind),
        };
        // Local Ollama accepts unauthenticated requests. Keep its route key
        // empty so the router can distinguish this narrow trust class from a
        // reusable bearer credential when validating cleartext loopback URLs.
        let api_key = match api_key {
            Some(key) => key,
            None if !kind.requires_credential() => String::new(),
            None => continue,
        };
        let base_url = kind.effective_base_url(config.base_url.as_deref());
        if kind == ProviderKind::OpenaiCompatible && base_url.is_none() {
            continue;
        }
        if !matches!(kind, ProviderKind::Gemini | ProviderKind::Xai) {
            if let Some(base) = base_url.as_deref() {
                if !base_url_is_allowed(base, kind == ProviderKind::Ollama && api_key.is_empty()) {
                    continue;
                }
            }
        }
        routes.push(tidebreak_router::Route {
            kind: route_kind(kind),
            api_key,
            // Gemini and xAI use fixed first-party endpoints in production.
            // Never let a stale/directly written setting redirect their keys.
            base_url: (!matches!(kind, ProviderKind::Gemini | ProviderKind::Xai))
                .then_some(base_url)
                .flatten(),
            curated_models: model_registry::models_for(kind)
                .map(|spec| spec.id.to_string())
                .chain(config.models.into_iter().map(|model| model.id))
                .collect(),
            model_rewrites: HashMap::new(),
            token_source: None,
            chatgpt_account_id: None,
        });
    }
    routes
}

/// Routes for one gateway snapshot: a per-protocol compat route carrying both
/// the raw and frozen model ids, or one inert Anthropic-compat route for an
/// empty snapshot — it still selects no model, but a signed-in profile then
/// has one gateway adapter rather than looking indistinguishable from
/// missing credentials. One builder for both snapshot sources, so a hosted
/// caller's routes and a managed profile's cannot drift apart (decision 62).
fn gateway_snapshot_routes(
    snapshot: Option<&GatewayModelSnapshot>,
    base: &str,
    source: std::sync::Arc<dyn tidebreak_router::BearerTokenSource>,
) -> Vec<tidebreak_router::Route> {
    let mut routes = Vec::new();
    let mut anthropic_models = Vec::new();
    let mut openai_models = Vec::new();
    let mut anthropic_rewrites = HashMap::new();
    let mut openai_rewrites = HashMap::new();
    if let Some(snapshot) = snapshot {
        for model in &snapshot.models {
            let protocol = snapshot
                .model_protocols
                .get(&model.id)
                .copied()
                .unwrap_or_default();
            let frozen = frozen_gateway_route_model(snapshot, model, protocol);
            match protocol {
                GatewayModelProtocol::AnthropicMessages => {
                    anthropic_models.push(model.id.clone());
                    anthropic_models.push(frozen.clone());
                    anthropic_rewrites.insert(frozen, model.id.clone());
                }
                GatewayModelProtocol::OpenaiResponses => {
                    openai_models.push(model.id.clone());
                    openai_models.push(frozen.clone());
                    openai_rewrites.insert(frozen, model.id.clone());
                }
            }
        }
    }
    if anthropic_models.is_empty() && openai_models.is_empty() {
        routes.push(tidebreak_router::Route {
            kind: tidebreak_router::RouteKind::ModelGateway,
            api_key: String::new(),
            base_url: Some(format!("{base}/compat/anthropic")),
            curated_models: Vec::new(),
            model_rewrites: HashMap::new(),
            token_source: Some(source),
            chatgpt_account_id: None,
        });
        return routes;
    }
    if !anthropic_models.is_empty() {
        routes.push(tidebreak_router::Route {
            kind: tidebreak_router::RouteKind::ModelGateway,
            api_key: String::new(),
            base_url: Some(format!("{base}/compat/anthropic")),
            curated_models: anthropic_models,
            model_rewrites: anthropic_rewrites,
            token_source: Some(source.clone()),
            chatgpt_account_id: None,
        });
    }
    if !openai_models.is_empty() {
        routes.push(tidebreak_router::Route {
            kind: tidebreak_router::RouteKind::ModelGatewayOpenai,
            api_key: String::new(),
            base_url: Some(format!("{base}/compat/openai/v1")),
            curated_models: openai_models,
            model_rewrites: openai_rewrites,
            token_source: Some(source),
            chatgpt_account_id: None,
        });
    }
    routes
}

/// One-shot boot migration for authentication that can outlive provider config.
///
/// Anthropic keys predate provider rows, and a ChatGPT OAuth session lives in
/// the OS credential store independently of the settings database. If either
/// credential is present while its provider row is missing after an update,
/// restore the same enabled state that saving the key or completing sign-in
/// would have written. An explicit `enabled: false` still wins because a
/// present row is never changed.
pub async fn migrate_legacy_provider_enablement(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<()> {
    for kind in [ProviderKind::Anthropic, ProviderKind::Openai] {
        if store.get_setting(&kind.setting_key()).await?.is_some() {
            continue;
        }
        if has_credential(secrets, kind).await {
            write_config(
                store,
                kind,
                &ProviderConfig {
                    enabled: true,
                    base_url: None,
                    models: Vec::new(),
                },
            )
            .await?;
        }
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
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<Option<ResolvedModelPolicy>> {
    if let Some((provider, id)) = model_registry::parse_selection_key(value) {
        if let Some(spec) = model_registry::find_for(provider, id) {
            return Ok(Some(ResolvedModelPolicy::curated(spec)));
        }
        if provider == ProviderKind::ModelGateway && id.starts_with(FROZEN_GATEWAY_MODEL_PREFIX) {
            // A hosted caller's frozen selections resolve against their own
            // snapshot (decision 62); nothing writes the stored one there.
            if let Some(snapshot) = caller_gateway {
                return Ok(resolve_frozen_gateway_policy(snapshot, id));
            }
            return Ok(read_gateway_snapshot(store)
                .await?
                .as_ref()
                .and_then(|snapshot| resolve_frozen_gateway_policy(snapshot, id)));
        }
        let models = match provider {
            kind if kind.accepts_configured_models() => read_config(store, provider).await?.models,
            // Resolution is not an offer: usability and routing gate the
            // gateway on policy separately, so the raw snapshot read here
            // only keeps stored selections legible.
            ProviderKind::ModelGateway => match caller_gateway {
                Some(snapshot) => snapshot.models.clone(),
                None => read_gateway_snapshot(store)
                    .await?
                    .map(|snapshot| snapshot.models)
                    .unwrap_or_default(),
            },
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

    let mut owners = bare_model_owners(store, value).await?;
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
///
/// A gateway-authenticated hosted machine is the one profile that is usable
/// with nothing stored at all: `caller_gateway` is the requesting caller's
/// own entitlement snapshot (decision 62), resolved per request from their
/// live credential, and its presence is what makes the gateway usable for
/// them. Every other deployment passes `None` and keeps the stored-row walk.
pub async fn provider_is_usable(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    kind: ProviderKind,
    policy: &crate::managed_policy::ManagedPolicy,
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<bool> {
    if policy.managed {
        if kind != ProviderKind::ModelGateway {
            return Ok(false);
        }
        let Some(gateway_url) = policy.gateway_url.as_deref() else {
            return Ok(false);
        };
        let Some(installation_id) =
            crate::connectors::stored_installation_id_for(secrets, gateway_url).await
        else {
            return Ok(false);
        };
        return Ok(gateway_snapshot_for_policy(store, policy)
            .await?
            .is_some_and(|snapshot| {
                snapshot.installation_id.as_deref() == Some(&installation_id)
            }));
    }
    if kind == ProviderKind::ModelGateway {
        // A hosted machine's gateway is usable exactly when this caller's own
        // entitlement snapshot resolved (decision 62): the snapshot exists
        // only because the caller's live token just exchanged and fetched it.
        return Ok(caller_gateway.is_some());
    }
    let config = read_config(store, kind).await?;
    if !config.enabled {
        return Ok(false);
    }
    if kind.requires_credential() && !has_credential(secrets, kind).await {
        return Ok(false);
    }
    if !matches!(kind, ProviderKind::Gemini | ProviderKind::Xai) {
        if let Some(base) = kind.effective_base_url(config.base_url.as_deref()) {
            let has_reusable_credential = has_credential(secrets, kind).await;
            if !base_url_is_allowed(
                &base,
                kind == ProviderKind::Ollama && !has_reusable_credential,
            ) {
                return Ok(false);
            }
        }
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
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<bool> {
    if !provider_is_usable(store, secrets, model.provider, policy, caller_gateway).await? {
        return Ok(false);
    }
    if model.provider == ProviderKind::ModelGateway {
        let Some(snapshot) = gateway_snapshot_for(store, policy, caller_gateway).await? else {
            return Ok(false);
        };
        let matches_snapshot = if model.route_model.starts_with(FROZEN_GATEWAY_MODEL_PREFIX) {
            resolve_frozen_gateway_policy(&snapshot, &model.route_model)
                .is_some_and(|current| current == *model)
        } else {
            snapshot
                .models
                .iter()
                .any(|candidate| candidate.id == model.id)
        };
        if !matches_snapshot {
            return Ok(false);
        }
    }
    // ChatGPT / Codex auth rejects some API-only OpenAI ids. Keep the row in
    // the catalog (API-key installs still need it) but mark it unusable here.
    if model.provider == ProviderKind::Openai
        && matches!(
            auth_mode_for(secrets, ProviderKind::Openai).await,
            Some(ProviderAuthMode::Chatgpt)
        )
    {
        let supported = model_registry::find_for(ProviderKind::Openai, &model.id)
            .is_some_and(model_registry::ModelSpec::supports_chatgpt_auth);
        return Ok(supported);
    }
    Ok(true)
}

/// Full typed catalog. Unavailable rows remain visible for provider-scoped
/// settings, but clients must not offer them as usable selections.
pub async fn catalog_models(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
    policy: &crate::managed_policy::ManagedPolicy,
    caller_gateway: Option<&GatewayModelSnapshot>,
) -> Result<Vec<CatalogModel>> {
    let mut models = Vec::new();
    for &kind in ProviderKind::ALL {
        let provider_usable =
            provider_is_usable(store, secrets, kind, policy, caller_gateway).await?;
        let chatgpt = matches!(
            auth_mode_for(secrets, kind).await,
            Some(ProviderAuthMode::Chatgpt)
        );
        models.extend(model_registry::models_for(kind).map(|spec| {
            let available = provider_usable && (!chatgpt || spec.supports_chatgpt_auth());
            CatalogModel {
                policy: ResolvedModelPolicy::curated(spec),
                available,
            }
        }));
        // Configured model sets: the compatible endpoint's custom entries,
        // and the managed gateway's entitled snapshot — which is empty on an
        // unmanaged profile, so no gateway row ever reaches the catalog there.
        let configured = match kind {
            kind if kind.accepts_configured_models() => read_config(store, kind).await?.models,
            ProviderKind::ModelGateway => gateway_models(store, policy, caller_gateway).await?,
            _ => Vec::new(),
        };
        models.extend(configured.iter().map(|model| CatalogModel {
            policy: match kind {
                ProviderKind::ModelGateway => ResolvedModelPolicy::gateway_for(model),
                _ => ResolvedModelPolicy::custom_for(kind, model),
            },
            available: provider_usable,
        }));
    }
    Ok(models)
}

#[cfg(test)]
mod tests;
