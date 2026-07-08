//! Session HTTP handlers.
//!
//! The conversation CRUD surface: create a session (which owns a workspace
//! directory), list them, fetch one. Driving a session — posting a message and
//! streaming the turn's events over WebSocket — lands in the next slice.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use serde::Deserialize;

use openwave_core::{Session, SessionId};

use crate::error::ServerError;
use crate::state::AppState;

/// Body of `POST /sessions`.
#[derive(Debug, Deserialize)]
pub struct CreateSession {
    /// Absolute path to the workspace directory the agent operates in.
    pub workspace_dir: PathBuf,
    /// Optional human-facing title.
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /sessions` — create a session and return it (`201 Created`).
pub async fn create_session(
    State(state): State<AppState>,
    Json(body): Json<CreateSession>,
) -> Result<impl IntoResponse, ServerError> {
    let session = Session {
        id: SessionId::new(),
        title: body.title,
        workspace_dir: body.workspace_dir,
        created_at: Utc::now(),
    };
    state.store.create_session(&session).await?;
    Ok((StatusCode::CREATED, Json(session)))
}

/// `GET /sessions` — list sessions, most-recently-created first.
pub async fn list_sessions(
    State(state): State<AppState>,
) -> Result<Json<Vec<Session>>, ServerError> {
    Ok(Json(state.store.list_sessions().await?))
}

/// `GET /sessions/{id}` — fetch one session, or `404`.
pub async fn get_session(
    State(state): State<AppState>,
    Path(id): Path<SessionId>,
) -> Result<Json<Session>, ServerError> {
    state
        .store
        .get_session(id)
        .await?
        .map(Json)
        .ok_or_else(|| ServerError::not_found(format!("session {id} not found")))
}
