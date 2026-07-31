//! Durable conversation outputs and their append-only revision history.
//!
//! Every mutation is keyed by a caller-minted identity so an ambiguous store
//! response can be retried without creating a second output or a second
//! revision. Revisions are insert-only: an update appends, and the previous
//! bytes stay addressable by their own revision id.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::deliverable::{
    deliverable_media_type, revision_byte_ceiling, validate_binary_deliverable,
    validate_deliverable_name, CreateOutput, DeliverableKind, NewOutputRevision, OutputRecord,
    OutputRevision, MAX_OUTPUT_REVISIONS,
};
use crate::error::{AgentError, Result};
use crate::id::{ChatId, OutputId, OutputRevisionId};

use super::super::{entities, store_err, DbStore};
use super::acquire_chat_write_lock;
use super::turn::canonical_db_timestamp;

pub(in crate::db) async fn create_output(
    store: &DbStore,
    request: &CreateOutput,
) -> Result<OutputRecord> {
    let media_type = validate_new_output(request)?;
    let created_at = canonical_db_timestamp(request.revision.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if !acquire_chat_write_lock(&transaction, request.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            request.chat_id
        )));
    }
    if let Some(existing) = find_output_on(&transaction, request.id).await? {
        // An exact retry must return the original record rather than fail. A
        // reused id that describes different content is a caller bug.
        let exact =
            exact_output_on(&transaction, &existing, request, &media_type, created_at).await?;
        transaction.rollback().await.map_err(store_err)?;
        return if exact {
            Ok(existing)
        } else {
            Err(AgentError::Store(format!(
                "output {} already exists with different content",
                request.id
            )))
        };
    }
    entities::output::ActiveModel {
        id: Set(request.id.0),
        chat_id: Set(request.chat_id.0),
        filename: Set(request.filename.clone()),
        media_type: Set(media_type.clone()),
        current_revision_id: Set(request.revision.id.0),
        revision_count: Set(1),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    insert_revision_on(&transaction, request.id, 1, &request.revision, created_at).await?;
    let record = require_output_on(&transaction, request.id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

pub(in crate::db) async fn append_output_revision(
    store: &DbStore,
    output_id: OutputId,
    revision: &NewOutputRevision,
) -> Result<OutputRecord> {
    let created_at = canonical_db_timestamp(revision.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_output_on(&transaction, output_id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("output {output_id} not found")));
    };
    // The output's fixed media type fixes the revision size ceiling: a binary
    // output accepts binary-sized revisions, a text output only text-sized ones.
    if let Err(error) = validate_revision(revision, revision_byte_ceiling(&existing.media_type)) {
        transaction.rollback().await.map_err(store_err)?;
        return Err(error);
    }
    // Take the owning chat's write lock so two concurrent revisions cannot
    // both read the same ordinal and race to publish a current revision.
    if !acquire_chat_write_lock(&transaction, existing.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            existing.chat_id
        )));
    }
    if existing.deleted_at.is_some() {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("output {output_id} is deleted")));
    }
    if let Some(recorded) = find_revision_on(&transaction, revision.id).await? {
        let exact =
            recorded.output_id == output_id && revision_matches(&recorded, revision, created_at);
        transaction.rollback().await.map_err(store_err)?;
        return if exact {
            require_output(store, output_id).await
        } else {
            Err(AgentError::Store(format!(
                "output revision {} already exists with different content",
                revision.id
            )))
        };
    }
    let existing = require_output_on(&transaction, output_id).await?;
    let ordinal = existing.revision_count + 1;
    if ordinal > MAX_OUTPUT_REVISIONS {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "output {output_id} has reached its {MAX_OUTPUT_REVISIONS}-revision limit; save the file under a new filename to publish further changes"
        )));
    }
    insert_revision_on(&transaction, output_id, ordinal, revision, created_at).await?;
    entities::output::ActiveModel {
        id: Set(output_id.0),
        current_revision_id: Set(revision.id.0),
        revision_count: Set(i32::try_from(ordinal).map_err(|_| {
            AgentError::Store("output revision count is outside the database range".into())
        })?),
        updated_at: Set(created_at),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let record = require_output_on(&transaction, output_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

pub(in crate::db) async fn get_output(
    store: &DbStore,
    id: OutputId,
) -> Result<Option<OutputRecord>> {
    find_output_on(&store.conn, id).await
}

pub(in crate::db) async fn list_outputs(
    store: &DbStore,
    chat_id: ChatId,
    limit: u64,
) -> Result<Vec<OutputRecord>> {
    entities::output::Entity::find()
        .filter(entities::output::Column::ChatId.eq(chat_id.0))
        .filter(entities::output::Column::DeletedAt.is_null())
        .order_by_desc(entities::output::Column::UpdatedAt)
        .order_by_desc(entities::output::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(output_from_model)
        .collect()
}

pub(in crate::db) async fn list_output_revisions(
    store: &DbStore,
    output_id: OutputId,
) -> Result<Vec<OutputRevision>> {
    entities::output_revision::Entity::find()
        .filter(entities::output_revision::Column::OutputId.eq(output_id.0))
        .order_by_desc(entities::output_revision::Column::Ordinal)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(revision_from_model)
        .collect()
}

pub(in crate::db) async fn get_output_revision(
    store: &DbStore,
    id: OutputRevisionId,
) -> Result<Option<OutputRevision>> {
    find_revision_on(&store.conn, id).await
}

pub(in crate::db) async fn delete_output(
    store: &DbStore,
    id: OutputId,
    deleted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let deleted_at = canonical_db_timestamp(deleted_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_output_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    };
    if existing.deleted_at.is_some() {
        // Deleting twice is the same durable outcome, not a conflict.
        transaction.rollback().await.map_err(store_err)?;
        return Ok(true);
    }
    entities::output::ActiveModel {
        id: Set(id.0),
        deleted_at: Set(Some(deleted_at)),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

pub(in crate::db) async fn restore_output(
    store: &DbStore,
    id: OutputId,
    restored_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let restored_at = canonical_db_timestamp(restored_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_output_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    };
    if existing.deleted_at.is_none() {
        // Restoring a live output is the same durable outcome, not a conflict.
        transaction.rollback().await.map_err(store_err)?;
        return Ok(true);
    }
    // Clearing the soft-delete is the exact inverse of `delete_output`; the
    // revision history is untouched. Surfacing the restored output as freshly
    // updated keeps a reversed retraction visible at the top of the catalog.
    entities::output::ActiveModel {
        id: Set(id.0),
        deleted_at: Set(None),
        updated_at: Set(restored_at),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    transaction.commit().await.map_err(store_err)?;
    Ok(true)
}

pub(in crate::db) async fn set_current_output_revision(
    store: &DbStore,
    output_id: OutputId,
    revision_id: OutputRevisionId,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<OutputRecord> {
    let updated_at = canonical_db_timestamp(updated_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_output_on(&transaction, output_id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("output {output_id} not found")));
    };
    // Serialize against a concurrent append so a revert and a new revision
    // cannot both publish a current revision from the same starting point.
    if !acquire_chat_write_lock(&transaction, existing.chat_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "chat {} does not exist",
            existing.chat_id
        )));
    }
    if existing.deleted_at.is_some() {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("output {output_id} is deleted")));
    }
    // The target must be a revision already recorded for this exact output.
    // Reverting never mints a revision, so an unknown or foreign id is a caller
    // bug rather than a new version.
    let Some(revision) = find_revision_on(&transaction, revision_id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "output revision {revision_id} not found"
        )));
    };
    if revision.output_id != output_id {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "output revision {revision_id} does not belong to output {output_id}"
        )));
    }
    entities::output::ActiveModel {
        id: Set(output_id.0),
        current_revision_id: Set(revision_id.0),
        updated_at: Set(updated_at),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let record = require_output_on(&transaction, output_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

fn validate_new_output(request: &CreateOutput) -> Result<String> {
    let media_type = match &request.kind {
        DeliverableKind::Text => {
            validate_deliverable_name(&request.filename).map_err(|message| {
                AgentError::Store(format!("invalid output filename: {message}"))
            })?;
            deliverable_media_type(&request.filename)
                .ok_or_else(|| {
                    AgentError::Store("output filename has no supported media type".into())
                })?
                .to_owned()
        }
        DeliverableKind::Binary { media_type } => {
            validate_binary_deliverable(&request.filename, media_type).map_err(|message| {
                AgentError::Store(format!("invalid binary output: {message}"))
            })?;
            media_type.clone()
        }
    };
    validate_revision(&request.revision, revision_byte_ceiling(&media_type))?;
    Ok(media_type)
}

fn validate_revision(revision: &NewOutputRevision, byte_ceiling: usize) -> Result<()> {
    if revision.byte_len > byte_ceiling as u64 {
        return Err(AgentError::Store(format!(
            "output revision is too large (maximum {byte_ceiling} bytes)"
        )));
    }
    // A revision records the foreground turn or the background run that produced
    // it, never both.
    if revision.turn_id.is_some() && revision.producing_run_id.is_some() {
        return Err(AgentError::Store(
            "output revision names both a producing turn and a producing run".into(),
        ));
    }
    Ok(())
}

async fn insert_revision_on<C>(
    conn: &C,
    output_id: OutputId,
    ordinal: u32,
    revision: &NewOutputRevision,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::output_revision::ActiveModel {
        id: Set(revision.id.0),
        output_id: Set(output_id.0),
        ordinal: Set(i32::try_from(ordinal).map_err(|_| {
            AgentError::Store("output revision ordinal is outside the database range".into())
        })?),
        byte_len: Set(i64::try_from(revision.byte_len).map_err(|_| {
            AgentError::Store("output revision length is outside the database range".into())
        })?),
        sha256: Set(revision.sha256.to_vec()),
        turn_id: Set(revision.turn_id.map(|turn_id| turn_id.0)),
        producing_run_id: Set(revision.producing_run_id.map(|run_id| run_id.0)),
        created_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

async fn exact_output_on<C>(
    conn: &C,
    stored: &OutputRecord,
    request: &CreateOutput,
    media_type: &str,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if stored.chat_id != request.chat_id
        || stored.filename != request.filename
        || stored.media_type != media_type
        || stored.current_revision != request.revision.id
        || stored.revision_count != 1
        || stored.created_at != created_at
        || stored.updated_at != created_at
        || stored.deleted_at.is_some()
    {
        return Ok(false);
    }
    let Some(revision) = find_revision_on(conn, request.revision.id).await? else {
        return Err(AgentError::Store(
            "output record has no current revision".into(),
        ));
    };
    Ok(revision.output_id == request.id
        && revision_matches(&revision, &request.revision, created_at))
}

fn revision_matches(
    stored: &OutputRevision,
    request: &NewOutputRevision,
    created_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    stored.id == request.id
        && stored.byte_len == request.byte_len
        && stored.sha256 == request.sha256
        && stored.turn_id == request.turn_id
        && stored.producing_run_id == request.producing_run_id
        && stored.created_at == created_at
}

async fn find_output_on<C>(conn: &C, id: OutputId) -> Result<Option<OutputRecord>>
where
    C: ConnectionTrait,
{
    entities::output::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(output_from_model)
        .transpose()
}

async fn require_output_on<C>(conn: &C, id: OutputId) -> Result<OutputRecord>
where
    C: ConnectionTrait,
{
    find_output_on(conn, id)
        .await?
        .ok_or_else(|| AgentError::Store(format!("output {id} not found")))
}

async fn require_output(store: &DbStore, id: OutputId) -> Result<OutputRecord> {
    require_output_on(&store.conn, id).await
}

async fn find_revision_on<C>(conn: &C, id: OutputRevisionId) -> Result<Option<OutputRevision>>
where
    C: ConnectionTrait,
{
    entities::output_revision::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(revision_from_model)
        .transpose()
}

fn output_from_model(model: entities::output::Model) -> Result<OutputRecord> {
    Ok(OutputRecord {
        id: OutputId(model.id),
        chat_id: ChatId(model.chat_id),
        filename: model.filename,
        media_type: model.media_type,
        current_revision: OutputRevisionId(model.current_revision_id),
        revision_count: u32::try_from(model.revision_count)
            .map_err(|_| AgentError::Store("stored output revision count is negative".into()))?,
        created_at: model.created_at,
        updated_at: model.updated_at,
        deleted_at: model.deleted_at,
    })
}

fn revision_from_model(model: entities::output_revision::Model) -> Result<OutputRevision> {
    let sha256: [u8; 32] = model
        .sha256
        .try_into()
        .map_err(|_| AgentError::Store("stored output revision digest is malformed".into()))?;
    Ok(OutputRevision {
        id: OutputRevisionId(model.id),
        output_id: OutputId(model.output_id),
        ordinal: u32::try_from(model.ordinal)
            .map_err(|_| AgentError::Store("stored output revision ordinal is negative".into()))?,
        byte_len: u64::try_from(model.byte_len)
            .map_err(|_| AgentError::Store("stored output revision length is negative".into()))?,
        sha256,
        turn_id: model.turn_id.map(crate::id::TurnId),
        producing_run_id: model.producing_run_id.map(crate::id::AgentRunId),
        created_at: model.created_at,
    })
}
