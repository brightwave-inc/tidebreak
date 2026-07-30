use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

use crate::error::{AgentError, Result};
use crate::model::{DocumentRecord, DocumentSourceUpsert};

use super::super::{
    document_from_model, entities, store_err, validate_document_source_blob, DbStore,
};
use super::blob as blob_ops;
use super::require_document_scope_write_lock;

pub(in crate::db) async fn accept_source(
    store: &DbStore,
    source: &DocumentSourceUpsert,
) -> Result<DocumentRecord> {
    validate_source_input(source)?;

    let transaction = store.conn.begin().await.map_err(store_err)?;
    require_document_scope_write_lock(&transaction, source.chat_id, source.project_id).await?;
    let existing = entities::document::Entity::find_by_id(source.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?;
    if let Some(current) = existing.as_ref() {
        if current.chat_id != source.chat_id.map(|id| id.0)
            || current.project_id != source.project_id.map(|id| id.0)
        {
            return Err(AgentError::Store(format!(
                "document {} cannot move between document corpora",
                source.id
            )));
        }
    }

    let byte_len = i64::try_from(source.source_blob.byte_len)
        .map_err(|_| AgentError::Store("document source is too large".into()))?;
    let active = entities::document::ActiveModel {
        id: Set(source.id.0),
        chat_id: Set(source.chat_id.map(|id| id.0)),
        project_id: Set(source.project_id.map(|id| id.0)),
        source_uri: Set(source.source_uri.clone()),
        media_type: Set(source.media_type.clone()),
        title: Set(source.title.clone()),
        source_blob_id: Set(Some(source.source_blob.id)),
        source_sha256: Set(Some(source.source_blob.sha256.to_vec())),
        source_byte_len: Set(Some(byte_len)),
        canonical_text: Set(source.canonical_text.clone()),
        created_at: Set(existing
            .as_ref()
            .map_or(source.updated_at, |current| current.created_at)),
        updated_at: Set(source.updated_at),
    };
    if existing.is_some() {
        active.update(&transaction).await.map_err(store_err)?;
    } else {
        active.insert(&transaction).await.map_err(store_err)?;
    }

    blob_ops::replace_reference_on(
        &transaction,
        existing.as_ref().and_then(|current| current.source_blob_id),
        source.source_blob.id,
    )
    .await?;
    let record = entities::document::Entity::find_by_id(source.id.0)
        .one(&transaction)
        .await
        .map_err(store_err)?
        .ok_or_else(|| AgentError::Store("accepted source document disappeared".into()))?;
    let record = document_from_model(record)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

fn validate_source_input(source: &DocumentSourceUpsert) -> Result<()> {
    if source.media_type.is_empty() || source.source_uri.as_deref() == Some("") {
        return Err(AgentError::Store("invalid document source metadata".into()));
    }
    validate_document_source_blob(&source.source_blob)?;
    Ok(())
}
