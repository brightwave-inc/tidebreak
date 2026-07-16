//! HTTP and WebSocket route handlers.
//!
//! Document lifecycle and search handlers live in the dedicated `document`
//! submodule; settings, providers, projects, chats, and event streaming remain
//! here.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use openwave_core::{
    AcceptTurnOutcome, AcceptTurnSteerOutcome, AgentRun, AgentRunExecution, AgentRunStatus,
    ApprovalDecision, CallId, Chat, ChatId, Project, ProjectId, RequestTurnCancellationOutcome,
    SandboxToolCall, SandboxToolCallStatus, SecretProvider, SequencedEvent, Store, TurnId,
    TurnSteer, TurnSteerId,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

mod client_execution;
mod document;
mod root_attachment;
pub use client_execution::*;
pub use document::*;
pub use root_attachment::*;

/// The store-settings key for the selected model.
const MODEL_SETTING: &str = "model";

/// Runtime settings a client can read. The API key itself is never returned —
/// it lives in the `SecretProvider`, not the store — only whether one is set.
#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    /// The model turns run against, or `None` to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether a model API key is configured (never the key itself).
    pub has_api_key: bool,
}

/// Body of `PUT /settings`. Each field is a *double* option so an absent key is
/// distinguished from an explicit `null`: absent leaves the value unchanged,
/// `null` resets it to the server default, and a value sets it.
#[derive(Debug, Deserialize)]
pub struct SettingsUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
}

/// Deserialize a present field (including JSON `null`) as `Some(..)`; `#[serde(default)]`
/// supplies `None` when the field is absent.
fn double_option<'de, D, T>(deserializer: D) -> std::result::Result<Option<Option<T>>, D::Error>
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
    }))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are touched.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<SettingsUpdate>,
) -> Result<Json<Settings>, ServerError> {
    match body.model {
        // Absent: leave the model unchanged.
        None => {}
        // Explicit null: reset to the server default (stored as JSON null, which
        // `read_model` reads back as "unset").
        Some(None) => {
            state
                .store
                .set_setting(MODEL_SETTING, &serde_json::Value::Null)
                .await?;
        }
        // A value: reject empty (it would break every turn), else set it.
        Some(Some(model)) => {
            if model.is_empty() {
                return Err(ServerError::bad_request("model must not be empty"));
            }
            state
                .store
                .set_setting(MODEL_SETTING, &serde_json::json!(model))
                .await?;
        }
    }
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
        has_api_key: has_api_key(&*state.secrets).await,
    }))
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
    let provider = parse_web_search_provider(&provider)?;
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
    let provider = parse_web_search_provider(&provider)?;
    Ok(Json(
        web_search::delete_credential(&*state.secrets, provider).await?,
    ))
}

fn parse_web_search_provider(
    value: &str,
) -> std::result::Result<openwave_web_search::WebSearchProviderKind, ServerError> {
    match value {
        "exa" => Ok(openwave_web_search::WebSearchProviderKind::Exa),
        "tavily" => Ok(openwave_web_search::WebSearchProviderKind::Tavily),
        _ => Err(ServerError::not_found(format!(
            "unknown web search provider kind: {value}"
        ))),
    }
}

/// The configured model, if any.
async fn read_model(store: &dyn Store) -> Result<Option<String>, ServerError> {
    Ok(store
        .get_setting(MODEL_SETTING)
        .await?
        .and_then(|value| value.as_str().map(str::to_owned)))
}

/// Whether any model provider credential is configured — stored or via the
/// env fallbacks the resolver also honors. Prefer `GET /providers` for
/// per-kind detail; this field is the legacy "is anything ready?" signal.
async fn has_api_key(secrets: &dyn SecretProvider) -> bool {
    for &kind in ProviderKind::ALL {
        if providers::has_credential(secrets, kind).await {
            return true;
        }
    }
    false
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
    providers::delete_credential(&*state.secrets, ProviderKind::Anthropic).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Response for `GET /providers`.
#[derive(Debug, Serialize)]
pub struct ProvidersList {
    pub providers: Vec<ProviderInfo>,
}

/// `GET /providers` — every known provider kind and its current config.
pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersList>, ServerError> {
    Ok(Json(ProvidersList {
        providers: providers::list_providers(&*state.store, &*state.secrets).await?,
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
    let info = providers::update_provider(&*state.store, &*state.secrets, kind, body).await?;
    Ok(Json(info))
}

/// `DELETE /providers/{kind}/credential` — remove the stored credential. `204`.
pub async fn delete_provider_credential(
    State(state): State<AppState>,
    Path(kind): Path<String>,
) -> Result<StatusCode, ServerError> {
    let kind = ProviderKind::parse(&kind)
        .ok_or_else(|| ServerError::not_found(format!("unknown provider kind: {kind}")))?;
    providers::delete_credential(&*state.secrets, kind).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A selectable model in the catalog.
#[derive(Debug, Serialize)]
pub struct ModelInfo {
    /// The identifier passed to the provider and stored as `chat.model`.
    pub id: String,
    /// The provider that serves the model.
    pub provider: String,
}

/// Response for `GET /models`.
#[derive(Debug, Serialize)]
pub struct ModelCatalog {
    /// The models a client can select from.
    pub models: Vec<ModelInfo>,
}

/// `GET /models` — the catalog a chat's model selector chooses from.
///
/// Models of enabled, credentialed providers. Falls back to Anthropic's curated
/// list when nothing is configured yet so the selector isn't empty on first run.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<ModelCatalog>, ServerError> {
    let models = providers::catalog_models(&*state.store, &*state.secrets)
        .await?
        .into_iter()
        .map(|(kind, id)| ModelInfo {
            id: id.to_string(),
            provider: kind.as_str().to_string(),
        })
        .collect();
    Ok(Json(ModelCatalog { models }))
}

/// Body of `POST /projects`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    let project = Project {
        id: ProjectId::new(),
        title: body.title,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    state.store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /projects` — list projects, most-recently-created first.
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<Project>>, ServerError> {
    Ok(Json(state.store.list_projects().await?))
}

/// `GET /projects/{id}` — fetch one project, or `404`.
pub async fn get_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
) -> Result<Json<Project>, ServerError> {
    state
        .store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
}

/// Body of `POST /chats`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateChat {
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
    /// Optional project to file this chat under; omitted for a loose chat.
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// Optional model for this chat; omitted to use the configured default.
    #[serde(default)]
    pub model: Option<String>,
}

/// `POST /chats` — create a chat and return it (`201 Created`).
pub async fn create_chat(
    State(state): State<AppState>,
    Json(body): Json<CreateChat>,
) -> Result<impl IntoResponse, ServerError> {
    // Return a product-facing 400 for an unknown project. The Store and schema
    // independently enforce the same membership invariant inside insertion.
    if let Some(project_id) = body.project_id {
        state
            .store
            .get_project(project_id)
            .await?
            .ok_or_else(|| ServerError::bad_request(format!("project {project_id} not found")))?;
    }
    if body.model.as_deref().is_some_and(str::is_empty) {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: body.title,
        model: body.model,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    let chat = state.store.create_chat_with_project_defaults(&chat).await?;
    Ok((StatusCode::CREATED, Json(chat)))
}

/// Body of `PATCH /chats/{id}`. A double option (like `PUT /settings`): absent
/// leaves the model unchanged, `null` clears it (fall back to the default), and a
/// value sets it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
}

/// `PATCH /chats/{id}` — update a chat's model selection; returns the chat. This
/// is what a chat UI's model selector writes to.
pub async fn patch_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    let mut chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    if let Some(model) = body.model {
        if model.as_deref().is_some_and(str::is_empty) {
            return Err(ServerError::bad_request("model must not be empty"));
        }
        state.store.set_chat_model(id, model.clone()).await?;
        chat.model = model;
    }
    Ok(Json(chat))
}

/// `GET /chats` — list chats, most-recently-created first.
pub async fn list_chats(State(state): State<AppState>) -> Result<Json<Vec<Chat>>, ServerError> {
    Ok(Json(state.store.list_chats().await?))
}

/// `GET /chats/{id}` — fetch one chat, or `404`.
pub async fn get_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<Chat>, ServerError> {
    state
        .store
        .get_chat(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))
}

/// Renderer-safe state for one agent run.
///
/// Worker lease tokens, delegated inputs, scheduling budgets, and other
/// executor-facing fields intentionally remain inside the server/store boundary.
#[derive(Debug, Serialize)]
pub struct AgentRunSnapshot {
    pub id: openwave_core::AgentRunId,
    pub parent_id: Option<openwave_core::AgentRunId>,
    pub execution: AgentRunExecution,
    pub status: AgentRunStatus,
    pub started_at: Option<chrono::DateTime<Utc>>,
    pub finished_at: Option<chrono::DateTime<Utc>>,
    /// Stable, bounded classification suitable for renderer display.
    pub last_error_code: Option<String>,
    /// The currently checkpointed, renderer-safe sandbox activity, if any.
    ///
    /// This is intentionally a small fixed vocabulary. It never exposes tool
    /// arguments, results, provider call identities, executor leases, or raw
    /// executor diagnostics.
    pub activity: Option<SandboxActivitySnapshot>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl AgentRunSnapshot {
    fn from_run(run: AgentRun, activity: Option<SandboxActivitySnapshot>) -> Self {
        Self {
            id: run.id,
            parent_id: run.parent_id,
            execution: run.execution,
            status: run.status,
            started_at: run.started_at,
            finished_at: run.finished_at,
            last_error_code: run.last_error_code,
            activity,
            created_at: run.created_at,
            updated_at: run.updated_at,
        }
    }
}

/// Fixed, renderer-safe names for live sandbox work.
///
/// Adding a new durable sandbox tool does not automatically expose it to a
/// renderer: it must be deliberately admitted here with a safe label.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxActivityKind {
    WebSearch,
}

/// Coarse checkpoint lifecycle suitable for display.
///
/// This intentionally does not mirror all durable executor states; only live
/// work is represented, and terminal checkpoints produce no activity.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxActivityStatus {
    Waiting,
    Running,
}

/// Renderer-safe projection of one live sandbox checkpoint.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SandboxActivitySnapshot {
    pub kind: SandboxActivityKind,
    pub status: SandboxActivityStatus,
}

fn sandbox_activity(calls: &[SandboxToolCall]) -> Option<SandboxActivitySnapshot> {
    // A sandbox can have at most one live checkpoint. Inspecting the latest
    // durable live checkpoint also prevents an older, completed activity from
    // lingering in the UI after the run has advanced.
    let call = calls.iter().rev().find(|call| !call.status.is_terminal())?;
    let kind = match call.name.as_str() {
        "web_search" => SandboxActivityKind::WebSearch,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = match call.status {
        SandboxToolCallStatus::Accepted => SandboxActivityStatus::Waiting,
        SandboxToolCallStatus::Claimed => SandboxActivityStatus::Running,
        SandboxToolCallStatus::Completed
        | SandboxToolCallStatus::Failed
        | SandboxToolCallStatus::Cancelled => return None,
        _ => return None,
    };
    Some(SandboxActivitySnapshot { kind, status })
}

/// `GET /chats/{id}/agent-runs` — list renderer-safe execution state.
pub async fn list_agent_runs(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<Vec<AgentRunSnapshot>>, ServerError> {
    state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let runs = state.store.list_agent_runs(id).await?;
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        let activity = if run.execution == AgentRunExecution::Sandbox {
            let calls = state
                .store
                .list_sandbox_tool_calls_for_agent_run(run.id)
                .await?;
            sandbox_activity(&calls)
        } else {
            None
        };
        snapshots.push(AgentRunSnapshot::from_run(run, activity));
    }
    Ok(Json(snapshots))
}

/// Body of `POST /chats/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct PostMessage {
    /// Stable client-generated identity for acceptance and ambiguous retries.
    pub turn_id: TurnId,
    /// The user's input for this turn.
    pub content: String,
}

/// `POST /chats/{id}/messages` — durably accept a message and queue its turn.
///
/// Returns `202 Accepted` after the input and queued turn commit; a supervised
/// worker claims it asynchronously and journals events for replay/live delivery.
/// Repeating an exact `turn_id` and payload is idempotent. `404` if the chat
/// doesn't exist, `409` if the identity names different input or another turn
/// already owns the chat's single durable live slot.
pub async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<PostMessage>,
) -> Result<StatusCode, ServerError> {
    if body.turn_id.0.is_nil() {
        return Err(ServerError::bad_request("turn_id must not be nil"));
    }
    if body.content.trim().is_empty() || body.content.contains('\0') {
        return Err(ServerError::bad_request(
            "message content must be non-empty and contain no NUL characters",
        ));
    }
    let chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    // An ambiguous HTTP retry names only its turn and content, not the resolved
    // model snapshot. Reuse the first acceptance's immutable model so a settings
    // change between attempts cannot turn the same request into a conflict.
    let model = if let Some(existing) = state.store.get_turn_run(body.turn_id).await? {
        if existing.chat_id != id {
            return Err(ServerError::conflict(format!(
                "turn {} was already accepted by another chat",
                body.turn_id
            )));
        }
        existing.model
    } else {
        // New-turn resolution order: chat override, global default, boot default.
        match chat.model.clone() {
            Some(model) => model,
            None => read_model(&*state.store)
                .await?
                .unwrap_or_else(|| state.agent_config.model.clone()),
        }
    };
    match state
        .store
        .accept_turn(body.turn_id, id, &model, &body.content)
        .await?
    {
        AcceptTurnOutcome::Accepted(_) | AcceptTurnOutcome::Existing(_) => {
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnOutcome::IdentityConflict => {
            // Concurrent identical requests can resolve different mutable model
            // defaults before either commits. Retry against the winner's immutable
            // model so the wire identity remains `(chat, turn_id, content)`.
            let Some(existing) = state.store.get_turn_run(body.turn_id).await? else {
                return Err(ServerError::conflict(format!(
                    "turn {} was accepted with conflicting request data",
                    body.turn_id
                )));
            };
            if existing.chat_id == id
                && matches!(
                    state
                        .store
                        .accept_turn(body.turn_id, id, &existing.model, &body.content)
                        .await?,
                    AcceptTurnOutcome::Existing(_)
                )
            {
                state.turn_job_wake.notify_one();
                Ok(StatusCode::ACCEPTED)
            } else {
                Err(ServerError::conflict(format!(
                    "turn {} was already accepted with different input",
                    body.turn_id
                )))
            }
        }
        AcceptTurnOutcome::ChatBusy(active) => Err(ServerError::conflict(format!(
            "chat {id} already has active turn {}",
            active.id
        ))),
    }
}

/// Body of `POST /chats/{id}/steer`.
#[derive(Debug, Deserialize)]
pub struct SteerBody {
    /// Stable client-generated identity for admission and ambiguous retries.
    pub steer_id: TurnSteerId,
    /// Exact durable turn to steer.
    pub turn_id: TurnId,
    /// User text to inject into the running turn.
    pub content: String,
    /// When true, preempt the provider stream immediately; otherwise the message
    /// waits for the next step boundary.
    #[serde(default)]
    pub interrupt: bool,
}

/// `POST /chats/{id}/steer` — durably enqueue an instruction for an active turn.
///
/// `202 Accepted` only after the exact instruction commits. A local notification
/// can reduce delivery latency, but the claimed worker always applies pending
/// instructions from the database. Repeating the same identity and payload is
/// idempotent. `404` if the chat doesn't exist, `409` for conflicting identity or
/// unavailable turn, and `400` for malformed input.
pub async fn post_steer(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<SteerBody>,
) -> Result<StatusCode, ServerError> {
    if body.steer_id.0.is_nil() {
        return Err(ServerError::bad_request("steer_id must not be nil"));
    }
    if body.content.trim().is_empty()
        || body.content.contains('\0')
        || body.content.chars().count() > TurnSteer::MAX_CONTENT_LEN
    {
        return Err(ServerError::bad_request(
            "steer content must be non-empty, contain no NUL characters, and fit the size limit",
        ));
    }
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    match state
        .store
        .accept_turn_steer(
            body.steer_id,
            body.turn_id,
            id,
            &body.content,
            body.interrupt,
        )
        .await?
    {
        AcceptTurnSteerOutcome::Accepted(_) | AcceptTurnSteerOutcome::Existing(_) => {
            state
                .active_turns
                .signal_steer(id, body.turn_id, body.interrupt);
            state.turn_job_wake.notify_one();
            Ok(StatusCode::ACCEPTED)
        }
        AcceptTurnSteerOutcome::IdentityConflict => Err(ServerError::conflict(format!(
            "steer {} was already used by different request data",
            body.steer_id
        ))),
        AcceptTurnSteerOutcome::TurnUnavailable => Err(ServerError::conflict(format!(
            "turn {} is not accepting steering instructions",
            body.turn_id
        ))),
    }
}

/// `POST /chats/{id}/cancel` — durably cancel one exact turn for a chat.
///
/// Queued work becomes terminal atomically; running work enters `cancelling`
/// until its exact worker acknowledges quiescence. `202 Accepted` is idempotent
/// for cancelling/cancelled retries. `404` if the chat doesn't exist, `409` if
/// the turn does not belong to the chat or can no longer accept cancellation.
#[derive(Debug, Deserialize)]
pub struct CancelBody {
    /// Exact durable turn to cancel.
    pub turn_id: TurnId,
}

pub async fn post_cancel(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<CancelBody>,
) -> Result<StatusCode, ServerError> {
    // Distinguish "unknown chat" (404) from "known chat, nothing running" (409).
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    if !state
        .store
        .get_turn_run(body.turn_id)
        .await?
        .is_some_and(|turn| turn.chat_id == id)
    {
        return Err(ServerError::conflict(format!(
            "turn {} does not belong to chat {id}",
            body.turn_id
        )));
    }
    let resolution = loop {
        if let Some(resolution) = state
            .store
            .request_turn_cancellation_and_append_event(body.turn_id, Utc::now())
            .await?
        {
            break resolution;
        }
        // A heartbeat can advance `updated_at` after this request captures its
        // operational timestamp. Retry the same empty command with fresh time;
        // the store serializes it against the heartbeat and terminal decisions.
        if !state
            .store
            .get_turn_run(body.turn_id)
            .await?
            .is_some_and(|turn| turn.chat_id == id)
        {
            return Err(ServerError::conflict(format!(
                "turn {} is not cancellable",
                body.turn_id
            )));
        }
        tokio::task::yield_now().await;
    };
    if matches!(
        resolution.outcome,
        RequestTurnCancellationOutcome::AlreadyTerminal(_)
    ) {
        return Err(ServerError::conflict(format!(
            "turn {} already finished before cancellation",
            body.turn_id
        )));
    }
    if let Some(event) = resolution.terminal_event {
        let _ = state.events.sender(id).send(event);
    }
    state.active_turns.cancel(id, body.turn_id);
    state.turn_job_wake.notify_one();
    // A parked-parent cancellation can fence a queued or running sandbox
    // child. Wake its worker promptly; durable claims remain the source of
    // truth if this notification is lost.
    state.agent_run_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// Body of `POST /chats/{id}/approvals/{call_id}`.
#[derive(Debug, Deserialize)]
pub struct ApprovalBody {
    /// `approve` or `reject`.
    pub decision: ApprovalChoice,
    /// Optional reject reason (ignored on approve).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Wire form of an approval decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Approve,
    Reject,
}

/// `POST /chats/{id}/approvals/{call_id}` — decide a parked Sensitive tool call.
///
/// `204` on success. `404` if the chat or call isn't pending. The turn stays
/// holding its slot until it finishes after the decision.
pub async fn post_approval(
    State(state): State<AppState>,
    Path((chat_id, call_id)): Path<(ChatId, CallId)>,
    Json(body): Json<ApprovalBody>,
) -> Result<StatusCode, ServerError> {
    // Confirm the chat exists so a typo'd id doesn't look like "not pending".
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let decision = match body.decision {
        ApprovalChoice::Approve => ApprovalDecision::Approve,
        ApprovalChoice::Reject => ApprovalDecision::Reject {
            reason: body
                .reason
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "user denied approval".into()),
        },
    };
    match state.approvals.resolve(chat_id, call_id, decision) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(crate::approvals::DecideError::NotPending) => Err(ServerError::not_found(format!(
            "no pending approval for call {call_id}"
        ))),
        Err(crate::approvals::DecideError::WrongChat) => Err(ServerError::not_found(format!(
            "no pending approval for call {call_id}"
        ))),
    }
}

/// Query for `GET /chats/{id}/events`.
#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    /// Resume after this journal sequence number; `0` (the default) replays from
    /// the start.
    #[serde(default)]
    pub after: i64,
}

/// `GET /chats/{id}/events` (WebSocket) — stream a chat's turn events.
///
/// On connect the client gets **snapshot → replay → live**: journaled events with
/// `seq > after` are replayed, then live events stream as they occur. Subscribing
/// to the live tail *before* replaying, dropping any live event whose `seq` was
/// already replayed, and replaying whenever the live cursor jumps means nothing
/// is missed or duplicated across the handoff or a worker-ownership race. `404`
/// if the chat doesn't exist.
///
/// Auth is checked by the bearer middleware. Browser clients authenticate via
/// `Sec-WebSocket-Protocol` (`openwave-token.<token>`). When the client offered
/// `openwave-v1`, this handler selects it in the upgrade response so the
/// browser accepts the handshake (RFC 6455).
pub async fn chat_events(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    headers: axum::http::HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    let upgrade = if offered_handshake_subprotocol(&headers) {
        upgrade.protocols([WS_HANDSHAKE_SUBPROTOCOL])
    } else {
        upgrade
    };
    Ok(upgrade.on_upgrade(move |socket| stream_events(socket, state, id, query.after)))
}

/// Serve one client's event stream for `chat`: replay from the journal, then live.
async fn stream_events(mut socket: WebSocket, state: AppState, chat: ChatId, after: i64) {
    // Subscribe before replaying, so an event emitted during replay is buffered on
    // the live channel rather than lost in the gap between the two.
    let mut live = state.events.subscribe(chat);

    // Replay everything the client hasn't seen yet from the durable journal.
    let mut last_seq = after;
    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            // Watch the socket so a client disconnect ends the task promptly.
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            },
            live_event = live.recv() => match live_event {
                Ok(event) => {
                    if event.seq <= last_seq {
                        continue; // already covered by replay
                    }
                    if event.seq > last_seq.saturating_add(1) {
                        // Durable commits can be published out of order across
                        // lease owners. Fill the gap from the journal before
                        // accepting this live tail; replay includes the current
                        // event because publication always follows commit.
                        if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    last_seq = event.seq;
                    if send_event(&mut socket, &event).await.is_err() {
                        break;
                    }
                }
                // Fell behind the live buffer. Rather than drop the client, catch
                // up from the journal (durable truth) and resume live — the seq
                // dedup above absorbs any overlap. A long/fast turn can outrun the
                // 256-slot buffer, so this keeps an ordinary client connected.
                Err(RecvError::Lagged(_)) => {
                    if replay_after(&mut socket, &*state.store, chat, &mut last_seq)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
        }
    }
}

/// Send journaled events with `seq > *last_seq` to the socket, advancing
/// `*last_seq`. `Err(())` means the connection should end (send or store failure).
async fn replay_after(
    socket: &mut WebSocket,
    store: &dyn Store,
    chat: ChatId,
    last_seq: &mut i64,
) -> Result<(), ()> {
    let events = store.list_events(chat, *last_seq).await.map_err(|_| ())?;
    for event in events {
        *last_seq = event.seq;
        send_event(socket, &event).await.map_err(|_| ())?;
    }
    Ok(())
}

/// Send one event as a JSON text frame. An event that fails to serialize is
/// skipped rather than sent as an empty frame (which a client couldn't decode).
async fn send_event(socket: &mut WebSocket, event: &SequencedEvent) -> Result<(), axum::Error> {
    let Ok(json) = serde_json::to_string(event) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
