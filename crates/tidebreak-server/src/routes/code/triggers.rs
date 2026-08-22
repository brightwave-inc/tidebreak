//! Arming and managing triggers.
//!
//! Triggers bind per repository and apply to workspaces that have a pull
//! request, so every route here is repository-scoped rather than
//! workspace-scoped (decision record 60).

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use tidebreak_core::{CodeTriggerId, RepoId};

use super::types::{CodeTriggerSnapshot, CreateCodeTriggerBody, UpdateCodeTriggerBody};
use crate::code::ScopedCode;
use crate::error::ServerError;

pub async fn list_repo_triggers(
    code: ScopedCode,
    Path(repo_id): Path<RepoId>,
) -> Result<Json<Vec<CodeTriggerSnapshot>>, ServerError> {
    let triggers = code.list_triggers(repo_id).await?;
    Ok(Json(
        triggers
            .into_iter()
            .map(CodeTriggerSnapshot::from)
            .collect(),
    ))
}

pub async fn create_repo_trigger(
    code: ScopedCode,
    Path(repo_id): Path<RepoId>,
    Json(body): Json<CreateCodeTriggerBody>,
) -> Result<impl IntoResponse, ServerError> {
    let trigger = code
        .create_trigger(repo_id, body.condition, body.action)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(CodeTriggerSnapshot::from(trigger)),
    ))
}

pub async fn update_repo_trigger(
    code: ScopedCode,
    Path((repo_id, id)): Path<(RepoId, CodeTriggerId)>,
    Json(body): Json<UpdateCodeTriggerBody>,
) -> Result<Json<CodeTriggerSnapshot>, ServerError> {
    let trigger = code.set_trigger_enabled(repo_id, id, body.enabled).await?;
    Ok(Json(CodeTriggerSnapshot::from(trigger)))
}

pub async fn delete_repo_trigger(
    code: ScopedCode,
    Path((repo_id, id)): Path<(RepoId, CodeTriggerId)>,
) -> Result<StatusCode, ServerError> {
    code.delete_trigger(repo_id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
