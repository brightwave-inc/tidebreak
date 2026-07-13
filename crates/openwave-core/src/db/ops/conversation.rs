use std::path::PathBuf;

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, MessageId, ProjectId, TurnId};
use crate::model::{Chat, Message, Role, ToolCallRecord, TurnRun, TurnRunStatus};

use super::super::{entities, store_err, DbStore};

pub(in crate::db) async fn create_chat(store: &DbStore, chat: &Chat) -> Result<()> {
    entities::chat::ActiveModel {
        id: Set(chat.id.0),
        project_id: Set(chat.project_id.map(|p| p.0)),
        title: Set(chat.title.clone()),
        model: Set(chat.model.clone()),
        workspace_dir: Set(chat.workspace_dir.to_string_lossy().into_owned()),
        created_at: Set(chat.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn set_chat_model(
    store: &DbStore,
    id: ChatId,
    model: Option<String>,
) -> Result<()> {
    entities::chat::Entity::update_many()
        .col_expr(
            entities::chat::Column::Model,
            sea_orm::sea_query::Expr::value(model),
        )
        .filter(entities::chat::Column::Id.eq(id.0))
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn get_chat(store: &DbStore, id: ChatId) -> Result<Option<Chat>> {
    Ok(entities::chat::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(chat_from_model))
}

pub(in crate::db) async fn list_chats(store: &DbStore) -> Result<Vec<Chat>> {
    Ok(entities::chat::Entity::find()
        .order_by_desc(entities::chat::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(chat_from_model)
        .collect())
}

pub(in crate::db) async fn get_turn_run(store: &DbStore, id: TurnId) -> Result<Option<TurnRun>> {
    entities::turn_run::Entity::find_by_id(id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
        .map(turn_run_from_model)
        .transpose()
}

pub(in crate::db) async fn list_turn_runs(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<TurnRun>> {
    entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::turn_run::Column::CreatedAt)
        .order_by_asc(entities::turn_run::Column::Id)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(turn_run_from_model)
        .collect()
}

pub(in crate::db) async fn append_message(store: &DbStore, message: &Message) -> Result<()> {
    entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        role: Set(role_to_db(message.role).to_string()),
        content: Set(message.content.clone()),
        created_at: Set(message.created_at),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_messages(store: &DbStore, chat_id: ChatId) -> Result<Vec<Message>> {
    entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::message::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(message_from_model)
        .collect()
}

pub(in crate::db) async fn upsert_tool_call(store: &DbStore, call: &ToolCallRecord) -> Result<()> {
    let model = entities::tool_call::ActiveModel {
        id: Set(call.id.0),
        chat_id: Set(call.chat_id.0),
        turn_id: Set(call.turn_id.0),
        provider_id: Set(call.provider_id.clone()),
        name: Set(call.name.clone()),
        arguments: Set(call.arguments.clone()),
        result: Set(call.result.clone()),
        is_error: Set(call.is_error),
        created_at: Set(call.created_at),
        completed_at: Set(call.completed_at),
    };
    entities::tool_call::Entity::insert(model)
        .on_conflict(
            OnConflict::column(entities::tool_call::Column::Id)
                .update_columns([
                    entities::tool_call::Column::Arguments,
                    entities::tool_call::Column::Result,
                    entities::tool_call::Column::IsError,
                    entities::tool_call::Column::CompletedAt,
                ])
                .to_owned(),
        )
        .exec(&store.conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    Ok(entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::tool_call::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(tool_call_from_model)
        .collect())
}

pub(in crate::db) async fn append_event(
    store: &DbStore,
    chat_id: ChatId,
    event: &AgentEvent,
) -> Result<i64> {
    // Next seq for this chat. This assumes a single writer per chat —
    // the server enforces it by allowing only one active turn per chat at
    // a time (a concurrent message is refused, not queued behind a second
    // writer). Under that invariant read-then-insert is race-free; the
    // composite (chat_id, seq) primary key is the backstop that turns any
    // concurrent double-write into an error, never a silent dup or lost seq.
    let last = entities::event::Entity::find()
        .filter(entities::event::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .one(&store.conn)
        .await
        .map_err(store_err)?;
    let seq = last.map_or(0, |model| model.seq) + 1;

    entities::event::ActiveModel {
        chat_id: Set(chat_id.0),
        seq: Set(seq),
        payload: Set(serde_json::to_value(event)?),
        created_at: Set(Utc::now()),
    }
    .insert(&store.conn)
    .await
    .map_err(store_err)?;
    Ok(seq)
}

pub(in crate::db) async fn list_events(
    store: &DbStore,
    chat_id: ChatId,
    after: i64,
) -> Result<Vec<SequencedEvent>> {
    entities::event::Entity::find()
        .filter(entities::event::Column::ChatId.eq(chat_id.0))
        .filter(entities::event::Column::Seq.gt(after))
        .order_by_asc(entities::event::Column::Seq)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|model| {
            Ok(SequencedEvent {
                seq: model.seq,
                event: serde_json::from_value(model.payload)?,
            })
        })
        .collect()
}

fn chat_from_model(model: entities::chat::Model) -> Chat {
    Chat {
        id: ChatId(model.id),
        project_id: model.project_id.map(ProjectId),
        title: model.title,
        model: model.model,
        workspace_dir: PathBuf::from(model.workspace_dir),
        created_at: model.created_at,
    }
}

fn message_from_model(model: entities::message::Model) -> Result<Message> {
    Ok(Message {
        id: MessageId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        role: role_from_db(&model.role)?,
        content: model.content,
        created_at: model.created_at,
    })
}

fn turn_run_from_model(model: entities::turn_run::Model) -> Result<TurnRun> {
    Ok(TurnRun {
        id: TurnId(model.id),
        chat_id: ChatId(model.chat_id),
        model: model.model,
        status: turn_run_status_from_db(&model.status)?,
        attempt_count: model.attempt_count,
        max_attempts: model.max_attempts,
        available_at: model.available_at,
        lease_token: model.lease_token,
        lease_expires_at: model.lease_expires_at,
        started_at: model.started_at,
        finished_at: model.finished_at,
        last_error_code: model.last_error_code,
        last_error_detail: model.last_error_detail,
        created_at: model.created_at,
        updated_at: model.updated_at,
    })
}

fn turn_run_status_from_db(text: &str) -> Result<TurnRunStatus> {
    match text {
        "queued" => Ok(TurnRunStatus::Queued),
        "running" => Ok(TurnRunStatus::Running),
        "retry_wait" => Ok(TurnRunStatus::RetryWait),
        "completed" => Ok(TurnRunStatus::Completed),
        "failed" => Ok(TurnRunStatus::Failed),
        "cancelled" => Ok(TurnRunStatus::Cancelled),
        other => Err(AgentError::Store(format!(
            "unknown durable turn status: {other}"
        ))),
    }
}

fn tool_call_from_model(model: entities::tool_call::Model) -> ToolCallRecord {
    ToolCallRecord {
        id: CallId(model.id),
        chat_id: ChatId(model.chat_id),
        turn_id: TurnId(model.turn_id),
        provider_id: model.provider_id,
        name: model.name,
        arguments: model.arguments,
        result: model.result,
        is_error: model.is_error,
        created_at: model.created_at,
        completed_at: model.completed_at,
    }
}

/// `Role` is persisted as its snake_case name (matching its serde encoding).
fn role_to_db(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_db(text: &str) -> Result<Role> {
    match text {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(AgentError::Store(format!("unknown role: {other}"))),
    }
}
