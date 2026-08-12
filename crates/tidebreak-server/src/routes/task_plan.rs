//! Renderer-safe recovery of a conversation's task plan.

use tidebreak_core::{ChatId, TaskPlan};

use crate::error::ServerError;
use crate::extract::{Json, Path};
use crate::scoped_store::ScopedStore;

/// `GET /chats/{id}/task-plan` — the chat's current plan, or `null`.
///
/// The journal only carries a refresh hint, so this is where the steps come
/// from, both live and after a reload. A chat that never made a plan is not an
/// error; it answers `null`.
pub async fn get_task_plan(
    store: ScopedStore,
    Path(chat_id): Path<ChatId>,
) -> Result<Json<Option<TaskPlan>>, ServerError> {
    store.require_chat(chat_id).await?;
    Ok(Json(store.get_task_plan(chat_id).await?))
}
