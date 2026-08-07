//! Renderer-safe recovery of a conversation's task plan.

use axum::extract::State;
use openwave_core::{ChatId, TaskPlan};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// `GET /chats/{id}/task-plan` — the chat's current plan, or `null`.
///
/// The journal only carries a refresh hint, so this is where the steps come
/// from, both live and after a reload. A chat that never made a plan is not an
/// error; it answers `null`.
pub async fn get_task_plan(
    State(state): State<AppState>,
    Path(chat_id): Path<ChatId>,
) -> Result<Json<Option<TaskPlan>>, ServerError> {
    if state.store.get_chat(chat_id).await?.is_none() {
        return Err(ServerError::not_found(format!("chat {chat_id} not found")));
    }
    Ok(Json(state.store.get_task_plan(chat_id).await?))
}
