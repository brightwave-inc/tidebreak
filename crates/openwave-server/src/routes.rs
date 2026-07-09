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

use openwave_core::{Agent, Chat, ChatId, Project, ProjectId, SequencedEvent, Store};

use crate::error::ServerError;
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

/// The store-settings key for the selected model.
const MODEL_SETTING: &str = "model";

/// Runtime settings a client can read and change. Secrets (e.g. API keys) are
/// never included here — they live in the `SecretProvider`, not the store.
#[derive(Debug, Serialize, Deserialize)]
pub struct Settings {
    /// The model turns run against, or `None` to use the server's default.
    #[serde(default)]
    pub model: Option<String>,
}

/// `GET /settings` — the current runtime settings.
pub async fn get_settings(State(state): State<AppState>) -> Result<Json<Settings>, ServerError> {
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
    }))
}

/// `PUT /settings` — update runtime settings, returning the new state. Only the
/// fields present in the body are changed.
pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<Settings>,
) -> Result<Json<Settings>, ServerError> {
    if let Some(model) = &body.model {
        state
            .store
            .set_setting(MODEL_SETTING, &serde_json::json!(model))
            .await?;
    }
    Ok(Json(Settings {
        model: read_model(&*state.store).await?,
    }))
}

/// The configured model, if any.
async fn read_model(store: &dyn Store) -> Result<Option<String>, ServerError> {
    Ok(store
        .get_setting(MODEL_SETTING)
        .await?
        .and_then(|value| value.as_str().map(str::to_owned)))
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
    let agent = Agent::new(
        state.provider.clone(),
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
