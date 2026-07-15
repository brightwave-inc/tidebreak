use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait, TryInsertResult,
};

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{ChatId, HostRootId, MessageId, ProjectId, TurnId};
use crate::model::{
    validate_chat_root_projection, validate_chat_root_projection_against_project, Chat,
    ChatRootAttachment, Message, Role, RootAttachmentOrigin, ToolCallRecord, TurnRunStatus,
    MAX_ROOT_ATTACHMENTS,
};

use super::super::{entities, project_from_models, store_err, DbStore};
use super::turn::canonical_db_timestamp;
use super::{
    acquire_chat_write_lock, acquire_turn_write_lock, agent_run::insert_foreground_agent_run_on,
};

pub(in crate::db) const MESSAGE_IDENTITY_OWNER_MESSAGE: &str = "message";
pub(in crate::db) const MESSAGE_IDENTITY_OWNER_STEER: &str = "turn_steer";

pub(in crate::db) async fn reserve_message_identity_on<C>(
    conn: &C,
    id: MessageId,
    chat_id: ChatId,
    turn_id: TurnId,
    owner: &str,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let inserted =
        entities::message_identity::Entity::insert(entities::message_identity::ActiveModel {
            id: Set(id.0),
            chat_id: Set(chat_id.0),
            turn_id: Set(turn_id.0),
            owner: Set(owner.to_owned()),
        })
        .on_conflict(
            OnConflict::column(entities::message_identity::Column::Id)
                .do_nothing()
                .to_owned(),
        )
        .do_nothing()
        .exec_without_returning(conn)
        .await
        .map_err(store_err)?;
    Ok(matches!(inserted, TryInsertResult::Inserted(1)))
}

pub(in crate::db) async fn transfer_steer_message_identity_on<C>(
    conn: &C,
    id: MessageId,
    chat_id: ChatId,
    turn_id: TurnId,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let transferred = entities::message_identity::Entity::update_many()
        .col_expr(
            entities::message_identity::Column::Owner,
            sea_orm::sea_query::Expr::value(MESSAGE_IDENTITY_OWNER_MESSAGE),
        )
        .filter(entities::message_identity::Column::Id.eq(id.0))
        .filter(entities::message_identity::Column::ChatId.eq(chat_id.0))
        .filter(entities::message_identity::Column::TurnId.eq(turn_id.0))
        .filter(entities::message_identity::Column::Owner.eq(MESSAGE_IDENTITY_OWNER_STEER))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(transferred.rows_affected == 1)
}

pub(in crate::db) async fn next_message_seq_on<C>(conn: &C, chat_id: ChatId) -> Result<i64>
where
    C: ConnectionTrait,
{
    entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .order_by_desc(entities::message::Column::Seq)
        .one(conn)
        .await
        .map_err(store_err)?
        .map_or(Ok(1), |message| {
            message.seq.checked_add(1).ok_or_else(|| {
                AgentError::Store(format!("chat {chat_id} message sequence overflow"))
            })
        })
}

pub(in crate::db) async fn create_chat(store: &DbStore, chat: &Chat) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let project_roots = load_chat_project_roots(&transaction, chat.project_id).await?;
    validate_chat_attachments(chat, &project_roots)?;
    insert_chat_on(&transaction, chat).await?;
    insert_foreground_agent_run_on(&transaction, chat.id, chat.created_at).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn create_chat_with_project_defaults(
    store: &DbStore,
    base: &Chat,
) -> Result<Chat> {
    if base.attachment_revision != 0 || !base.root_attachments.is_empty() {
        return Err(AgentError::Store(
            "chat project defaults must be derived from an empty revision-zero projection".into(),
        ));
    }
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let project_roots = load_chat_project_roots(&transaction, base.project_id).await?;
    let mut chat = base.clone();
    chat.root_attachments = project_roots
        .into_iter()
        .map(|root_id| ChatRootAttachment {
            root_id,
            origin: RootAttachmentOrigin::ProjectDefault,
        })
        .collect();
    if !chat.root_attachments.is_empty() {
        chat.attachment_revision = 1;
    }
    validate_chat_root_projection(&chat).map_err(|message| AgentError::Store(message.into()))?;
    insert_chat_on(&transaction, &chat).await?;
    insert_foreground_agent_run_on(&transaction, chat.id, chat.created_at).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(chat)
}

async fn load_chat_project_roots<C>(
    conn: &C,
    project_id: Option<ProjectId>,
) -> Result<Vec<HostRootId>>
where
    C: ConnectionTrait,
{
    let Some(project_id) = project_id else {
        return Ok(Vec::new());
    };
    let mut rows = entities::project::Entity::find_by_id(project_id.0)
        .find_with_related(entities::project_root_attachment::Entity)
        .order_by_asc(entities::project_root_attachment::Column::Position)
        .all(conn)
        .await
        .map_err(store_err)?;
    let (model, roots) = rows
        .pop()
        .ok_or_else(|| AgentError::Store(format!("chat project {project_id} does not exist")))?;
    Ok(project_from_models(model, roots)?.root_attachments)
}

async fn insert_chat_on<C>(conn: &C, chat: &Chat) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::chat::ActiveModel {
        id: Set(chat.id.0),
        project_id: Set(chat.project_id.map(|p| p.0)),
        title: Set(chat.title.clone()),
        model: Set(chat.model.clone()),
        attachment_revision: Set(chat.attachment_revision),
        created_at: Set(chat.created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    for (position, attachment) in chat.root_attachments.iter().copied().enumerate() {
        entities::chat_root_attachment::ActiveModel {
            chat_id: Set(chat.id.0),
            root_id: Set(*attachment.root_id.as_uuid()),
            position: Set(i32::try_from(position)
                .map_err(|_| AgentError::Store("chat root position exceeds i32".into()))?),
            origin: Set(attachment_origin_to_db(attachment.origin).to_owned()),
        }
        .insert(conn)
        .await
        .map_err(store_err)?;
    }
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
    let mut rows = entities::chat::Entity::find_by_id(id.0)
        .find_with_related(entities::chat_root_attachment::Entity)
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(&store.conn)
        .await
        .map_err(store_err)?;
    rows.pop()
        .map(|(model, roots)| chat_from_models(model, roots))
        .transpose()
}

pub(in crate::db) async fn list_chats(store: &DbStore) -> Result<Vec<Chat>> {
    let mut chats = entities::chat::Entity::find()
        .find_with_related(entities::chat_root_attachment::Entity)
        .order_by_asc(entities::chat_root_attachment::Column::Position)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|(model, roots)| chat_from_models(model, roots))
        .collect::<Result<Vec<_>>>()?;
    chats.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.0.cmp(&left.id.0))
    });
    Ok(chats)
}

pub(in crate::db) async fn append_message(store: &DbStore, message: &Message) -> Result<()> {
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, message.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            message.chat_id
        )));
    }
    if !reserve_message_identity_on(
        &transaction,
        message.id,
        message.chat_id,
        message.turn_id,
        MESSAGE_IDENTITY_OWNER_MESSAGE,
    )
    .await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "message identity {} is already reserved",
            message.id
        )));
    }
    let seq = next_message_seq_on(&transaction, message.chat_id).await?;
    let active = entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        seq: Set(seq),
        role: Set(role_to_db(message.role).to_string()),
        content: Set(message.content.clone()),
        created_at: Set(message.created_at),
    };
    if let Err(error) = active.insert(&transaction).await {
        transaction.rollback().await.map_err(store_err)?;
        return Err(store_err(error));
    }
    transaction.commit().await.map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn list_messages(store: &DbStore, chat_id: ChatId) -> Result<Vec<Message>> {
    entities::message::Entity::find()
        .filter(entities::message::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::message::Column::Seq)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(message_from_model)
        .collect()
}

pub(in crate::db) async fn list_tool_calls(
    store: &DbStore,
    chat_id: ChatId,
) -> Result<Vec<ToolCallRecord>> {
    entities::tool_call::Entity::find()
        .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::tool_call::Column::CreatedAt)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(super::client_execution::tool_call_from_model)
        .collect()
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

    let seq = append_event_on(&transaction, chat_id, None, None, None, None, event).await?;
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
        || turn.claim_count != claim.claim_count
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
        None,
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
    scan_token: Option<uuid::Uuid>,
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
        scan_token: Set(scan_token),
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

fn chat_from_models(
    model: entities::chat::Model,
    rows: Vec<entities::chat_root_attachment::Model>,
) -> Result<Chat> {
    if rows.len() > MAX_ROOT_ATTACHMENTS {
        return Err(AgentError::Store(format!(
            "chat {} exceeds the root attachment limit",
            model.id
        )));
    }
    let root_attachments = rows
        .into_iter()
        .enumerate()
        .map(|(expected, row)| {
            if usize::try_from(row.position).ok() != Some(expected) {
                return Err(AgentError::Store(format!(
                    "chat {} root positions are not contiguous",
                    model.id
                )));
            }
            Ok(ChatRootAttachment {
                root_id: HostRootId::from_uuid(row.root_id).map_err(|error| {
                    AgentError::Store(format!("chat {} has an invalid root id: {error}", model.id))
                })?,
                origin: attachment_origin_from_db(&row.origin)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let chat = Chat {
        id: ChatId(model.id),
        project_id: model.project_id.map(ProjectId),
        title: model.title,
        model: model.model,
        attachment_revision: model.attachment_revision,
        root_attachments,
        created_at: model.created_at,
    };
    validate_chat_root_projection(&chat).map_err(|message| AgentError::Store(message.into()))?;
    Ok(chat)
}

fn validate_chat_attachments(chat: &Chat, project_roots: &[HostRootId]) -> Result<()> {
    validate_chat_root_projection_against_project(chat, project_roots)
        .map_err(|message| AgentError::Store(message.into()))
}

pub(in crate::db) fn attachment_origin_to_db(origin: RootAttachmentOrigin) -> &'static str {
    match origin {
        RootAttachmentOrigin::ProjectDefault => "project_default",
        RootAttachmentOrigin::Conversation => "conversation",
    }
}

pub(in crate::db) fn attachment_origin_from_db(value: &str) -> Result<RootAttachmentOrigin> {
    match value {
        "project_default" => Ok(RootAttachmentOrigin::ProjectDefault),
        "conversation" => Ok(RootAttachmentOrigin::Conversation),
        other => Err(AgentError::Store(format!(
            "unknown chat root attachment origin: {other}"
        ))),
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
