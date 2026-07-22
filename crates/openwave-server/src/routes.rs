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
    ApprovalDecision, CallId, Chat, ChatId, DeleteChatOutcome, DeleteProjectOutcome,
    Message as StoredMessage, MessageId, Project, ProjectId, RequestAgentRunCancellationOutcome,
    RequestTurnCancellationOutcome, Role, SandboxToolCall, SandboxToolCallStatus, SecretProvider,
    SequencedEvent, Store, ToolCallExecution, ToolCallRecord, ToolCallStatus, TurnId, TurnSteer,
    TurnSteerId,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::event_projection::RendererSequencedEvent;
use crate::extract::{Json, Path, Query};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;
use crate::web_search::{
    self, WebSearchConfigInfo, WebSearchConfigUpdate, WebSearchCredentialReadiness,
    WebSearchCredentialsInfo,
};

mod client_execution;
mod delegated_file_execution;
mod document;
mod root_attachment;
pub use client_execution::*;
pub use delegated_file_execution::*;
pub use document::*;
pub use root_attachment::*;

/// The store-settings key for the selected model.
const MODEL_SETTING: &str = "model";

/// Product-facing project names stay compact across desktop and API clients.
pub const MAX_PROJECT_TITLE_CHARS: usize = 120;
/// Project metadata requests need only a compact JSON object.
pub const MAX_PROJECT_METADATA_BODY_BYTES: usize = 1_024;

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

/// Body of `PATCH /projects/{id}`. An explicit `null` clears the title, while
/// an absent field leaves it unchanged.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdate {
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
}

fn normalize_project_title(title: Option<String>) -> Result<Option<String>, ServerError> {
    let title = title
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if title
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_PROJECT_TITLE_CHARS)
    {
        return Err(ServerError::bad_request(format!(
            "project title must not exceed {MAX_PROJECT_TITLE_CHARS} characters"
        )));
    }
    Ok(title)
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    let project = Project {
        id: ProjectId::new(),
        title: normalize_project_title(body.title)?,
        attachment_revision: 0,
        root_attachments: Vec::new(),
        created_at: Utc::now(),
    };
    state.store.create_project(&project).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `PATCH /projects/{id}` — update bounded human-facing project metadata.
pub async fn patch_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
    Json(body): Json<ProjectUpdate>,
) -> Result<Json<Project>, ServerError> {
    let title = body.title.map(normalize_project_title).transpose()?;
    if let Some(title) = title {
        if !state.store.update_project_title(id, title).await? {
            return Err(ServerError::not_found(format!("project {id} not found")));
        }
    }
    state
        .store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("project {id} not found")))
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

/// `DELETE /projects/{id}` — remove an empty project. Owned conversations,
/// documents, and root defaults must be removed through their explicit
/// lifecycle APIs first; this boundary never cascades them.
pub async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<ProjectId>,
) -> Result<StatusCode, ServerError> {
    match state.store.delete_project(id).await? {
        DeleteProjectOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteProjectOutcome::NotFound => {
            Err(ServerError::not_found(format!("project {id} not found")))
        }
        DeleteProjectOutcome::NotEmpty => Err(ServerError::conflict_kind(
            "project_not_empty",
            "remove the project's conversations, documents, and connected folders before deleting it",
        )),
    }
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
            .ok_or_else(|| ServerError::not_found(format!("project {project_id} not found")))?;
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
    /// An explicit `null` clears the title. Non-empty titles are trimmed before
    /// persistence so sidebar labels remain stable across clients.
    #[serde(default, deserialize_with = "double_option")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub model: Option<Option<String>>,
}

/// `PATCH /chats/{id}` — update the human-facing title and/or model selection.
pub async fn patch_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<ChatUpdate>,
) -> Result<Json<Chat>, ServerError> {
    // Validate every supplied field before touching durable state. This keeps a
    // mixed request all-or-nothing from the user's point of view.
    if body
        .model
        .as_ref()
        .is_some_and(|model| model.as_deref().is_some_and(str::is_empty))
    {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    let title = body.title.map(|title| {
        title
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    });

    let mut chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    if !state
        .store
        .update_chat_metadata(id, title.clone(), body.model.clone())
        .await?
    {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
    if let Some(title) = title {
        chat.title = title;
    }
    if let Some(model) = body.model {
        chat.model = model;
    }
    Ok(Json(chat))
}

/// A renderer-safe durable transcript entry. Internal routing and tool state
/// deliberately remain behind the server boundary.
#[derive(Debug, Serialize)]
pub struct ChatMessageSnapshot {
    pub id: MessageId,
    pub role: Role,
    pub content: String,
    pub created_at: chrono::DateTime<Utc>,
    pub citations: Vec<openwave_core::AssistantCitationSnapshot>,
}

/// One visible transcript plus the durable journal watermark that produced it.
/// The renderer uses the watermark to subscribe only to future events, avoiding
/// duplicate text when reopening a completed conversation.
#[derive(Debug, Serialize)]
pub struct ChatTranscript {
    pub messages: Vec<ChatMessageSnapshot>,
    /// Finished tool activity from terminal turns, projected through a fixed
    /// renderer-safe allowlist. Canonical tool records never cross this API.
    pub tool_activity: Vec<openwave_core::ChatToolActivitySnapshot>,
    pub last_event_seq: i64,
}

impl From<StoredMessage> for ChatMessageSnapshot {
    fn from(message: StoredMessage) -> Self {
        Self {
            id: message.id,
            role: message.role,
            content: message.content,
            created_at: message.created_at,
            citations: Vec::new(),
        }
    }
}

/// `GET /chats/{id}/messages` — replay the visible durable transcript in
/// commit order. The existence check prevents a missing chat from looking like
/// an empty conversation.
pub async fn list_chat_messages(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<Json<ChatTranscript>, ServerError> {
    let transcript = state
        .store
        .get_chat_transcript(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;
    let mut citations_by_message = std::collections::HashMap::new();
    for citation in transcript.citations {
        citations_by_message
            .entry(citation.message_id)
            .or_insert_with(Vec::new)
            .push(citation);
    }
    let messages = transcript
        .messages
        .into_iter()
        .filter(|message| matches!(message.role, Role::User | Role::Assistant))
        .map(|message| {
            let mut snapshot = ChatMessageSnapshot::from(message);
            snapshot.citations = citations_by_message
                .remove(&snapshot.id)
                .unwrap_or_default();
            snapshot
        })
        .collect();
    Ok(Json(ChatTranscript {
        messages,
        tool_activity: transcript.tool_activity,
        last_event_seq: transcript.last_event_seq,
    }))
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

/// `DELETE /chats/{id}` — remove a quiesced conversation and its product
/// history. Rooted or active conversations deliberately return a conflict: the
/// caller must first finish cancellation and durable broker detachment.
pub async fn delete_chat(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
) -> Result<StatusCode, ServerError> {
    match state.store.delete_chat(id).await? {
        DeleteChatOutcome::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteChatOutcome::NotFound => Err(ServerError::not_found(format!("chat {id} not found"))),
        DeleteChatOutcome::ActiveWork => Err(ServerError::conflict_kind(
            "chat_active",
            "finish or cancel the active work before deleting this conversation",
        )),
        DeleteChatOutcome::RootsAttached => Err(ServerError::conflict_kind(
            "chat_roots_attached",
            "detach connected folders before deleting this conversation",
        )),
        DeleteChatOutcome::RootAttachmentStateUnresolved => Err(ServerError::conflict_kind(
            "chat_root_attachment_unresolved",
            "reconcile connected-folder changes before deleting this conversation",
        )),
    }
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
    /// The currently checkpointed, renderer-safe activity, if any.
    ///
    /// This is intentionally a small fixed vocabulary. It never exposes tool
    /// arguments, results, provider call identities, executor leases, or raw
    /// executor diagnostics.
    pub activity: Option<AgentActivitySnapshot>,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

impl AgentRunSnapshot {
    fn from_run(run: AgentRun, activity: Option<AgentActivitySnapshot>) -> Self {
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

/// Fixed, renderer-safe names for supported live work.
///
/// Adding a durable tool does not automatically expose it to a renderer: it
/// must be deliberately admitted here with a safe label.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityKind {
    WebSearch,
    ReadDelegatedFile,
    ListConnectedFolders,
    ListFolder,
    ReadConnectedFile,
}

/// Coarse checkpoint lifecycle suitable for display.
///
/// This intentionally does not mirror all durable executor states; only live
/// work is represented, and terminal checkpoints produce no activity.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityStatus {
    Waiting,
    Running,
}

/// Renderer-safe projection of one live supported checkpoint.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AgentActivitySnapshot {
    pub kind: AgentActivityKind,
    pub status: AgentActivityStatus,
}

fn sandbox_activity(calls: &[SandboxToolCall]) -> Option<AgentActivitySnapshot> {
    // A sandbox can have at most one live checkpoint. Inspecting the latest
    // durable live checkpoint also prevents an older, completed activity from
    // lingering in the UI after the run has advanced.
    let call = calls.iter().rev().find(|call| !call.status.is_terminal())?;
    let kind = match call.name.as_str() {
        "web_search" => AgentActivityKind::WebSearch,
        openwave_core::SANDBOX_READ_DELEGATED_FILE_TOOL => AgentActivityKind::ReadDelegatedFile,
        // Unknown tool names are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = match call.status {
        SandboxToolCallStatus::Accepted => AgentActivityStatus::Waiting,
        SandboxToolCallStatus::Claimed => AgentActivityStatus::Running,
        SandboxToolCallStatus::Completed
        | SandboxToolCallStatus::Failed
        | SandboxToolCallStatus::Cancelled => return None,
        _ => return None,
    };
    Some(AgentActivitySnapshot { kind, status })
}

fn foreground_activity(
    calls: &[ToolCallRecord],
    now: chrono::DateTime<Utc>,
) -> Option<AgentActivitySnapshot> {
    // A foreground turn can park on exactly one client tool call. Looking at
    // the latest live supported call means a completed folder operation never
    // lingers after its continuation advances.
    let call = calls.iter().rev().find(|call| {
        call.execution == ToolCallExecution::Client && call.status == ToolCallStatus::Pending
    })?;
    let kind = match call.name.as_str() {
        "list_connected_folders" => AgentActivityKind::ListConnectedFolders,
        "list_folder" => AgentActivityKind::ListFolder,
        "read_connected_file" => AgentActivityKind::ReadConnectedFile,
        // Unknown client tools are executor data, not a renderer API contract.
        _ => return None,
    };
    let status = if call
        .client_lease_expires_at
        .is_some_and(|expires_at| expires_at > now)
    {
        AgentActivityStatus::Running
    } else {
        AgentActivityStatus::Waiting
    };
    Some(AgentActivitySnapshot { kind, status })
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
    // This read model needs only live client checkpoints. Loading the complete
    // tool-call transcript here would needlessly deserialize historical model
    // arguments, results, and local diagnostics just to render current work.
    let client_calls = state.store.list_pending_client_tool_calls(id).await?;
    let now = Utc::now();
    let mut snapshots = Vec::with_capacity(runs.len());
    for run in runs {
        let activity = if run.execution == AgentRunExecution::Sandbox {
            let calls = state
                .store
                .list_sandbox_tool_calls_for_agent_run(run.id)
                .await?;
            sandbox_activity(&calls)
        } else if run.execution == AgentRunExecution::Foreground {
            foreground_activity(&client_calls, now)
        } else {
            None
        };
        snapshots.push(AgentRunSnapshot::from_run(run, activity));
    }
    Ok(Json(snapshots))
}

/// Closed renderer-safe acknowledgement for sandbox cancellation.
#[derive(Debug, Serialize)]
pub struct AgentRunCancellationSnapshot {
    pub id: openwave_core::AgentRunId,
    pub status: AgentRunCancellationStatus,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunCancellationStatus {
    Cancelling,
    Cancelled,
}

/// `POST /chats/{chat_id}/agent-runs/{run_id}/cancel` — durably request
/// cancellation of one sandbox child.
///
/// The durable transition commits before any process-local signal is sent.
/// Exact retries of cancelling or cancelled work remain accepted. Foreground,
/// wrong-chat, successful, and failed runs are rejected without exposing
/// executor details.
pub async fn post_agent_run_cancel(
    State(state): State<AppState>,
    Path((chat_id, run_id)): Path<(ChatId, openwave_core::AgentRunId)>,
) -> Result<(StatusCode, Json<AgentRunCancellationSnapshot>), ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let Some(run) = state.store.get_agent_run(run_id).await? else {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    };
    if run.chat_id != chat_id || run.execution != AgentRunExecution::Sandbox {
        return Err(ServerError::conflict("sandbox run is not cancellable"));
    }

    let mut outcome = None;
    for _ in 0..8 {
        if let Some(resolved) = state.store.request_agent_run_cancellation(run_id).await? {
            outcome = Some(resolved);
            break;
        }
        let Some(current) = state.store.get_agent_run(run_id).await? else {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        };
        if current.chat_id != chat_id || current.execution != AgentRunExecution::Sandbox {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
        tokio::task::yield_now().await;
    }
    let Some(outcome) = outcome else {
        return Err(ServerError::conflict(
            "sandbox run cancellation could not be serialized",
        ));
    };

    let (run, status) = match outcome {
        RequestAgentRunCancellationOutcome::Requested(run) => {
            let status = AgentRunCancellationStatus::Cancelling;
            (run, status)
        }
        RequestAgentRunCancellationOutcome::Cancelled(run) => {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelled =>
        {
            (run, AgentRunCancellationStatus::Cancelled)
        }
        RequestAgentRunCancellationOutcome::Existing(run)
            if run.status == AgentRunStatus::Cancelling =>
        {
            (run, AgentRunCancellationStatus::Cancelling)
        }
        RequestAgentRunCancellationOutcome::AlreadyTerminal(_)
        | RequestAgentRunCancellationOutcome::Existing(_) => {
            return Err(ServerError::conflict("sandbox run is not cancellable"));
        }
    };

    signal_sandbox_run_after_commit(&state, run.id).await;
    state.agent_run_wake.notify_one();
    Ok((
        StatusCode::ACCEPTED,
        Json(AgentRunCancellationSnapshot { id: run.id, status }),
    ))
}

/// Best-effort local acceleration after a sandbox cancellation has committed.
///
/// The immutable receipts provide exact attempt identities. Missing receipts,
/// transient read failures, and absent local workers are harmless because the
/// durable state machine remains authoritative and its workers will eventually
/// observe the cancellation through heartbeats, lease expiry, or terminal
/// write fencing.
async fn signal_sandbox_run_after_commit(state: &AppState, run_id: openwave_core::AgentRunId) {
    if let Ok(Some(signal)) = state.store.get_agent_run_cancellation_signal(run_id).await {
        state
            .sandbox_attempts
            .cancel_model(run_id, signal.lease_token);
    }
    // Cancelling a waiting run atomically terminalizes its live tool call and
    // records the exact executor lease. Never infer that lease from mutable
    // call state or signal every call belonging to a run.
    if let Ok(calls) = state
        .store
        .list_sandbox_tool_calls_for_agent_run(run_id)
        .await
    {
        for call in calls {
            if call.status != SandboxToolCallStatus::Cancelled {
                continue;
            }
            if let Ok(Some(receipt)) = state.store.get_sandbox_tool_call_receipt(call.id).await {
                if receipt.status == SandboxToolCallStatus::Cancelled {
                    state.sandbox_attempts.cancel_search(
                        call.id,
                        run_id,
                        receipt.executor_lease_token,
                    );
                }
            }
        }
    }
}

/// Signal only children durably owned by the cancelled origin turn.
async fn signal_origin_sandbox_runs_after_commit(
    state: &AppState,
    chat_id: ChatId,
    origin_turn_id: TurnId,
) {
    let Ok(runs) = state.store.list_agent_runs(chat_id).await else {
        return;
    };
    for run in runs {
        if run.execution != AgentRunExecution::Sandbox
            || !matches!(
                run.status,
                AgentRunStatus::Cancelling | AgentRunStatus::Cancelled
            )
        {
            continue;
        }
        let Ok(Some(admission)) = state.store.get_sandbox_agent_admission(run.id).await else {
            continue;
        };
        if admission.origin_turn_id == origin_turn_id {
            signal_sandbox_run_after_commit(state, run.id).await;
        }
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    fn client_call(name: &str, lease_expires_at: Option<chrono::DateTime<Utc>>) -> ToolCallRecord {
        ToolCallRecord {
            id: CallId::new(),
            chat_id: ChatId::new(),
            turn_id: TurnId::new(),
            provider_id: "provider-call-identity".into(),
            name: name.into(),
            arguments: serde_json::json!({
                "root_id": "5b3e9987-5ebf-4bb0-bc6f-0c041b156027",
                "path": "taxes/2026/private-return.txt",
                "grant": "private-grant"
            }),
            execution: ToolCallExecution::Client,
            status: ToolCallStatus::Pending,
            result: None,
            error_code: Some("private-error-code".into()),
            error_detail: Some("private error detail".into()),
            client_executor_id: Some(uuid::Uuid::new_v4()),
            client_lease_expires_at: lease_expires_at,
            created_at: Utc::now(),
            resolved_at: None,
        }
    }

    #[test]
    fn foreground_folder_activity_has_a_closed_safe_vocabulary() {
        let now = Utc::now();
        for (name, kind) in [
            ("list_connected_folders", "list_connected_folders"),
            ("list_folder", "list_folder"),
            ("read_connected_file", "read_connected_file"),
        ] {
            let activity = foreground_activity(
                &[client_call(name, Some(now + chrono::Duration::minutes(1)))],
                now,
            )
            .expect("supported foreground folder work is visible");
            assert_eq!(
                serde_json::to_value(activity).unwrap(),
                serde_json::json!({"kind": kind, "status": "running"})
            );
        }

        let waiting = foreground_activity(&[client_call("list_folder", None)], now)
            .expect("an unclaimed folder operation is visible");
        assert_eq!(
            serde_json::to_value(waiting).unwrap(),
            serde_json::json!({"kind": "list_folder", "status": "waiting"})
        );

        assert!(foreground_activity(&[client_call("unknown_client_tool", None)], now).is_none());

        let rendered = serde_json::to_string(
            &foreground_activity(&[client_call("read_connected_file", None)], now).unwrap(),
        )
        .unwrap();
        for forbidden in [
            "5b3e9987-5ebf-4bb0-bc6f-0c041b156027",
            "taxes/2026/private-return.txt",
            "private-grant",
            "provider-call-identity",
            "private-error-code",
            "private error detail",
        ] {
            assert!(!rendered.contains(forbidden), "activity leaked {forbidden}");
        }
    }
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
    // The turn transaction has already committed its child cancellation
    // cascade. Exact local handles now reduce provider shutdown latency without
    // taking part in the durable decision.
    signal_origin_sandbox_runs_after_commit(&state, id, body.turn_id).await;
    state.turn_job_wake.notify_one();
    // A parked-parent cancellation can fence a queued or running sandbox
    // child. Wake its worker promptly; durable claims remain the source of
    // truth if this notification is lost.
    state.agent_run_wake.notify_one();
    Ok(StatusCode::ACCEPTED)
}

/// Body of `POST /chats/{id}/approvals/{call_id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalBody {
    /// `approve` or `reject`.
    pub decision: ApprovalChoice,
    /// Optional reject reason (invalid on approve).
    #[serde(default)]
    pub reason: Option<String>,
    /// Remember an approval for matching calls in this chat.
    #[serde(default)]
    pub remember: bool,
}

/// Wire form of an approval decision.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalChoice {
    Approve,
    Reject,
}

/// Bounded query for restart/reconnect approval recovery.
#[derive(Debug, Deserialize)]
pub(crate) struct PendingApprovalsQuery {
    #[serde(default = "default_pending_approvals_limit")]
    pub limit: u64,
}

fn default_pending_approvals_limit() -> u64 {
    50
}

/// Closed renderer-safe pending approval projection. Canonical arguments,
/// model-authored summaries, and unknown tool names never cross this boundary.
#[derive(Debug, Serialize)]
pub(crate) struct PendingApprovalSnapshot {
    pub call_id: CallId,
    pub turn_id: TurnId,
    pub action: crate::event_projection::RendererToolName,
    pub approval: openwave_core::ToolApprovalKind,
    pub class: openwave_core::ApprovalClass,
    pub can_approve: bool,
}

/// `GET /chats/{id}/approvals` — recover a bounded page of pending cards.
pub(crate) async fn list_pending_approvals(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
    Query(query): Query<PendingApprovalsQuery>,
) -> Result<Json<Vec<PendingApprovalSnapshot>>, ServerError> {
    if !(1..=100).contains(&query.limit) {
        return Err(ServerError::bad_request(
            "approval limit must be between 1 and 100",
        ));
    }
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    let approvals = state
        .store
        .list_pending_tool_call_approvals(chat_id, query.limit)
        .await?;
    Ok(Json(
        approvals
            .into_iter()
            .map(|approval| {
                let action =
                    crate::event_projection::RendererToolName::from(approval.tool_name.as_str());
                let kind = approval.kind;
                PendingApprovalSnapshot {
                    call_id: approval.call_id,
                    turn_id: approval.turn_id,
                    action,
                    approval: kind,
                    class: approval.class,
                    can_approve: kind.is_approvable(),
                }
            })
            .collect(),
    ))
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
        ApprovalChoice::Approve => {
            if body.reason.is_some() {
                return Err(ServerError::bad_request(
                    "approval reason is only valid when rejecting",
                ));
            }
            ApprovalDecision::Approve
        }
        ApprovalChoice::Reject => ApprovalDecision::Reject {
            reason: body
                .reason
                .map(|reason| reason.trim().to_owned())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "user denied approval".into()),
        },
    };
    if body.remember && !matches!(&decision, ApprovalDecision::Approve) {
        return Err(ServerError::bad_request(
            "only an approval can be remembered",
        ));
    }
    if decision
        .reason()
        .is_some_and(|reason| !openwave_core::ToolApproval::valid_reason(reason))
    {
        return Err(ServerError::bad_request(
            "approval reject reason is invalid",
        ));
    }
    match state
        .approvals
        .resolve_with_remember(chat_id, call_id, decision, body.remember)
        .await?
    {
        crate::approvals::ResolveApprovalOutcome::Resolved => Ok(StatusCode::NO_CONTENT),
        crate::approvals::ResolveApprovalOutcome::NotPending => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::WrongChat => Err(ServerError::not_found(
            format!("no pending approval for call {call_id}"),
        )),
        crate::approvals::ResolveApprovalOutcome::NotApprovable => Err(ServerError::conflict_kind(
            "approval_action_not_presentable",
            "this action cannot be approved from the renderer",
        )),
        crate::approvals::ResolveApprovalOutcome::DecisionConflict => {
            Err(ServerError::conflict_kind(
                "approval_already_decided",
                "this approval was already decided differently",
            ))
        }
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
    let Ok(json) = serde_json::to_string(&RendererSequencedEvent::from(event)) else {
        return Ok(());
    };
    socket.send(Message::Text(json.into())).await
}
