use std::path::PathBuf;

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, MessageId, ProjectId, TurnId};
use crate::model::{Chat, Message, Role, ToolCallRecord, TurnRunStatus};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{acquire_chat_write_lock, acquire_turn_write_lock};

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
    // Serialize sequence allocation on the durable chat row. This is also the
    // lock used by turn acceptance, so independently running servers cannot
    // race a journal append with the next turn's admission.
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if entities::turn_run::Entity::find()
        .filter(entities::turn_run::Column::ChatId.eq(chat_id.0))
        .one(&transaction)
        .await
        .map_err(store_err)?
        .is_some()
    {
        return Err(AgentError::Store(format!(
            "chat {chat_id} uses durable turns; append through an exact claim"
        )));
    }

    let seq = append_event_on(&transaction, chat_id, None, None, None, event).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(seq)
}

pub(in crate::db) async fn append_turn_event(
    store: &DbStore,
    chat_id: ChatId,
    turn_id: TurnId,
    lease_token: uuid::Uuid,
    attempt_event_ordinal: i32,
    now: chrono::DateTime<Utc>,
    event: &AgentEvent,
) -> Result<Option<i64>> {
    if turn_id.0.is_nil() {
        return Err(AgentError::Store("event turn id must not be nil".into()));
    }
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "event lease token must not be nil".into(),
        ));
    }
    if !(1..i32::MAX).contains(&attempt_event_ordinal) {
        return Err(AgentError::Store(
            "attempt event ordinal must be positive and below the terminal slot".into(),
        ));
    }
    if matches!(
        event,
        AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnFailed { .. }
            | AgentEvent::TurnCancelled { .. }
    ) {
        return Err(AgentError::Store(
            "terminal turn events must be committed by turn resolution".into(),
        ));
    }
    if let AgentEvent::TurnStarted {
        turn_id: payload_turn_id,
    } = event
    {
        if *payload_turn_id != turn_id {
            return Err(AgentError::Store(format!(
                "turn-started event names {payload_turn_id}, not authoritative turn {turn_id}"
            )));
        }
    }
    let now = canonical_db_timestamp(now)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, chat_id).await? {
        return Err(AgentError::Store(format!("chat {chat_id} does not exist")));
    }
    if !acquire_turn_write_lock(&transaction, turn_id).await? {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    if let Some(existing) = entities::event::Entity::find()
        .filter(entities::event::Column::LeaseToken.eq(lease_token))
        .filter(entities::event::Column::AttemptEventOrdinal.eq(attempt_event_ordinal))
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let payload = serde_json::from_value::<AgentEvent>(existing.payload.clone())?;
        if existing.chat_id != chat_id.0
            || existing.turn_id != Some(turn_id.0)
            || existing.lease_token != Some(lease_token)
            || existing.attempt_event_ordinal != Some(attempt_event_ordinal)
            || existing.terminal
            || payload != *event
        {
            return Err(AgentError::Store(format!(
                "turn event identity ({lease_token}, {attempt_event_ordinal}) was reused with different data"
            )));
        }
        let seq = existing.seq;
        transaction.commit().await.map_err(store_err)?;
        return Ok(Some(seq));
    }

    let Some(claim) = entities::turn_claim::Entity::find_by_id(lease_token)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .filter(|claim| claim.turn_id == turn_id.0)
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    let Some(turn) = entities::turn_run::Entity::find_by_id(turn_id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    else {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    };
    if turn.chat_id != chat_id.0 {
        return Err(AgentError::Store(format!(
            "turn {turn_id} does not belong to chat {chat_id}"
        )));
    }
    if turn.status != TurnRunStatus::Running.as_str()
        || turn.attempt_count != claim.attempt_count
        || turn.lease_token != Some(lease_token)
        || turn
            .lease_expires_at
            .is_none_or(|lease_expires_at| lease_expires_at <= now)
        || turn.updated_at > now
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(None);
    }

    let seq = append_event_on(
        &transaction,
        chat_id,
        Some(turn_id),
        Some(lease_token),
        Some(attempt_event_ordinal),
        event,
    )
    .await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(Some(seq))
}

pub(in crate::db::ops) async fn append_event_on<C>(
    conn: &C,
    chat_id: ChatId,
    turn_id: Option<TurnId>,
    lease_token: Option<uuid::Uuid>,
    attempt_event_ordinal: Option<i32>,
    event: &AgentEvent,
) -> Result<i64>
where
    C: ConnectionTrait,
{
    let last = entities::event::Entity::find()
        .filter(entities::event::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::event::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?;
    let seq = last
        .map_or(Some(1), |model| model.seq.checked_add(1))
        .ok_or_else(|| AgentError::Store(format!("event sequence exhausted for chat {chat_id}")))?;
    entities::event::ActiveModel {
        chat_id: Set(chat_id.0),
        seq: Set(seq),
        turn_id: Set(turn_id.map(|id| id.0)),
        lease_token: Set(lease_token),
        attempt_event_ordinal: Set(attempt_event_ordinal),
        terminal: Set(turn_id.is_some() && is_terminal_event(event)),
        payload: Set(serde_json::to_value(event)?),
        created_at: Set(Utc::now()),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(seq)
}

fn is_terminal_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TurnCompleted { .. }
            | AgentEvent::TurnFailed { .. }
            | AgentEvent::TurnCancelled { .. }
    )
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
