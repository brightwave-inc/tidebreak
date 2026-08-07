//! Route handlers extracted from the parent `routes` module.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::{header, StatusCode};
use serde::{Deserialize, Serialize};

use openwave_core::{PermissionMode, ReasoningEffort, SecretProvider, Store};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::model_roles::{self, ModelRole};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;
use crate::voice_transcription::{self, VoiceTranscriptionInfo, VoiceTranscriptionUpdate};

pub(super) async fn read_model(store: &dyn Store) -> openwave_core::Result<Option<String>> {
    model_roles::read_selection(store, ModelRole::Chat).await
}

/// The `chat` role's model with no conversation in hand: the global selection,
/// else the model this process launched with.
///
/// The boot default is process state, which is why the chat role has no
/// registry-backed default list of its own the way `utility` does.
pub(super) async fn chat_role_model(
    store: &dyn Store,
    boot_default: &str,
) -> openwave_core::Result<String> {
    Ok(read_model(store)
        .await?
        .unwrap_or_else(|| boot_default.to_owned()))
}

/// Resolve which model a new execution in `chat` should use.
///
/// The order is the chat's override, then the global `model` setting, then the
/// boot default. A foreground turn freezes the result when its message is
/// accepted; a sandbox child inherits its origin turn's frozen selection and
/// only falls back here when it was admitted before that was recorded.
pub(crate) async fn resolve_chat_model(
    store: &dyn Store,
    chat: &openwave_core::Chat,
    boot_default: &str,
) -> openwave_core::Result<String> {
    match chat.model.clone() {
        Some(model) => Ok(model),
        None => chat_role_model(store, boot_default).await,
    }
}

/// Resolve, canonicalize, and availability-check a model selection before it
/// crosses a persistence boundary. Custom embedders with an injected provider
/// retain their free-form model contract; the production configured resolver
/// always enforces the typed registry.
pub(super) async fn validate_model_selection(
    state: &AppState,
    value: &str,
    allow_legacy_custom: bool,
) -> Result<String, ServerError> {
    if value.is_empty() {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    if !state.resolver.enforces_model_registry() {
        return Ok(value.to_owned());
    }
    let Some(policy) =
        providers::resolve_model_policy(&*state.store, value, allow_legacy_custom).await?
    else {
        return Err(ServerError::bad_request_kind(
            "unknown_model",
            format!(
                "model `{value}` is not registered for that provider; configure it in that provider's model list first"
            ),
        ));
    };
    let managed = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if !providers::model_is_usable(&*state.store, &*state.secrets, &policy, &managed).await? {
        return Err(ServerError::conflict_kind(
            "model_provider_unavailable",
            format!(
                "provider `{}` cannot serve model `{}` with its current configuration or credential",
                policy.provider, policy.id
            ),
        ));
    }
    Ok(policy.key)
}

/// Whether any model provider credential is configured — stored or via the
/// env fallbacks the resolver also honors. Prefer `GET /providers` for
/// per-kind detail; this field is the legacy "is anything ready?" signal.
pub(super) async fn has_api_key(secrets: &dyn SecretProvider) -> bool {
    for &kind in ProviderKind::ALL {
        if providers::has_credential(secrets, kind).await {
            return true;
        }
    }
    false
}

/// Refuse selecting a permission mode above the managed ceiling.
///
/// The picker-facing half of the lockdown: the authoritative clamp lives at
/// the turn gate, which also catches chats whose stored mode predates the
/// policy. Selecting a mode at or below the ceiling stays open — the policy
/// names a maximum, not a fixed mode.
pub(super) async fn refuse_permission_mode_over_ceiling(
    state: &AppState,
    requested: Option<PermissionMode>,
) -> Result<(), ServerError> {
    let Some(mode) = requested else {
        return Ok(());
    };
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if !policy.permits_permission_mode(mode) {
        return Err(ServerError::conflict_kind(
            "permission_mode_locked",
            format!(
                "permission mode `{}` exceeds the maximum this managed profile allows (`{}`)",
                mode.as_str(),
                policy
                    .permission_mode_ceiling
                    .unwrap_or(PermissionMode::Allow)
                    .as_str()
            ),
        ));
    }
    Ok(())
}

/// Refuse a BYOK credential write on a managed profile.
///
/// The gateway session is a managed profile's only model credential; stored
/// BYOK keys are frozen while the policy holds — inert, not deleted, so an
/// unmanaged profile is byte-for-byte unaffected.
pub(super) async fn refuse_credential_writes_when_managed(
    state: &AppState,
) -> Result<(), ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    if policy.managed {
        return Err(providers::managed_profile_refusal(
            "this profile is managed by a model gateway; provider API keys are locked",
        ));
    }
    Ok(())
}

/// Body of `PUT /settings/api-key`.
///
/// Legacy shim: writes the Anthropic credential in the typed blob shape and
/// enables the Anthropic provider. Prefer `PUT /providers/anthropic`.
#[derive(Deserialize)]
pub struct ApiKey {
    /// The provider API key to store. Written to the `SecretProvider` (the OS
    /// keychain on desktop), never to the database, and never read back out.
    pub api_key: String,
}

// Redact the key so it can't leak through a `{:?}`.
impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey").field("api_key", &"***").finish()
    }
}

/// `PUT /settings/api-key` — store the Anthropic API key. `204 No Content`.
pub async fn put_api_key(
    State(state): State<AppState>,
    Json(body): Json<ApiKey>,
) -> Result<StatusCode, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    if body.api_key.is_empty() {
        return Err(ServerError::bad_request("api_key must not be empty"));
    }
    // Write the typed credential and enable Anthropic so the new providers
    // surface and the legacy shim stay equivalent.
    providers::write_credential(
        &*state.secrets,
        ProviderKind::Anthropic,
        &ProviderCredential::api_key(&body.api_key),
    )
    .await?;
    let mut config = providers::read_config(&*state.store, ProviderKind::Anthropic).await?;
    config.enabled = true;
    providers::write_config(&*state.store, ProviderKind::Anthropic, &config).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /settings/api-key` — remove the stored Anthropic API key. `204`.
///
/// Clears only the stored key. If the daemon was launched with an
/// `ANTHROPIC_API_KEY` in its environment, that fallback still applies — so
/// `has_api_key` may stay `true` and turns keep resolving a provider after a
/// delete. The environment is a deploy-time default the API doesn't override.
pub async fn delete_api_key(State(state): State<AppState>) -> Result<StatusCode, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    providers::delete_credential(&*state.secrets, ProviderKind::Anthropic).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Response for `GET /providers`.
#[derive(Debug, Serialize)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

/// `GET /providers` — every known provider kind and its current config. The
/// model gateway appears only on a managed profile, projected from policy.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersList>, ServerError> {
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    Ok(Json(ProvidersList {
        providers: providers::list_providers(&*state.store, &*state.secrets, &policy).await?,
    }))
}

/// `PUT /providers/{kind}` — update a provider's config and/or credential.
pub async fn put_provider(
    State(state): State<AppState>,
    Path(kind): Path<String>,
    Json(body): Json<ProviderUpdate>,
) -> Result<Json<ProviderInfo>, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    if kind == ProviderKind::Openai
        && matches!(
            body.credential,
            Some(providers::ProviderCredential::ApiKey { .. })
        )
    {
        // Switching to a key retires the subscription session. Sign out
        // rather than letting the write clear the vault on its own: that
        // cancels a sign-in still in flight (it would otherwise land after
        // the write and replace the key) and revokes the refresh token
        // instead of dropping it locally while it stays live at OpenAI.
        state.chatgpt.sign_out().await?;
    } else if kind == ProviderKind::Openai && body.enabled == Some(false) {
        // Completing sign-in forces enabled=true. Drop an in-flight attempt
        // so it cannot overwrite an explicit disable that landed first.
        state.chatgpt.cancel_pending().await;
    }
    let info = providers::update_provider(
        &*state.store,
        &*state.secrets,
        kind,
        body,
        &*state.os_policy,
    )
    .await?;
    Ok(Json(info))
}

/// `DELETE /providers/{kind}/credential` — remove the stored credential. `204`.
pub async fn delete_provider_credential(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<StatusCode, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    refuse_credential_writes_when_managed(&state).await?;
    if kind == ProviderKind::Openai {
        // Revoke through the runtime first so an in-flight sign-in is
        // cancelled; the delete below still has to run because the stored
        // credential may be an API key rather than the OAuth marker.
        state.chatgpt.sign_out().await?;
    }
    providers::delete_credential(&*state.secrets, kind).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /providers/openai/chatgpt/sign-in` — start ChatGPT OAuth; returns the
/// browser URL. Completion is asynchronous; poll `GET /providers`.
pub async fn post_openai_chatgpt_sign_in(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    let authorization_url = state.chatgpt.begin_sign_in().await?;
    Ok(Json(
        serde_json::json!({ "authorization_url": authorization_url }),
    ))
}

/// `POST /providers/openai/chatgpt/sign-out` — revoke and clear ChatGPT OAuth.
pub async fn post_openai_chatgpt_sign_out(
    State(state): State<AppState>,
) -> Result<StatusCode, ServerError> {
    refuse_credential_writes_when_managed(&state).await?;
    state.chatgpt.sign_out().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /providers/openai/chatgpt/status` — pending / signed-in / error.
pub async fn get_openai_chatgpt_status(
    State(state): State<AppState>,
) -> Result<Json<crate::chatgpt_runtime::ChatGptSignInStatus>, ServerError> {
    Ok(Json(state.chatgpt.status().await))
}

pub async fn get_voice_transcription(
    State(state): State<AppState>,
) -> Result<Json<VoiceTranscriptionInfo>, ServerError> {
    Ok(Json(
        voice_transcription::info(&*state.store, &*state.secrets, &*state.local_voice).await?,
    ))
}

pub async fn put_voice_transcription(
    State(state): State<AppState>,
    Json(body): Json<VoiceTranscriptionUpdate>,
) -> Result<Json<VoiceTranscriptionInfo>, ServerError> {
    Ok(Json(
        voice_transcription::update(&*state.store, &*state.secrets, &*state.local_voice, body)
            .await?,
    ))
}

pub async fn post_voice_transcription(
    State(state): State<AppState>,
    headers: HeaderMap,
    audio: Bytes,
) -> Result<Json<serde_json::Value>, ServerError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ServerError::bad_request("voice recording content type is required"))?;
    let text = voice_transcription::transcribe(
        &*state.store,
        &*state.secrets,
        &*state.local_voice,
        content_type,
        audio,
    )
    .await?;
    Ok(Json(serde_json::json!({ "text": text })))
}

pub async fn post_voice_transcription_install(
    State(state): State<AppState>,
    Json(request): Json<voice_transcription::LocalVoiceInstall>,
) -> Result<Json<voice_transcription::LocalVoiceInfo>, ServerError> {
    Ok(Json(
        voice_transcription::install_local(&*state.store, &*state.local_voice, request).await?,
    ))
}

/// A selectable model in the catalog.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ModelInfo {
    /// Stable provider-qualified selection key used by settings and chats.
    pub key: String,
    /// The identifier passed to the provider and stored as `chat.model`.
    pub id: String,
    /// Human-readable label for the selector (e.g. `"Claude Opus 4.8"`).
    pub display_name: String,
    /// The provider that serves the model.
    pub provider: ProviderKind,
    /// The vendor whose curated model this row is, when that differs from the
    /// provider serving it — a gateway-served model whose id exactly matches a
    /// curated one. For presentation only (icon and branding); routing still
    /// uses `provider`, and a client falls back to it when this is null.
    pub vendor: Option<ProviderKind>,
    /// How thoroughly OpenWave has exercised this provider/model combination.
    pub verification: crate::model_registry::VerificationTier,
    /// Whether the provider is enabled, configured, credentialed, and able to
    /// serve this model at its configured endpoint/location.
    pub available: bool,
    /// Approximate context window in tokens.
    pub context_window: u32,
    /// Maximum model output in tokens.
    pub max_output_tokens: u32,
    /// Input modalities accepted by the model.
    pub input_modalities: Vec<crate::model_registry::InputModality>,
    /// Whether the model can produce an internal reasoning stream.
    pub supports_reasoning: bool,
    /// The reasoning-effort levels this model accepts, ascending. Empty when
    /// the model exposes no effort control, which is what a client checks
    /// before offering the selector at all.
    ///
    /// Carries the enum rather than plain strings so the generated TypeScript
    /// is the same union a chat's stored effort has, and a client cannot offer
    /// a level it could not then set.
    pub reasoning_efforts: Vec<ReasoningEffort>,
    /// Whether the model accepts image input alongside text.
    pub multimodal: bool,
}

/// One named model role and what it resolves to right now.
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ModelRoleInfo {
    /// The role this row describes.
    pub role: ModelRole,
    /// The catalog key the user selected for this role, or `None` when the role
    /// is left automatic.
    pub selection: Option<String>,
    /// The catalog key this role resolves to right now, selection or not.
    ///
    /// A selector that offers "automatic" as a choice can only say what that
    /// choice means if the server says which model it lands on. `None` when the
    /// role resolves to nothing the catalog can name, which leaves the client
    /// with nothing to promise rather than a guess — and, for `utility`, means
    /// the work that depends on it is skipped.
    pub resolved_key: Option<String>,
}

/// Response for `GET /models`.
#[derive(Debug, Serialize)]
pub struct ModelCatalog {
    /// The models a client can select from.
    pub models: Vec<ModelInfo>,
    /// Every named role, its selection, and what it currently resolves to.
    pub roles: Vec<ModelRoleInfo>,
}

/// `GET /models` — the catalog a chat's model selector chooses from.
///
/// All typed registry rows plus current availability. Clients may explain
/// unavailable rows, but must never offer them as usable selections.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<ModelCatalog>, ServerError> {
    let mut roles = Vec::with_capacity(ModelRole::ALL.len());
    for &role in ModelRole::ALL {
        let selection = model_roles::read_selection(&*state.store, role).await?;
        let resolved_key = resolved_role_key(&state, role, selection.as_deref()).await?;
        roles.push(ModelRoleInfo {
            role,
            selection,
            resolved_key,
        });
    }
    let policy = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
    let models = providers::catalog_models(&*state.store, &*state.secrets, &policy)
        .await?
        .into_iter()
        .map(|entry| ModelInfo {
            key: entry.policy.key,
            id: entry.policy.id,
            display_name: entry.policy.display_name,
            provider: entry.policy.provider,
            vendor: entry.policy.vendor,
            verification: entry.policy.verification,
            available: entry.available,
            context_window: entry.policy.context_window,
            max_output_tokens: entry.policy.max_output_tokens,
            input_modalities: entry.policy.input_modalities.clone(),
            supports_reasoning: entry.policy.supports_reasoning,
            reasoning_efforts: entry.policy.reasoning_efforts.clone(),
            multimodal: entry
                .policy
                .input_modalities
                .contains(&crate::model_registry::InputModality::Image),
        })
        .collect();
    Ok(Json(ModelCatalog { models, roles }))
}

/// Body of `PUT /models/roles/{role}`. An explicit `null` selection returns the
/// role to automatic resolution.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoleUpdate {
    /// The catalog key to pin this role to, or `null` for automatic.
    #[serde(default)]
    pub selection: Option<String>,
}

/// `PUT /models/roles/{role}` — pin a role to one model, or clear it back to
/// automatic resolution.
///
/// A selection must be a registered model whose provider is currently usable, so
/// a role cannot be pinned to something that could not run. For `chat` this
/// writes the same setting as `PUT /settings`.
pub async fn put_model_role(
    State(state): State<AppState>,
    Path(role): Path<String>,
    Json(body): Json<ModelRoleUpdate>,
) -> Result<Json<ModelRoleInfo>, ServerError> {
    let role = ModelRole::parse(&role)
        .ok_or_else(|| ServerError::not_found(format!("unknown model role: {role}")))?;
    let selection = match body.selection {
        Some(selection) => Some(validate_model_selection(&state, &selection, false).await?),
        None => None,
    };
    model_roles::write_selection(&*state.store, role, selection.as_deref()).await?;
    let resolved_key = resolved_role_key(&state, role, selection.as_deref()).await?;
    Ok(Json(ModelRoleInfo {
        role,
        selection,
        resolved_key,
    }))
}

/// The catalog key `role` resolves to right now, given its stored `selection`.
///
/// This is the server's one answer for "what does this role's default mean" —
/// the settings page and the composer both label their automatic choice with
/// it, which is why the managed re-route below lives here rather than in a
/// client.
async fn resolved_role_key(
    state: &AppState,
    role: ModelRole,
    selection: Option<&str>,
) -> Result<Option<String>, ServerError> {
    match role {
        // The chat role goes through the same seam a new execution does, minus
        // the per-chat override there is no chat here to read — so the label a
        // client shows for "default" is what the next turn actually gets: the
        // accept path below freezes its model through the same
        // `effective_chat_policy`, managed re-route included. Its last resort
        // is the boot default, which no role's list can name.
        ModelRole::Chat => {
            let fallback = match selection {
                Some(selection) => selection.to_owned(),
                None => chat_role_model(&*state.store, &state.agent_config.model).await?,
            };
            let managed = crate::managed_policy::resolve(&*state.store, &*state.os_policy).await?;
            Ok(model_roles::effective_chat_policy(
                &*state.store,
                &*state.secrets,
                &managed,
                &fallback,
            )
            .await?
            .map(|policy| policy.key))
        }
        _ => Ok(
            model_roles::resolve(&*state.store, &*state.secrets, &*state.os_policy, role)
                .await?
                .map(|policy| policy.key),
        ),
    }
}
