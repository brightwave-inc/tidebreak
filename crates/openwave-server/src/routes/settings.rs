//! Route handlers extracted from the parent `routes` module.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};

use openwave_core::{AgentRun, ChatId, PermissionMode, ReasoningEffort, Store, TurnId};

use crate::code_execution::{
    self, CodeExecutionConfigInfo, CodeExecutionConfigUpdate, CodeExecutionCredentialReadiness,
    CodeExecutionCredentialsInfo,
};
use crate::error::ServerError;
use crate::exec_write_snapshot::{
    render_file_change_preview, undo_one_file_change, undo_turn_file_changes, ExecFilePreviewError,
    ExecFilePreviewRequest, ExecFilePreviewRevision, ExecFileUndoOutcome, ExecTurnUndoOutcome,
};
use crate::extract::{Json, Path};
use crate::model_roles::{self, ModelRole};
use crate::scoped_store::ScopedStore;
use crate::state::AppState;
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

use super::providers_models::{has_api_key, read_model, validate_model_selection};
use super::MAX_ACTIVE_BACKGROUND_AGENTS_SETTING;
use super::SERVED_BYTES_CONTENT_POLICY;

/// Runtime settings a client can read. The API key itself is never returned —
/// it lives in the `SecretProvider`, not the store — only whether one is set.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
pub struct Settings {
    /// The model turns run against, or `None` to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether a model API key is configured (never the key itself).
    pub has_api_key: bool,
    /// The sticky new-chat defaults, so a composer for a chat that does not
    /// exist yet can show what `POST /chats` will seed.
    #[serde(default)]
    pub chat_defaults: StickyChatDefaults,
    /// Maximum nonterminal spawned agents allowed in one chat.
    pub max_active_background_agents: u32,
}

/// The reader's last explicit per-chat choices — what an unspecified field of
/// `POST /chats` seeds. A `None` field has no recorded choice and keeps the
/// hard default (configured model, `ask`, open network).
///
/// The permission mode is reported clamped to any managed ceiling, so what a
/// picker displays is what creation will actually seed.
#[derive(Debug, Default, Serialize, Deserialize, ts_rs::TS)]
pub struct StickyChatDefaults {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default)]
    pub network_policy: Option<openwave_core::NetworkPolicy>,
}

/// Body of `PUT /settings`. Each field is a *double* option so an absent key is
/// distinguished from an explicit `null`: absent leaves the value unchanged,
/// `null` resets it to the server default, and a value sets it.
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
    #[serde(default)]
    pub max_active_background_agents: Option<u32>,
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`; `#[serde(default)]`
/// supplies `None` when the field is absent.
pub(super) fn double_option<'de, D, T>(
    deserializer: D,
) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    serde::Deserialize::deserialize(deserializer).map(Some)
}

/// `GET /settings` — the current runtime settings.
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<Settings>, ServerError> {
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
        chat_defaults: read_sticky_chat_defaults(&state).await?,
        max_active_background_agents: read_max_active_background_agents(&*state.store).await?,
    }))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are touched.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(mut body): Json<SettingsUpdate>,
) -> Result<Json<Settings>, ServerError> {
    if let Some(Some(model)) = body.model.as_mut() {
        *model = validate_model_selection(&state, model, false).await?;
    }
    match body.model {
        // Absent: leave the model unchanged.
        None => {}
        // Explicit null: reset to the server default.
        Some(None) => {
            model_roles::write_selection(&*state.store, ModelRole::Chat, None).await?;
        }
        // A value: reject empty (it would break every turn), else set it.
        Some(Some(model)) => {
            if model.is_empty() {
                return Err(ServerError::bad_request("model must not be empty"));
            }
            model_roles::write_selection(&*state.store, ModelRole::Chat, Some(&model)).await?;
        }
    }
    if let Some(limit) = body.max_active_background_agents {
        if limit == 0 || limit > AgentRun::MAX_CONCURRENCY_LIMIT {
            return Err(ServerError::bad_request(format!(
                "max_active_background_agents must be in 1..={}",
                AgentRun::MAX_CONCURRENCY_LIMIT
            )));
        }
        state
            .store
            .set_setting(
                MAX_ACTIVE_BACKGROUND_AGENTS_SETTING,
                &serde_json::json!(limit),
            )
            .await?;
    }
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
        chat_defaults: read_sticky_chat_defaults(&state).await?,
        max_active_background_agents: read_max_active_background_agents(&*state.store).await?,
    }))
}

pub(crate) async fn read_max_active_background_agents(
    store: &dyn Store,
) -> openwave_core::Result<u32> {
    Ok(store
        .get_setting(MAX_ACTIVE_BACKGROUND_AGENTS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|limit| *limit > 0 && *limit <= AgentRun::MAX_CONCURRENCY_LIMIT)
        .unwrap_or(AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS))
}

/// `GET /web-search` — read host-owned web-search selection and readiness.
/// No model tool is registered by this endpoint.
pub async fn get_web_search_config(
    State(state): State<AppState>,
) -> Result<Json<WebSearchConfigInfo>, ServerError> {
    Ok(Json(
        web_search::config_info(&*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /web-search` — select a fixed provider and bounded timeout. Provider
/// credentials remain in the OS keychain under fixed provider-owned names.
pub async fn put_web_search_config(
    State(state): State<AppState>,
    Json(body): Json<WebSearchConfigUpdate>,
) -> Result<Json<WebSearchConfigInfo>, ServerError> {
    Ok(Json(
        web_search::update_config(&*state.store, &*state.secrets, body).await?,
    ))
}

/// `GET /code-execution` — read host-owned provider selection, timeout policy,
/// and readiness. No executable or provider endpoint is accepted here.
pub async fn get_code_execution_config(
    State(state): State<AppState>,
) -> Result<Json<CodeExecutionConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::config_info(&state.config, &*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /code-execution` — select a fixed provider and bounded host timeout.
pub async fn put_code_execution_config(
    State(state): State<AppState>,
    Json(body): Json<CodeExecutionConfigUpdate>,
) -> Result<Json<CodeExecutionConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::update_config(&state.config, &*state.store, &*state.secrets, body).await?,
    ))
}

/// `GET /code-execution/credentials` — readiness for the fixed E2B and Daytona
/// credential slots. Local execution needs no credential and is absent here.
pub async fn get_code_execution_credentials(
    State(state): State<AppState>,
) -> Json<CodeExecutionCredentialsInfo> {
    Json(code_execution::credentials_info(&*state.secrets).await)
}

const MAX_CODE_EXECUTION_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Body of `PUT /code-execution/credentials/{provider}`. Debug output always
/// redacts the credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeExecutionCredentialUpdate {
    pub api_key: String,
}

impl std::fmt::Debug for CodeExecutionCredentialUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeExecutionCredentialUpdate")
            .field("api_key", &"***")
            .finish()
    }
}

/// Store a managed provider key in its fixed slot without changing selection.
pub async fn put_code_execution_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<CodeExecutionCredentialUpdate>,
) -> Result<Json<CodeExecutionCredentialReadiness>, ServerError> {
    let provider = code_execution::credential_provider(&provider)?;
    if body.api_key.len() > MAX_CODE_EXECUTION_CREDENTIAL_BYTES {
        return Err(ServerError::bad_request(format!(
            "{provider} api_key must be at most {MAX_CODE_EXECUTION_CREDENTIAL_BYTES} bytes"
        )));
    }
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return Err(ServerError::bad_request(format!(
            "{provider} api_key must not be empty"
        )));
    }
    Ok(Json(
        code_execution::write_credential(&*state.secrets, provider, api_key).await?,
    ))
}

/// Remove only the requested provider's credential; selection remains unchanged.
pub async fn delete_code_execution_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<CodeExecutionCredentialReadiness>, ServerError> {
    let provider = code_execution::credential_provider(&provider)?;
    Ok(Json(
        code_execution::delete_credential(&*state.secrets, provider).await?,
    ))
}

/// `POST /chats/{chat_id}/turns/{turn_id}/file-changes/undo` — restore the
/// prior bytes journaled for one turn without clobbering later edits.
pub async fn post_undo_turn_file_changes(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id)): Path<(ChatId, TurnId)>,
) -> Result<Json<ExecTurnUndoOutcome>, ServerError> {
    store.require_chat(chat_id).await?;
    let outcome = undo_turn_file_changes(&*state.store, &*state.blobs, chat_id, turn_id).await?;
    if outcome.files.is_empty() {
        return Err(ServerError::not_found(format!(
            "turn {turn_id} has no retained file changes in chat {chat_id}"
        )));
    }
    Ok(Json(outcome))
}

/// `POST /chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/undo` —
/// restore one file from the turn without touching its siblings.
pub async fn post_undo_one_file_change(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id, snapshot_id)): Path<(ChatId, TurnId, uuid::Uuid)>,
) -> Result<Json<ExecFileUndoOutcome>, ServerError> {
    store.require_chat(chat_id).await?;
    undo_one_file_change(&*state.store, &*state.blobs, chat_id, turn_id, snapshot_id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found("no retained file change for this turn"))
}

/// `GET /chats/{chat_id}/turns/{turn_id}/file-changes/{snapshot_id}/preview/{revision}`
/// — render one authorized journal revision without exposing source bytes,
/// paths, or a reusable document identity.
pub async fn get_file_change_preview(
    State(state): State<AppState>,
    store: ScopedStore,
    Path((chat_id, turn_id, snapshot_id, revision)): Path<(
        ChatId,
        TurnId,
        uuid::Uuid,
        ExecFilePreviewRevision,
    )>,
) -> Result<Response, ServerError> {
    store.require_chat(chat_id).await?;
    let _permit = state
        .file_preview_permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ServerError::too_many_requests_kind(
                "file_preview_busy",
                "Document preview rendering is busy; try again shortly.",
            )
        })?;
    let rendered = render_file_change_preview(
        &*state.store,
        &*state.blobs,
        ExecFilePreviewRequest {
            chat_id,
            turn_id,
            snapshot_id,
            revision,
            scripts_dir: state.config.exec_scripts_dir.as_deref(),
            temp_root: &state.config.data_dir.join("file-preview-temp"),
        },
    )
    .await
    .map_err(file_preview_error)?;
    let byte_len = rendered.bytes.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, rendered.media_type.as_str())
        .header(header::CONTENT_LENGTH, byte_len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, SERVED_BYTES_CONTENT_POLICY)
        .header(header::REFERRER_POLICY, "no-referrer")
        .header(header::CONTENT_DISPOSITION, "inline")
        .header("x-openwave-preview-width", rendered.width.to_string())
        .header("x-openwave-preview-height", rendered.height.to_string())
        .body(Body::from(rendered.bytes))
        .map_err(|_| ServerError::internal("failed to build document preview response"))
}

fn file_preview_error(error: ExecFilePreviewError) -> ServerError {
    match error {
        ExecFilePreviewError::NotFound => {
            ServerError::not_found("No file change with that identity exists in this turn.")
        }
        ExecFilePreviewError::Unsupported => ServerError::unsupported_media_type_kind(
            "file_preview_unsupported",
            "No visual preview is available for this file type.",
        ),
        ExecFilePreviewError::Empty => ServerError::unprocessable_kind(
            "file_preview_empty",
            "This side of the change has no file.",
        ),
        ExecFilePreviewError::Stale => ServerError::conflict_kind(
            "file_preview_stale",
            "The file changed again; its after preview is no longer available.",
        ),
        ExecFilePreviewError::TooLarge => ServerError::unprocessable_kind(
            "file_preview_too_large",
            "This revision is too large to preview.",
        ),
        ExecFilePreviewError::Unavailable => ServerError::unprocessable_kind(
            "file_preview_unavailable",
            "This revision is no longer available to preview.",
        ),
        ExecFilePreviewError::RenderFailed => ServerError::unprocessable_kind(
            "file_preview_failed",
            "OpenWave could not render this revision on this device.",
        ),
    }
}

/// Maximum API-key size accepted by the local credential endpoint. This is
/// far beyond ordinary provider keys while keeping accidental pasted blobs out
/// of the OS keychain.
const MAX_WEB_SEARCH_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Body of `PUT /web-search/credentials/{provider}`. The custom `Debug`
/// implementation redacts the only sensitive field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchCredentialUpdate {
    pub api_key: String,
}

impl std::fmt::Debug for WebSearchCredentialUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebSearchCredentialUpdate")
            .field("api_key", &"***")
            .finish()
    }
}

/// `GET /web-search/credentials` — readiness for the fixed Exa and Tavily
/// credential slots. This route never returns the stored keys.
pub async fn get_web_search_credentials(
    State(state): State<AppState>,
) -> Result<Json<WebSearchCredentialsInfo>, ServerError> {
    Ok(Json(web_search::credentials_info(&*state.secrets).await?))
}

/// `PUT /web-search/credentials/{provider}` — store a key in one fixed
/// provider slot. It does not change provider selection or timeout policy.
pub async fn put_web_search_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<WebSearchCredentialUpdate>,
) -> Result<Json<WebSearchCredentialReadiness>, ServerError> {
    let provider = web_search::credential_provider(&provider)?;
    if body.api_key.len() > MAX_WEB_SEARCH_CREDENTIAL_BYTES {
        return Err(ServerError::bad_request(format!(
            "web search api_key must be at most {MAX_WEB_SEARCH_CREDENTIAL_BYTES} bytes"
        )));
    }
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return Err(ServerError::bad_request(
            "web search api_key must not be empty",
        ));
    }
    Ok(Json(
        web_search::write_credential(&*state.secrets, provider, api_key).await?,
    ))
}

/// `DELETE /web-search/credentials/{provider}` — remove only that fixed
/// provider key. It does not change provider selection or timeout policy.
pub async fn delete_web_search_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<WebSearchCredentialReadiness>, ServerError> {
    let provider = web_search::credential_provider(&provider)?;
    Ok(Json(
        web_search::delete_credential(&*state.secrets, provider).await?,
    ))
}

/// Settings keys holding the sticky new-chat defaults: the reader's last
/// explicit per-chat choice at these routes, replayed into the next chat.
///
/// Deployment-scoped like every other setting. `model` deliberately gets its
/// own key rather than reusing the global `model` selection: seeding is a
/// creation-time copy into the new chat, so picking a model in one chat never
/// retargets existing chats that ride the configured default.
pub(super) const STICKY_MODEL_KEY: &str = "chat_default.model";
pub(super) const STICKY_REASONING_EFFORT_KEY: &str = "chat_default.reasoning_effort";
pub(super) const STICKY_PERMISSION_MODE_KEY: &str = "chat_default.permission_mode";
pub(super) const STICKY_NETWORK_POLICY_KEY: &str = "chat_default.network_policy";

/// Read one sticky new-chat default. A stored value this build no longer
/// recognizes reads as unset rather than failing the create.
pub(super) async fn read_sticky_default<T: serde::de::DeserializeOwned>(
    store: &dyn Store,
    key: &str,
) -> openwave_core::Result<Option<T>> {
    Ok(store
        .get_setting(key)
        .await?
        .and_then(|value| serde_json::from_value(value).ok()))
}

/// Record (or clear, with `None`) one sticky new-chat default.
pub(super) async fn write_sticky_default<T: Serialize>(
    store: &dyn Store,
    key: &str,
    value: Option<&T>,
) -> Result<(), ServerError> {
    let value = match value {
        Some(value) => serde_json::to_value(value)
            .map_err(|_| ServerError::internal("could not encode a sticky chat default"))?,
        None => serde_json::Value::Null,
    };
    Ok(store.set_setting(key, &value).await?)
}

/// Read every sticky new-chat default, the permission mode clamped to any
/// managed ceiling — what `POST /chats` will seed, and therefore what a
/// composer should display before the chat exists.
pub(super) async fn read_sticky_chat_defaults(
    state: &AppState,
) -> Result<StickyChatDefaults, ServerError> {
    let store = &*state.store;
    let permission_mode = match read_sticky_default(store, STICKY_PERMISSION_MODE_KEY).await? {
        // The managed ceiling clamps a sticky mode recorded before the
        // policy arrived: a remembered `allow` under an `ask` ceiling seeds
        // (and reads back) `ask`, mirroring the turn gate's treatment of
        // stored over-ceiling modes.
        Some(mode) => crate::managed_policy::resolve(store, &*state.os_policy)
            .await?
            .clamp_permission_mode(Some(mode)),
        None => None,
    };
    Ok(StickyChatDefaults {
        model: read_sticky_default(store, STICKY_MODEL_KEY).await?,
        reasoning_effort: read_sticky_default(store, STICKY_REASONING_EFFORT_KEY).await?,
        permission_mode,
        network_policy: read_sticky_default(store, STICKY_NETWORK_POLICY_KEY).await?,
    })
}
