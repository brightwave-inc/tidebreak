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

use openwave_core::{Result, SecretProvider, Store};

use crate::error::ServerError;

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

    /// Curated model specs this kind contributes to `GET /models`.
    ///
    /// `openai_compatible` is free-form (local / custom gateways) — it contributes
    /// nothing to the curated catalog; the chat model field accepts any string.
    pub fn curated_specs(self) -> &'static [ModelSpec] {
        match self {
            ProviderKind::Anthropic => ANTHROPIC_MODELS,
            ProviderKind::Openai => OPENAI_MODELS,
            ProviderKind::OpenaiCompatible => &[],
        }
    }
}

/// Capability metadata for a curated model, surfaced by `GET /models` so the
/// chat model selector can show a human label and (later) reason about what a
/// model supports.
///
/// Kept deliberately small — the fields we need at the reasoning-effort /
/// multimodal boundary. Values are conservative where a hard number isn't
/// obvious; extend as new capability boundaries (pricing, extra modalities)
/// start to matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Identifier passed to the provider and stored as `chat.model`.
    pub id: &'static str,
    /// Human-readable label for the selector (e.g. `"Claude Opus 4.8"`).
    pub display_name: &'static str,
    /// Provider that serves the model.
    pub provider: ProviderKind,
    /// Approximate context window in tokens.
    pub context_window: u32,
    /// Whether the model exposes a reasoning-effort control.
    pub supports_reasoning_effort: bool,
    /// Whether the model accepts image input alongside text.
    pub multimodal: bool,
}

/// Anthropic's curated catalog.
const ANTHROPIC_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        supports_reasoning_effort: true,
        multimodal: true,
    },
    ModelSpec {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        supports_reasoning_effort: true,
        multimodal: true,
    },
    ModelSpec {
        id: "claude-haiku-4-5-20251001",
        display_name: "Claude Haiku 4.5",
        provider: ProviderKind::Anthropic,
        context_window: 200_000,
        supports_reasoning_effort: true,
        multimodal: true,
    },
];

/// OpenAI's curated catalog.
const OPENAI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-4o",
        display_name: "GPT-4o",
        provider: ProviderKind::Openai,
        context_window: 128_000,
        supports_reasoning_effort: false,
        multimodal: true,
    },
    ModelSpec {
        id: "gpt-4o-mini",
        display_name: "GPT-4o mini",
        provider: ProviderKind::Openai,
        context_window: 128_000,
        supports_reasoning_effort: false,
        multimodal: true,
    },
    ModelSpec {
        id: "o3",
        display_name: "o3",
        provider: ProviderKind::Openai,
        context_window: 200_000,
        supports_reasoning_effort: true,
        multimodal: true,
    },
    ModelSpec {
        id: "o4-mini",
        display_name: "o4-mini",
        provider: ProviderKind::Openai,
        context_window: 200_000,
        supports_reasoning_effort: true,
        multimodal: true,
    },
];

/// Human label for a model id: the curated display name when the id is in the
/// registry, else a readable fallback derived from the id (a trailing date
/// suffix is dropped and the remaining `-`-separated tokens are title-cased).
///
/// Useful for ids outside the curated catalog — a free-form
/// `openai_compatible` model, or a `chat.model` a client set directly.
pub fn display_name_for(id: &str) -> String {
    for &kind in ProviderKind::ALL {
        for spec in kind.curated_specs() {
            if spec.id == id {
                return spec.display_name.to_string();
            }
        }
    }
    derive_display_name(id)
}

/// Fallback label for an id with no curated entry.
fn derive_display_name(id: &str) -> String {
    strip_date_suffix(id)
        .split('-')
        .map(title_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drop a trailing `-YYYYMMDD` or `-YYYY-MM-DD` date suffix, if present.
fn strip_date_suffix(id: &str) -> &str {
    let tokens: Vec<&str> = id.split('-').collect();
    if tokens.len() < 2 {
        return id;
    }
    let is_digits = |t: &str, len: usize| t.len() == len && t.bytes().all(|b| b.is_ascii_digit());

    let last = tokens[tokens.len() - 1];
    if is_digits(last, 8) {
        return &id[..id.len() - last.len() - 1];
    }
    if tokens.len() >= 4 {
        let (y, m, d) = (
            tokens[tokens.len() - 3],
            tokens[tokens.len() - 2],
            tokens[tokens.len() - 1],
        );
        if is_digits(y, 4) && is_digits(m, 2) && is_digits(d, 2) {
            let cut = y.len() + m.len() + d.len() + 3; // three separating dashes
            return &id[..id.len() - cut];
        }
    }
    id
}

/// Title-case a single id token, honoring a few brand spellings.
fn title_token(token: &str) -> String {
    match token {
        "gpt" => return "GPT".to_string(),
        "claude" => return "Claude".to_string(),
        "opus" => return "Opus".to_string(),
        "sonnet" => return "Sonnet".to_string(),
        "haiku" => return "Haiku".to_string(),
        "mini" => return "Mini".to_string(),
        "nano" => return "Nano".to_string(),
        _ => {}
    }
    let mut chars = token.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
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
}

impl ProviderConfig {
    /// Default config when nothing is stored: disabled, no base URL.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            base_url: None,
        }
    }
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
            base_url: config.base_url,
            has_credential: has_credential(secrets, kind).await,
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
        base_url: config.base_url,
        has_credential: has_credential(secrets, kind).await,
    })
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
            curated_models: kind
                .curated_specs()
                .iter()
                .map(|spec| spec.id.to_string())
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
            },
        )
        .await?;
    }
    Ok(())
}

/// Model specs from providers that are both enabled and credentialed.
pub async fn catalog_models(
    store: &dyn Store,
    secrets: &dyn SecretProvider,
) -> Result<Vec<&'static ModelSpec>> {
    let mut models: Vec<&'static ModelSpec> = Vec::new();
    for &kind in ProviderKind::ALL {
        let config = read_config(store, kind).await?;
        if !config.enabled {
            continue;
        }
        if !has_credential(secrets, kind).await {
            continue;
        }
        models.extend(kind.curated_specs());
    }
    // If nothing is enabled+credentialed yet, still surface Anthropic's curated
    // list so a fresh install's model selector isn't empty before the user
    // finishes provider setup (turns still fail-closed without a key).
    if models.is_empty() {
        models.extend(ProviderKind::Anthropic.curated_specs());
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
    fn display_name_prefers_curated_entry() {
        assert_eq!(display_name_for("claude-opus-4-8"), "Claude Opus 4.8");
        assert_eq!(display_name_for("gpt-4o-mini"), "GPT-4o mini");
    }

    #[test]
    fn display_name_falls_back_for_unknown_ids() {
        // Trailing date suffix is dropped, remaining tokens title-cased.
        assert_eq!(
            display_name_for("claude-sonnet-9-20260101"),
            "Claude Sonnet 9"
        );
        assert_eq!(display_name_for("gpt-6-2026-01-01"), "GPT 6");
        assert_eq!(display_name_for("local-model"), "Local Model");
    }

    #[test]
    fn curated_specs_ids_match_across_kinds() {
        // Every spec reports the provider whose catalog it lives in.
        for &kind in ProviderKind::ALL {
            for spec in kind.curated_specs() {
                assert_eq!(spec.provider, kind);
            }
        }
        assert!(ProviderKind::OpenaiCompatible.curated_specs().is_empty());
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
}
