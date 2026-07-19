use std::collections::HashSet;

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};

use crate::citation::{
    parse_assistant_citations, AssistantCitationReference, AssistantCitationSnapshot,
    MAX_ASSISTANT_CITATIONS, MAX_CITATION_EXCERPT_CHARS, MAX_CITATION_HEADING_CHARS,
    MAX_CITATION_PAGES,
};
use crate::error::{AgentError, Result};
use crate::id::{AssistantCitationId, ChatId, MessageId, TurnId};
use crate::model::{Message, Role, SourceLocation};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::conversation::{
    next_message_seq_on, reserve_message_identity_on, MESSAGE_IDENTITY_OWNER_MESSAGE,
};

pub(in crate::db) async fn append_assistant_message(
    store: &DbStore,
    message: &Message,
    references: &[AssistantCitationReference],
) -> Result<()> {
    validate_assistant_message(message, references)?;
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
        if exact_appended_message_on(store, message, references).await? {
            return Ok(());
        }
        return Err(AgentError::Store(format!(
            "message identity {} is already reserved",
            message.id
        )));
    }
    let evidence =
        resolve_references_on(&transaction, message.chat_id, message.turn_id, references).await?;
    entities::message::ActiveModel {
        id: Set(message.id.0),
        chat_id: Set(message.chat_id.0),
        turn_id: Set(message.turn_id.0),
        seq: Set(next_message_seq_on(&transaction, message.chat_id).await?),
        role: Set("assistant".into()),
        content: Set(message.content.clone()),
        created_at: Set(created_at),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    insert_for_message_on(&transaction, message, &evidence).await?;
    transaction.commit().await.map_err(store_err)
}

async fn exact_appended_message_on(
    store: &DbStore,
    message: &Message,
    references: &[AssistantCitationReference],
) -> Result<bool> {
    let created_at = super::turn::canonical_db_timestamp(message.created_at)?;
    let Some(stored) = entities::message::Entity::find_by_id(message.id.0)
        .one(&store.conn)
        .await
        .map_err(store_err)?
    else {
        return Ok(false);
    };
    if stored.chat_id != message.chat_id.0
        || stored.turn_id != message.turn_id.0
        || stored.role != "assistant"
        || stored.content != message.content
        || stored.created_at != created_at
    {
        return Ok(false);
    }
    let expected = resolve_references_on(&store.conn, message.chat_id, message.turn_id, references)
        .await?
        .into_iter()
        .map(|evidence| AssistantCitationReference {
            source_token: evidence.source_token,
        })
        .collect::<Vec<_>>();
    Ok(exact_references_for_message_on(&store.conn, message.id).await? == expected)
}

pub(in crate::db) fn validate_assistant_message(
    message: &Message,
    references: &[AssistantCitationReference],
) -> Result<()> {
    if message.role != Role::Assistant || message.id.0.is_nil() {
        return Err(AgentError::Store(
            "citations require a non-nil assistant message".into(),
        ));
    }
    if parse_assistant_citations(&message.content).content != message.content {
        return Err(AgentError::Store(
            "assistant message contains an unstripped source reference".into(),
        ));
    }
    if references.len() > MAX_ASSISTANT_CITATIONS
        || references.iter().copied().collect::<HashSet<_>>().len() != references.len()
    {
        return Err(AgentError::Store(
            "assistant citation references are invalid".into(),
        ));
    }
    Ok(())
}

pub(in crate::db) async fn resolve_references_on<C>(
    conn: &C,
    chat_id: ChatId,
    turn_id: TurnId,
    references: &[AssistantCitationReference],
) -> Result<Vec<entities::retrieval_evidence::Model>>
where
    C: ConnectionTrait,
{
    let mut evidence = Vec::with_capacity(references.len());
    for reference in references {
        if let Some(row) = entities::retrieval_evidence::Entity::find()
            .filter(entities::retrieval_evidence::Column::SourceToken.eq(reference.source_token))
            .filter(entities::retrieval_evidence::Column::ChatId.eq(chat_id.0))
            .filter(entities::retrieval_evidence::Column::TurnId.eq(turn_id.0))
            .one(conn)
            .await
            .map_err(store_err)?
        {
            evidence.push(row);
        }
    }
    Ok(evidence)
}

pub(in crate::db) async fn insert_for_message_on<C>(
    conn: &C,
    message: &Message,
    evidence: &[entities::retrieval_evidence::Model],
) -> Result<()>
where
    C: ConnectionTrait,
{
    if evidence.is_empty() {
        return Ok(());
    }
    let rows = evidence
        .iter()
        .enumerate()
        .map(|(index, row)| entities::assistant_citation::ActiveModel {
            id: Set(AssistantCitationId::derive(
                message.id,
                u16::try_from(index + 1).expect("citation limit fits u16"),
            )
            .0),
            message_id: Set(message.id.0),
            ordinal: Set(i32::try_from(index + 1).expect("citation limit fits i32")),
            chat_id: Set(message.chat_id.0),
            turn_id: Set(message.turn_id.0),
            evidence_call_id: Set(row.call_id),
            evidence_rank: Set(row.rank),
        })
        .collect::<Vec<_>>();
    entities::assistant_citation::Entity::insert_many(rows)
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(())
}

pub(in crate::db) async fn exact_references_for_message_on<C>(
    conn: &C,
    message_id: MessageId,
) -> Result<Vec<AssistantCitationReference>>
where
    C: ConnectionTrait,
{
    let rows = entities::assistant_citation::Entity::find()
        .find_also_related(entities::retrieval_evidence::Entity)
        .filter(entities::assistant_citation::Column::MessageId.eq(message_id.0))
        .order_by_asc(entities::assistant_citation::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut references = Vec::with_capacity(rows.len());
    for (row, evidence) in rows {
        let evidence = evidence
            .ok_or_else(|| AgentError::Store("assistant citation evidence disappeared".into()))?;
        if evidence.chat_id != row.chat_id || evidence.turn_id != row.turn_id {
            return Err(AgentError::Store(
                "assistant citation evidence owner is corrupt".into(),
            ));
        }
        references.push(AssistantCitationReference {
            source_token: evidence.source_token,
        });
    }
    Ok(references)
}

pub(in crate::db) async fn list_snapshots_on<C>(
    conn: &C,
    chat_id: ChatId,
) -> Result<Vec<AssistantCitationSnapshot>>
where
    C: ConnectionTrait,
{
    let rows = entities::assistant_citation::Entity::find()
        .find_also_related(entities::retrieval_evidence::Entity)
        .filter(entities::assistant_citation::Column::ChatId.eq(chat_id.0))
        .order_by_asc(entities::assistant_citation::Column::MessageId)
        .order_by_asc(entities::assistant_citation::Column::Ordinal)
        .all(conn)
        .await
        .map_err(store_err)?;
    let mut snapshots = Vec::with_capacity(rows.len());
    let mut ordinal_owner = None;
    let mut expected_ordinal = 1_i32;
    for (row, evidence) in rows {
        let evidence = evidence
            .ok_or_else(|| AgentError::Store("assistant citation evidence disappeared".into()))?;
        if ordinal_owner != Some(row.message_id) {
            ordinal_owner = Some(row.message_id);
            expected_ordinal = 1;
        }
        if row.ordinal != expected_ordinal
            || !(1..=MAX_ASSISTANT_CITATIONS as i32).contains(&row.ordinal)
            || row.id
                != AssistantCitationId::derive(
                    MessageId(row.message_id),
                    u16::try_from(row.ordinal).expect("validated citation ordinal fits u16"),
                )
                .0
        {
            return Err(AgentError::Store(
                "assistant citation ordering or identity is corrupt".into(),
            ));
        }
        expected_ordinal += 1;
        let evidence = super::client_execution::evidence_from_model(evidence)?;
        if evidence.chat_id != chat_id
            || evidence.turn_id.0 != row.turn_id
            || evidence.call_id.0 != row.evidence_call_id
            || i32::from(evidence.evidence.rank) != row.evidence_rank
        {
            return Err(AgentError::Store(
                "assistant citation projection owner is corrupt".into(),
            ));
        }
        let mut pages = Vec::new();
        for region in evidence.evidence.source_regions {
            let SourceLocation::Page { number } = region.location;
            let page = number.get();
            if !pages.contains(&page) {
                pages.push(page);
                if pages.len() == MAX_CITATION_PAGES {
                    break;
                }
            }
        }
        let headings = evidence.evidence.heading_path;
        let heading = (!headings.is_empty()).then(|| {
            headings
                .join(" > ")
                .chars()
                .take(MAX_CITATION_HEADING_CHARS)
                .collect()
        });
        snapshots.push(AssistantCitationSnapshot {
            id: AssistantCitationId(row.id),
            message_id: MessageId(row.message_id),
            ordinal: u16::try_from(row.ordinal)
                .map_err(|_| AgentError::Store("invalid assistant citation ordinal".into()))?,
            excerpt: evidence
                .evidence
                .snippet
                .chars()
                .take(MAX_CITATION_EXCERPT_CHARS)
                .collect(),
            heading,
            pages,
        });
    }
    Ok(snapshots)
}
