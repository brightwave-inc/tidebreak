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
    Agent, Chat, ChatId, Project, ProjectId, SecretProvider, SequencedEvent, Store,
};

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::resolver::ANTHROPIC_API_KEY;
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

/// Whether a model API key is configured — in the secret store or the
/// environment fallback the resolver also honors.
async fn has_api_key(secrets: &dyn SecretProvider) -> bool {
    if matches!(secrets.get_secret(ANTHROPIC_API_KEY).await, Ok(Some(key)) if !key.is_empty()) {
        return true;
    }
    std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.is_empty())
}

/// Body of `PUT /settings/api-key`.
#[derive(Debug, Deserialize)]
pub struct ApiKey {
    /// The provider API key to store. Written to the `SecretProvider` (the OS
    /// keychain on desktop), never to the database, and never read back out.
    pub api_key: String,
}

/// `PUT /settings/api-key` — store the model provider's API key. `204 No Content`.
pub async fn put_api_key(
    State(state): State<AppState>,
    Json(body): Json<ApiKey>,
) -> Result<StatusCode, ServerError> {
    if body.api_key.is_empty() {
        return Err(ServerError::bad_request("api_key must not be empty"));
    }
    state
        .secrets
        .set_secret(ANTHROPIC_API_KEY, &body.api_key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /settings/api-key` — remove the stored API key. `204 No Content`.
pub async fn delete_api_key(State(state): State<AppState>) -> Result<StatusCode, ServerError> {
    state.secrets.delete_secret(ANTHROPIC_API_KEY).await?;
    Ok(StatusCode::NO_CONTENT)
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
    let chat = Chat {
        id: ChatId::new(),
        project_id: body.project_id,
        title: body.title,
        workspace_dir: body.workspace_dir,
        created_at: Utc::now(),
    };
    state.store.create_chat(&chat).await?;
    Ok((StatusCode::CREATED, Json(chat)))
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

    // The model is reconfigurable at runtime via PUT /settings; a stored value
    // overrides the boot default for this turn.
    let mut agent_config = state.agent_config.clone();
    if let Some(model) = read_model(&*state.store).await? {
        agent_config.model = model;
    }
    // Resolve the provider from the currently-configured key, so a key set via
    // PUT /settings/api-key takes effect on this turn.
    let provider = state.resolver.resolve().await;
    let agent = Agent::new(
        provider,
        state.tools.clone(),
        state.store.clone(),
        agent_config,
    );
    let store = state.store.clone();
    let events = state.events.clone();
    tokio::spawn(async move {
        // Hold the slot for the turn's lifetime; dropping it frees the chat.
        let _active = active;
        crate::hub::drive_and_journal(agent, chat, body.content, store, events).await;
    });

    Ok(StatusCode::ACCEPTED)
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
pub async fn chat_events(
    State(state): State<AppState>,
    Path(id): Path<ChatId>,
    Query(query): Query<EventsQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    if state.store.get_chat(id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {id} not found")));
    }
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
