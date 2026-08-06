use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::citation::{
    parse_assistant_citations, AssistantCitationInput, AssistantCitationSnapshot, CitationLocator,
    MAX_ASSISTANT_CITATIONS,
};
use crate::error::{AgentError, Result};
use crate::id::{AssistantCitationId, ChatId, MessageId};
use crate::model::{Message, Role};
use crate::storage::{AppendClaimedMessageOutcome, ChatCitationSnapshot, TurnLeaseFence};

use super::super::{entities, store_err, DbStore};
use super::conversation::{
    next_message_seq_on, reasoning_to_db, reserve_message_identity_on,
    MESSAGE_IDENTITY_OWNER_MESSAGE,
};
use super::{acquire_chat_write_lock, acquire_turn_write_lock};

pub(in crate::db) async fn append_assistant_message(
    store: &DbStore,
    message: &Message,
    citations: &[AssistantCitationInput],
) -> Result<()> {
    validate_assistant_message(message, citations)?;
    let created_at = super::turn::canonical_db_timestamp(message.created_at)?;
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
        if exact_appended_message_on(store, message, citations).await? {
            return Ok(());
        }
        return Err(AgentError::Store(format!(
            "message identity {} is already reserved",
            message.id
        )));
    }
    entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        seq: Set(next_message_seq_on(&transaction, message.chat_id).await?),
        role: Set("assistant".into()),
        content: Set(message.content.clone()),
        llm_content: Set(message.llm_content.clone()),
        reasoning: Set(reasoning_to_db(&message.reasoning)),
        turn_lease_token: Set(None),
        created_at: Set(created_at),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    insert_for_message_on(&transaction, message, citations).await?;
    transaction.commit().await.map_err(store_err)
}

pub(in crate::db) async fn append_claimed_assistant_message(
    store: &DbStore,
    message: &Message,
    citations: &[AssistantCitationInput],
    lease_token: uuid::Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<AppendClaimedMessageOutcome> {
    validate_assistant_message(message, citations)?;
    if lease_token.is_nil() {
        return Err(AgentError::Store(
            "claimed message lease token must not be nil".into(),
        ));
    }
    let created_at = super::turn::canonical_db_timestamp(message.created_at)?;
    let now = super::turn::canonical_db_timestamp(now)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, message.chat_id).await?
        || !acquire_turn_write_lock(&transaction, message.turn_id).await?
    {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(AppendClaimedMessageOutcome::LeaseLost);
    }
    if let Some(existing) = entities::message::Entity::find_by_id(message.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
    {
        let exact = exact_appended_message_model_on(
            &transaction,
            &existing,
            message,
            citations,
            Some(lease_token),
        )
        .await?;
        transaction.commit().await.map_err(store_err)?;
        return Ok(if exact {
            AppendClaimedMessageOutcome::Existing
        } else {
            AppendClaimedMessageOutcome::IdentityConflict
        });
    }
    if super::turn::turn_lease_is_current_on(&transaction, message.turn_id, lease_token, now)
        .await?
        != TurnLeaseFence::Current
    {
        transaction.commit().await.map_err(store_err)?;
        return Ok(AppendClaimedMessageOutcome::LeaseLost);
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
        transaction.commit().await.map_err(store_err)?;
        return Ok(AppendClaimedMessageOutcome::IdentityConflict);
    }
    entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        seq: Set(next_message_seq_on(&transaction, message.chat_id).await?),
        role: Set("assistant".into()),
        content: Set(message.content.clone()),
        llm_content: Set(message.llm_content.clone()),
        reasoning: Set(reasoning_to_db(&message.reasoning)),
        turn_lease_token: Set(Some(lease_token)),
        created_at: Set(created_at),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    insert_for_message_on(&transaction, message, citations).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(AppendClaimedMessageOutcome::Appended)
}

async fn exact_appended_message_on(
    store: &DbStore,
    message: &Message,
    citations: &[AssistantCitationInput],
) -> Result<bool> {
    let Some(stored) = entities::message::Entity::find_by_id(message.id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    exact_appended_message_model_on(&store.conn, &stored, message, citations, None).await
}

async fn exact_appended_message_model_on<C>(
    conn: &C,
    stored: &entities::message::Model,
    message: &Message,
    citations: &[AssistantCitationInput],
    turn_lease_token: Option<uuid::Uuid>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    let created_at = super::turn::canonical_db_timestamp(message.created_at)?;
    if stored.chat_id != message.chat_id.0
        || stored.turn_id != message.turn_id.0
        || stored.role != "assistant"
        || stored.content != message.content
        || stored.llm_content != message.llm_content
        || stored.reasoning != reasoning_to_db(&message.reasoning)
        || stored.turn_lease_token != turn_lease_token
        || stored.created_at != created_at
    {
        return Ok(false);
    }
    Ok(exact_citations_for_message_on(conn, message.id).await? == citations)
}

pub(in crate::db) fn validate_assistant_message(
    message: &Message,
    citations: &[AssistantCitationInput],
) -> Result<()> {
    if message.role != Role::Assistant || message.id.0.is_nil() {
        return Err(AgentError::Store(
            "citations require a non-nil assistant message".into(),
        ));
    }
    let parsed = parse_assistant_citations(&message.content);
    if citations.len() > MAX_ASSISTANT_CITATIONS
        || citations.iter().any(|citation| !citation.is_valid())
        || parsed.citations != citations
    {
        return Err(AgentError::Store(
            "assistant citation inputs do not match the message".into(),
        ));
    }
    Ok(())
}

pub(in crate::db) async fn insert_for_message_on<C>(
    conn: &C,
    message: &Message,
    citations: &[AssistantCitationInput],
) -> Result<()>
where
    C: ConnectionTrait,
{
    let mut rows = Vec::with_capacity(citations.len());
    for (index, citation) in citations.iter().enumerate() {
        let ordinal = u16::try_from(index + 1).expect("citation limit fits u16");
        rows.push(entities::assistant_citation::ActiveModel {
            id: Set(AssistantCitationId::derive(message.id, ordinal).0),
            message_id: Set(message.id.0),
            ordinal: Set(i32::from(ordinal)),
            document_id: Set(citation.document_id.0),
            locator: Set(serde_json::to_value(&citation.locator)
                .map_err(|error| AgentError::Store(error.to_string()))?),
        });
    }
    if !rows.is_empty() {
        entities::assistant_citation::Entity::insert_many(rows)
            .exec(conn)
            .await
            .map_err(store_err)?;
    }
    Ok(())
}

pub(in crate::db) async fn exact_citations_for_message_on<C>(
    conn: &C,
    message_id: MessageId,
) -> Result<Vec<AssistantCitationInput>>
where
    C: ConnectionTrait,
{
    entities::assistant_citation::Entity::find()
        .filter(entities::assistant_citation::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::assistant_citation::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(|row| {
            Ok(AssistantCitationInput {
                document_id: crate::DocumentId(row.document_id),
                locator: serde_json::from_value::<CitationLocator>(row.locator)
                    .map_err(|error| AgentError::Store(error.to_string()))?,
            })
        })
        .collect()
}

pub(in crate::db) async fn list_snapshots_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<ChatCitationSnapshot>>
where
    C: ConnectionTrait,
{
    let rows = entities::assistant_citation::Entity::find()
        .find_also_related(entities::message::Entity)
        .order_by_asc(entities::assistant_citation::Column::MessageId)
        .order_by_asc(entities::assistant_citation::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    rows.into_iter()
        .filter_map(|(row, message)| {
            message
                .is_some_and(|message| message.chat_id == chat_id.0)
                .then_some(row)
        })
        .map(|row| {
            let ordinal = u16::try_from(row.ordinal)
                .map_err(|_| AgentError::Store("invalid assistant citation ordinal".into()))?;
            if row.id != AssistantCitationId::derive(MessageId(row.message_id), ordinal).0 {
                return Err(AgentError::Store(
                    "assistant citation identity is corrupt".into(),
                ));
            }
            Ok(ChatCitationSnapshot {
                message_id: MessageId(row.message_id),
                citation: AssistantCitationSnapshot {
                    id: AssistantCitationId(row.id),
                    ordinal,
                    document_id: crate::DocumentId(row.document_id),
                    locator: serde_json::from_value(row.locator)
                        .map_err(|error| AgentError::Store(error.to_string()))?,
                },
            })
        })
        .collect()
}
