//! Route handlers extracted from the parent `routes` module.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use tidebreak_core::{
    AgentRun, ChatId, CompactionPolicy, OwnerId, PermissionMode, ReasoningEffort, Store, TurnId,
    DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS, DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES,
    DEFAULT_COMPACTION_TARGET_FRACTION, DEFAULT_COMPACTION_THRESHOLD_FRACTION,
};

use crate::code_execution::{
    self, ExecConfigInfo, ExecConfigUpdate, ExecCredentialReadiness, ExecCredentialsInfo,
};
use crate::error::ServerError;
use crate::exec_write_snapshot::{
    render_file_change_preview, undo_one_file_change, undo_turn_file_changes, ExecFilePreviewError,
    ExecFilePreviewRequest, ExecFilePreviewRevision, ExecFileUndoOutcome, ExecTurnUndoOutcome,
};
use crate::extract::{Json, Path};
use crate::model_roles::{self, ModelRole};
use crate::principal::AuthContext;
use crate::scoped_store::ScopedStore;
use crate::state::AppState;
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

use super::providers_models::{has_api_key, read_model, validate_model_selection};
use super::{
    COMPACTION_MIN_THRESHOLD_TOKENS_SETTING, COMPACTION_PROTECT_RECENT_MESSAGES_SETTING,
    COMPACTION_TARGET_FRACTION_SETTING, COMPACTION_THRESHOLD_FRACTION_SETTING,
    MAX_ACTIVE_BACKGROUND_AGENTS_SETTING, MODEL_VISIBILITY_OVERRIDES_SETTING,
    SANDBOX_AGENT_CHECKIN_STEPS_SETTING, SANDBOX_AGENT_ERROR_CHECKIN_SETTING,
    SERVED_BYTES_CONTENT_POLICY,
};

/// Largest accepted check-in cadence. Steps are the expensive unit — each is a
/// model completion over the whole replayed chain — so this is "absurd but
/// finite", not a number anyone should reach.
const MAX_SANDBOX_AGENT_CHECKIN_STEPS: u32 = 1_000;
/// Largest accepted consecutive-error threshold before a run checks in.
const MAX_SANDBOX_AGENT_ERROR_CHECKIN: u32 = 100;

/// Host-tunable chat compaction cadence and retention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
pub struct CompactionSettings {
    /// Compact when unabridged tokens exceed this fraction of the context window.
    pub threshold_fraction: f64,
    /// After compaction, keep about this fraction of the window as raw recent history.
    pub target_fraction: f64,
    /// Absolute floor applied before scaling by context window.
    pub min_threshold_tokens: u32,
    /// Newest durable messages that must never enter the compacted prefix.
    pub protect_recent_messages: u32,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            threshold_fraction: DEFAULT_COMPACTION_THRESHOLD_FRACTION,
            target_fraction: DEFAULT_COMPACTION_TARGET_FRACTION,
            min_threshold_tokens: DEFAULT_COMPACTION_MIN_THRESHOLD_TOKENS as u32,
            protect_recent_messages: DEFAULT_COMPACTION_PROTECT_RECENT_MESSAGES as u32,
        }
    }
}

impl From<&CompactionSettings> for CompactionPolicy {
    fn from(settings: &CompactionSettings) -> Self {
        Self {
            threshold_fraction: settings.threshold_fraction,
            target_fraction: settings.target_fraction,
            min_threshold_tokens: settings.min_threshold_tokens as usize,
            protect_recent_messages: settings.protect_recent_messages as usize,
        }
    }
}

/// A reader's explicit deviation from a model's curated `recommended` flag.
///
/// Only deviations are stored. Effective visibility is the catalog's
/// `recommended` flag flipped by a matching override, so a catalog refresh
/// gives new models their curated default without a reconciliation step, and
/// "we changed the default" stays distinguishable from "you chose" forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ModelVisibility {
    /// Show a model the catalog does not recommend.
    Show,
    /// Hide a model the catalog recommends.
    Hide,
}

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
    /// Preferred maximum concurrent background agents. Spawn unsettled
    /// children on one origin turn are further capped at
    /// [`AgentRun::MAX_ACTIVE_BACKGROUND_AGENTS`] (wait_for_agents membership).
    pub max_active_background_agents: u32,
    /// Model steps a background agent takes before it must check in.
    ///
    /// A cadence, not a cap: reaching it never fails the run — the agent wraps
    /// up with what it has and reports back for direction.
    pub sandbox_agent_checkin_steps: u32,
    /// Consecutive failed tool calls after which a background agent checks in.
    pub sandbox_agent_error_checkin: u32,
    /// When and how hard semantic compaction may run.
    #[serde(default)]
    pub compaction: CompactionSettings,
    /// Per-model deviations from the catalog's `recommended` flag, keyed by the
    /// same provider-qualified selection key `ModelInfo.key` and a chat's model
    /// carry (`"<provider>::<id>"`).
    ///
    /// Deviations only: a model with no entry uses its catalog default, and
    /// resetting one to the default means sending the map without that key.
    /// `PUT /settings` **replaces this map wholesale** rather than merging, so
    /// a writer sends the complete set of deviations it wants to persist.
    ///
    /// Visibility is a picker concern: the server stores and serves this map
    /// and never filters `GET /models` by it. A hidden model remains fully
    /// valid for existing chats, replay, and explicit selection.
    #[serde(default)]
    pub model_visibility_overrides: BTreeMap<String, ModelVisibility>,
    /// Whether the computer-use capability (screen capture + app control) is
    /// enabled. Read at boot; turning it off unregisters the tools on the next
    /// launch.
    pub computer_use_enabled: bool,
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
    pub network_policy: Option<tidebreak_core::NetworkPolicy>,
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
    #[serde(default)]
    pub sandbox_agent_checkin_steps: Option<u32>,
    #[serde(default)]
    pub sandbox_agent_error_checkin: Option<u32>,
    #[serde(default)]
    pub compaction: Option<CompactionSettingsUpdate>,
    /// The complete set of per-model visibility deviations to persist.
    ///
    /// Absent leaves the stored map unchanged; present **replaces** it, so an
    /// empty map clears every override and returns every model to its curated
    /// default. Merging was rejected because it gives a client no way to
    /// express a deletion at all.
    #[serde(default)]
    pub model_visibility_overrides: Option<BTreeMap<String, ModelVisibility>>,
    /// Set the computer-use master switch. Absent leaves it unchanged. Applies
    /// at the next boot (the tools register or not then).
    #[serde(default)]
    pub computer_use_enabled: Option<bool>,
}

/// Partial update for [`CompactionSettings`]. Absent fields leave the current
/// value unchanged; the merged result is validated together.
#[derive(Debug, Default, Deserialize)]
pub struct CompactionSettingsUpdate {
    #[serde(default)]
    pub threshold_fraction: Option<f64>,
    #[serde(default)]
    pub target_fraction: Option<f64>,
    #[serde(default)]
    pub min_threshold_tokens: Option<u32>,
    #[serde(default)]
    pub protect_recent_messages: Option<u32>,
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
pub async fn get_settings(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<Settings>, ServerError> {
    Ok(Json(
        read_settings(&state, &auth.principal.owner_id()).await?,
    ))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are touched.
pub async fn put_settings(
    State(state): State<AppState>,
    auth: AuthContext,
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
        // Stored value may exceed the per-turn unsettled wait ceiling; spawn
        // admission clamps with AgentRun::MAX_ACTIVE_BACKGROUND_AGENTS.
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
    if let Some(steps) = body.sandbox_agent_checkin_steps {
        if steps == 0 || steps > MAX_SANDBOX_AGENT_CHECKIN_STEPS {
            return Err(ServerError::bad_request(format!(
                "sandbox_agent_checkin_steps must be in 1..={MAX_SANDBOX_AGENT_CHECKIN_STEPS}"
            )));
        }
        state
            .store
            .set_setting(
                SANDBOX_AGENT_CHECKIN_STEPS_SETTING,
                &serde_json::json!(steps),
            )
            .await?;
    }
    if let Some(errors) = body.sandbox_agent_error_checkin {
        if errors == 0 || errors > MAX_SANDBOX_AGENT_ERROR_CHECKIN {
            return Err(ServerError::bad_request(format!(
                "sandbox_agent_error_checkin must be in 1..={MAX_SANDBOX_AGENT_ERROR_CHECKIN}"
            )));
        }
        state
            .store
            .set_setting(
                SANDBOX_AGENT_ERROR_CHECKIN_SETTING,
                &serde_json::json!(errors),
            )
            .await?;
    }
    if let Some(update) = body.compaction {
        let mut next = read_compaction_settings(&*state.store).await?;
        if let Some(value) = update.threshold_fraction {
            next.threshold_fraction = value;
        }
        if let Some(value) = update.target_fraction {
            next.target_fraction = value;
        }
        if let Some(value) = update.min_threshold_tokens {
            next.min_threshold_tokens = value;
        }
        if let Some(value) = update.protect_recent_messages {
            next.protect_recent_messages = value;
        }
        validate_compaction_settings(&next)?;
        write_compaction_settings(&*state.store, &next).await?;
    }
    if let Some(overrides) = body.model_visibility_overrides {
        validate_model_visibility_overrides(&overrides)?;
        state
            .store
            .set_setting(
                MODEL_VISIBILITY_OVERRIDES_SETTING,
                &serde_json::json!(overrides),
            )
            .await?;
    }
    if let Some(enabled) = body.computer_use_enabled {
        state
            .store
            .set_setting(
                crate::routes::COMPUTER_USE_ENABLED_SETTING,
                &serde_json::json!(enabled),
            )
            .await?;
    }
    Ok(Json(
        read_settings(&state, &auth.principal.owner_id()).await?,
    ))
}

/// The settings both handlers return, read back from the store so a response
/// always reflects what was persisted rather than what was requested.
async fn read_settings(state: &AppState, owner: &OwnerId) -> Result<Settings, ServerError> {
    Ok(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
        chat_defaults: read_sticky_chat_defaults(state, owner).await?,
        max_active_background_agents: read_max_active_background_agents(&*state.store).await?,
        sandbox_agent_checkin_steps: read_sandbox_agent_checkin_steps(&*state.store).await?,
        sandbox_agent_error_checkin: read_sandbox_agent_error_checkin(&*state.store).await?,
        compaction: read_compaction_settings(&*state.store).await?,
        model_visibility_overrides: read_model_visibility_overrides(&*state.store).await?,
        computer_use_enabled: read_computer_use_enabled(&*state.store).await?,
    })
}

/// Whether computer use is enabled. Default on; an explicit `false` disables
/// it (the tools unregister at the next boot).
pub(crate) async fn read_computer_use_enabled(store: &dyn Store) -> tidebreak_core::Result<bool> {
    Ok(store
        .get_setting(crate::routes::COMPUTER_USE_ENABLED_SETTING)
        .await?
        .and_then(|value| value.as_bool())
        .unwrap_or(true))
}

/// The largest number of stored visibility deviations.
///
/// The overrides live in one settings row, and a client only ever needs one
/// entry per catalog model. This bounds an unbounded map without constraining
/// any real use.
const MAX_MODEL_VISIBILITY_OVERRIDES: usize = 512;

fn validate_model_visibility_overrides(
    overrides: &BTreeMap<String, ModelVisibility>,
) -> Result<(), ServerError> {
    if overrides.len() > MAX_MODEL_VISIBILITY_OVERRIDES {
        return Err(ServerError::bad_request(format!(
            "model_visibility_overrides must contain at most {MAX_MODEL_VISIBILITY_OVERRIDES} entries",
        )));
    }
    for key in overrides.keys() {
        // Structural validation only. Whether the key names a model in today's
        // catalog is deliberately not checked: an override must survive a
        // provider being decredentialed or a model briefly leaving the catalog.
        if crate::model_registry::parse_selection_key(key).is_none() {
            return Err(ServerError::bad_request(format!(
                "`{key}` is not a provider-qualified model key",
            )));
        }
    }
    Ok(())
}

pub(crate) async fn read_model_visibility_overrides(
    store: &dyn Store,
) -> tidebreak_core::Result<BTreeMap<String, ModelVisibility>> {
    Ok(store
        .get_setting(MODEL_VISIBILITY_OVERRIDES_SETTING)
        .await?
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default())
}

pub(crate) async fn read_max_active_background_agents(
    store: &dyn Store,
) -> tidebreak_core::Result<u32> {
    Ok(store
        .get_setting(MAX_ACTIVE_BACKGROUND_AGENTS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|limit| *limit > 0 && *limit <= AgentRun::MAX_CONCURRENCY_LIMIT)
        .unwrap_or(AgentRun::DEFAULT_MAX_ACTIVE_BACKGROUND_AGENTS))
}

/// The stored check-in cadence, as the number of model steps one background
/// run may take before it must wrap up and report back.
pub(crate) async fn read_sandbox_agent_checkin_steps(
    store: &dyn Store,
) -> tidebreak_core::Result<u32> {
    Ok(read_sandbox_agent_checkin_steps_override(store)
        .await?
        .unwrap_or(tidebreak_core::DEFAULT_SANDBOX_AGENT_CHECKIN_STEPS as u32))
}

/// The stored cadence only if one was explicitly set.
///
/// The sandbox worker distinguishes "no choice recorded" (keep the boot
/// configuration's step budget) from "the user chose a cadence" (override it),
/// so the two cannot fight over runs when no setting exists.
pub(crate) async fn read_sandbox_agent_checkin_steps_override(
    store: &dyn Store,
) -> tidebreak_core::Result<Option<u32>> {
    Ok(store
        .get_setting(SANDBOX_AGENT_CHECKIN_STEPS_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|steps| *steps > 0 && *steps <= MAX_SANDBOX_AGENT_CHECKIN_STEPS))
}

/// The stored consecutive tool-error threshold after which a background run
/// checks in rather than continuing to thrash.
pub(crate) async fn read_sandbox_agent_error_checkin(
    store: &dyn Store,
) -> tidebreak_core::Result<u32> {
    Ok(store
        .get_setting(SANDBOX_AGENT_ERROR_CHECKIN_SETTING)
        .await?
        .and_then(|value| serde_json::from_value::<u32>(value).ok())
        .filter(|errors| *errors > 0 && *errors <= MAX_SANDBOX_AGENT_ERROR_CHECKIN)
        .unwrap_or(tidebreak_core::DEFAULT_SANDBOX_AGENT_ERROR_CHECKIN as u32))
}

/// Absolute floor / ceiling for `min_threshold_tokens`.
const MIN_COMPACTION_MIN_THRESHOLD_TOKENS: u32 = 1_000;
const MAX_COMPACTION_MIN_THRESHOLD_TOKENS: u32 = 2_000_000;

fn validate_compaction_settings(settings: &CompactionSettings) -> Result<(), ServerError> {
    if !(settings.threshold_fraction > 0.0 && settings.threshold_fraction <= 1.0) {
        return Err(ServerError::bad_request(
            "compaction.threshold_fraction must be in (0, 1]",
        ));
    }
    if !(settings.target_fraction > 0.0 && settings.target_fraction <= 1.0) {
        return Err(ServerError::bad_request(
            "compaction.target_fraction must be in (0, 1]",
        ));
    }
    // Both fractions are already known to be ordinary numbers in (0, 1], so
    // this reads the same as rejecting `!(threshold > target)` without the
    // negated partial-order comparison.
    if settings.threshold_fraction <= settings.target_fraction {
        return Err(ServerError::bad_request(
            "compaction.threshold_fraction must be greater than compaction.target_fraction",
        ));
    }
    if !(MIN_COMPACTION_MIN_THRESHOLD_TOKENS..=MAX_COMPACTION_MIN_THRESHOLD_TOKENS)
        .contains(&settings.min_threshold_tokens)
    {
        return Err(ServerError::bad_request(format!(
            "compaction.min_threshold_tokens must be in {MIN_COMPACTION_MIN_THRESHOLD_TOKENS}..={MAX_COMPACTION_MIN_THRESHOLD_TOKENS}"
        )));
    }
    if settings.protect_recent_messages < 1 {
        return Err(ServerError::bad_request(
            "compaction.protect_recent_messages must be >= 1",
        ));
    }
    Ok(())
}

pub(crate) async fn read_compaction_settings(
    store: &dyn Store,
) -> tidebreak_core::Result<CompactionSettings> {
    let defaults = CompactionSettings::default();
    let settings = CompactionSettings {
        threshold_fraction: store
            .get_setting(COMPACTION_THRESHOLD_FRACTION_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<f64>(value).ok())
            .filter(|value| *value > 0.0 && *value <= 1.0)
            .unwrap_or(defaults.threshold_fraction),
        target_fraction: store
            .get_setting(COMPACTION_TARGET_FRACTION_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<f64>(value).ok())
            .filter(|value| *value > 0.0 && *value <= 1.0)
            .unwrap_or(defaults.target_fraction),
        min_threshold_tokens: store
            .get_setting(COMPACTION_MIN_THRESHOLD_TOKENS_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<u32>(value).ok())
            .filter(|value| {
                (MIN_COMPACTION_MIN_THRESHOLD_TOKENS..=MAX_COMPACTION_MIN_THRESHOLD_TOKENS)
                    .contains(value)
            })
            .unwrap_or(defaults.min_threshold_tokens),
        protect_recent_messages: store
            .get_setting(COMPACTION_PROTECT_RECENT_MESSAGES_SETTING)
            .await?
            .and_then(|value| serde_json::from_value::<u32>(value).ok())
            .filter(|value| *value >= 1)
            .unwrap_or(defaults.protect_recent_messages),
    };
    // Independently-stored keys can drift into an impossible pair; fall closed
    // to defaults rather than hand the agent a policy that cannot resolve.
    if settings.threshold_fraction <= settings.target_fraction {
        return Ok(defaults);
    }
    Ok(settings)
}

async fn write_compaction_settings(
    store: &dyn Store,
    settings: &CompactionSettings,
) -> Result<(), ServerError> {
    store
        .set_setting(
            COMPACTION_THRESHOLD_FRACTION_SETTING,
            &serde_json::json!(settings.threshold_fraction),
        )
        .await?;
    store
        .set_setting(
            COMPACTION_TARGET_FRACTION_SETTING,
            &serde_json::json!(settings.target_fraction),
        )
        .await?;
    store
        .set_setting(
            COMPACTION_MIN_THRESHOLD_TOKENS_SETTING,
            &serde_json::json!(settings.min_threshold_tokens),
        )
        .await?;
    store
        .set_setting(
            COMPACTION_PROTECT_RECENT_MESSAGES_SETTING,
            &serde_json::json!(settings.protect_recent_messages),
        )
        .await?;
    Ok(())
}

/// Resolve the host compaction policy for the next turn.
pub(crate) async fn read_compaction_policy(
    store: &dyn Store,
) -> tidebreak_core::Result<CompactionPolicy> {
    Ok(CompactionPolicy::from(
        &read_compaction_settings(store).await?,
    ))
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
) -> Result<Json<ExecConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::config_info(&state.config, &*state.store, &*state.secrets).await?,
    ))
}

/// `PUT /code-execution` — select a fixed provider and bounded host timeout.
pub async fn put_code_execution_config(
    State(state): State<AppState>,
    Json(body): Json<ExecConfigUpdate>,
) -> Result<Json<ExecConfigInfo>, ServerError> {
    Ok(Json(
        code_execution::update_config(&state.config, &*state.store, &*state.secrets, body).await?,
    ))
}

/// `GET /code-execution/credentials` — readiness for the fixed E2B and Daytona
/// credential slots. Local execution needs no credential and is absent here.
pub async fn get_code_execution_credentials(
    State(state): State<AppState>,
) -> Json<ExecCredentialsInfo> {
    Json(code_execution::credentials_info(&*state.secrets).await)
}

const MAX_CODE_EXECUTION_CREDENTIAL_BYTES: usize = 8 * 1024;

/// Body of `PUT /code-execution/credentials/{provider}`. Debug output always
/// redacts the credential.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecCredentialUpdate {
    pub api_key: String,
}

impl std::fmt::Debug for ExecCredentialUpdate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecCredentialUpdate")
            .field("api_key", &"***")
            .finish()
    }
}

/// Store a managed provider key in its fixed slot without changing selection.
pub async fn put_code_execution_credential(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<ExecCredentialUpdate>,
) -> Result<Json<ExecCredentialReadiness>, ServerError> {
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
) -> Result<Json<ExecCredentialReadiness>, ServerError> {
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
        .header("x-tidebreak-preview-width", rendered.width.to_string())
        .header("x-tidebreak-preview-height", rendered.height.to_string())
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
            "Tidebreak could not render this revision on this device.",
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
/// Owner-scoped within the deployment. `model` deliberately gets its own key
/// rather than reusing the global `model` selection: seeding is a
/// creation-time copy into the new chat, so picking a model in one chat never
/// retargets existing chats that ride the configured default.
pub(super) const STICKY_MODEL_KEY: &str = "chat_default.model";
pub(super) const STICKY_REASONING_EFFORT_KEY: &str = "chat_default.reasoning_effort";
pub(super) const STICKY_PERMISSION_MODE_KEY: &str = "chat_default.permission_mode";
pub(super) const STICKY_NETWORK_POLICY_KEY: &str = "chat_default.network_policy";

/// Resolve a sticky setting's durable key for one principal. The local profile
/// keeps the historical key so existing desktop defaults survive upgrade;
/// named self-host users receive disjoint keys.
pub(super) fn sticky_default_key(owner: &OwnerId, key: &str) -> String {
    if owner.is_local() {
        key.to_owned()
    } else {
        format!("{key}.owner.{}", owner.as_str())
    }
}

/// Read one sticky new-chat default. A stored value this build no longer
/// recognizes reads as unset rather than failing the create.
pub(super) async fn read_sticky_default<T: serde::de::DeserializeOwned>(
    store: &dyn Store,
    owner: &OwnerId,
    key: &str,
) -> tidebreak_core::Result<Option<T>> {
    let key = sticky_default_key(owner, key);
    Ok(store
        .get_setting(&key)
        .await?
        .and_then(|value| serde_json::from_value(value).ok()))
}

/// Encode one sticky new-chat default for a direct or transactional write.
pub(super) fn sticky_default_value<T: Serialize>(
    value: Option<&T>,
) -> Result<serde_json::Value, ServerError> {
    match value {
        Some(value) => serde_json::to_value(value)
            .map_err(|_| ServerError::internal("could not encode a sticky chat default")),
        None => Ok(serde_json::Value::Null),
    }
}

/// Record (or clear, with `None`) one sticky new-chat default.
pub(super) async fn write_sticky_default<T: Serialize>(
    store: &dyn Store,
    owner: &OwnerId,
    key: &str,
    value: Option<&T>,
) -> Result<(), ServerError> {
    let value = sticky_default_value(value)?;
    Ok(store
        .set_setting(&sticky_default_key(owner, key), &value)
        .await?)
}

/// Read every sticky new-chat default, the permission mode clamped to any
/// managed ceiling — what `POST /chats` will seed, and therefore what a
/// composer should display before the chat exists.
pub(super) async fn read_sticky_chat_defaults(
    state: &AppState,
    owner: &OwnerId,
) -> Result<StickyChatDefaults, ServerError> {
    let store = &*state.store;
    let permission_mode =
        match read_sticky_default(store, owner, STICKY_PERMISSION_MODE_KEY).await? {
            // The managed ceiling clamps a sticky mode recorded before the
            // policy arrived: a remembered `allow` under an `ask` ceiling seeds
            // (and reads back) `ask`, mirroring the turn gate's treatment of
            // stored over-ceiling modes.
            Some(mode) => {
                crate::managed_policy::resolve(&*state.provisioned_policy, &*state.os_policy)?
                    .clamp_permission_mode(Some(mode))
            }
            None => None,
        };
    Ok(StickyChatDefaults {
        model: read_sticky_default(store, owner, STICKY_MODEL_KEY).await?,
        reasoning_effort: read_sticky_default(store, owner, STICKY_REASONING_EFFORT_KEY).await?,
        permission_mode,
        network_policy: read_sticky_default(store, owner, STICKY_NETWORK_POLICY_KEY).await?,
    })
}
