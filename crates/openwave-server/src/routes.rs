//! Chat HTTP handlers.
//!
//! The conversation CRUD surface (create / list / get), posting a message to
//! start a turn, and the WebSocket stream of a chat's turn events.

use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;

use openwave_core::{
    Agent, ApprovalDecision, CallId, Chat, ChatId, Project, ProjectId, SecretProvider,
    SequencedEvent, Store,
};

use crate::auth::{offered_handshake_subprotocol, WS_HANDSHAKE_SUBPROTOCOL};
use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::providers::{self, ProviderCredential, ProviderInfo, ProviderKind, ProviderUpdate};
use crate::state::AppState;

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
pub struct CreateProject {
    /// Absolute path to the project's workspace/corpus root.
    pub workspace_dir: PathBuf,
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /projects` — create a project and return it (`201 Created`).
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<impl IntoResponse, ServerError> {
    if !body.workspace_dir.is_absolute() {
        return Err(ServerError::bad_request(format!(
            "workspace_dir must be an absolute path, got {:?}",
            body.workspace_dir
        )));
    }
    let project = Project {
        id: ProjectId::new(),
        title: body.title,
        workspace_dir: body.workspace_dir,
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
pub struct CreateChat {
    /// Absolute path to the workspace directory the agent operates in.
    pub workspace_dir: PathBuf,
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
    // The workspace path must be absolute: a relative one is resolved against
    // the server process's CWD only later (when a tool canonicalizes it), so the
    // same chat would map to different directories across restarts or launch
    // dirs. Reject it here rather than persist an ambiguous path.
    if !body.workspace_dir.is_absolute() {
        return Err(ServerError::bad_request(format!(
            "workspace_dir must be an absolute path, got {:?}",
            body.workspace_dir
        )));
    }
    // Membership is validated here (the store has no DB-level foreign key): a
    // chat can't reference a project that doesn't exist.
    if let Some(project_id) = body.project_id {
        if state.store.get_project(project_id).await?.is_none() {
            return Err(ServerError::bad_request(format!(
                "project {project_id} not found"
            )));
        }
    }
    if body.model.as_deref().is_some_and(str::is_empty) {
        return Err(ServerError::bad_request("model must not be empty"));
    }
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: body.title,
        model: body.model,
        workspace_dir: body.workspace_dir,
        created_at: Utc::now(),
    };
    state.store.create_chat(&chat).await?;
    Ok((StatusCode::CREATED, Json(chat)))
}

/// Body of `PATCH /chats/{id}`. A double option (like `PUT /settings`): absent
/// leaves the model unchanged, `null` clears it (fall back to the default), and a
/// value sets it.
#[derive(Debug, Deserialize)]
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

/// Body of `POST /chats/{id}/messages`.
#[derive(Debug, Deserialize)]
pub struct PostMessage {
    /// The user's input for this turn.
    pub content: String,
}

/// `POST /chats/{id}/messages` — submit a message and start a turn.
///
/// Returns `202 Accepted` immediately; the turn runs in the background and its
/// events are journaled as they emit (a client watches them over the event
/// stream). `404` if the chat doesn't exist, `409` if a turn is already running
/// for it (one turn per chat at a time).
pub async fn post_message(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Json(body): Json<PostMessage>,
) -> Result<StatusCode, ServerError> {
    let chat = state
        .store
        .get_chat(id)
        .await?
        .ok_or_else(|| ServerError::not_found(format!("chat {id} not found")))?;

    // Claim the chat's single turn slot up front; a concurrent turn is refused.
    let active = state.active_turns.try_acquire(id).ok_or_else(|| {
        ServerError::conflict(format!("chat {id} already has a turn in progress"))
    })?;

    // Model resolution order: the chat's own selection wins, then the global
    // default setting (PUT /settings), then the boot default in agent_config.
    // Short-circuit: only read the setting when the chat has no model of its own,
    // so a chat with a model doesn't pay (or fail on) the settings lookup.
    let mut agent_config = state.agent_config.clone();
    let model = match chat.model.clone() {
        Some(model) => Some(model),
        None => read_model(&*state.store).await?,
    };
    if let Some(model) = model {
        agent_config.model = model;
    }
    // Resolve the provider from currently-configured providers, so a key set via
    // PUT /providers/{kind} (or the legacy /settings/api-key) takes effect on
    // this turn. The composite router selects the adapter from the model name.
    let provider = state.resolver.resolve().await;
    let agent = Agent::new(
        provider,
        state.tools.clone(),
        state.store.clone(),
        agent_config,
    )
    .with_approvals(state.approvals.clone());
    let store = state.store.clone();
    let events = state.events.clone();
    tokio::spawn(async move {
        // Hold the slot for the turn's lifetime; dropping it frees the chat.
        let _active = active;
        crate::hub::drive_and_journal(agent, chat, body.content, store, events).await;
    });

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
/// to the live tail *before* replaying, and dropping any live event whose `seq`
/// was already replayed, means nothing is missed or duplicated across the handoff.
/// `404` if the chat doesn't exist.
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
