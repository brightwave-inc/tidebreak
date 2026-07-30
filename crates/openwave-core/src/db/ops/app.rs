//! Profile-scoped local apps and their append-only revision history.
//!
//! Mirrors the conversation-output ops with one deliberate difference: there
//! is no chat anywhere in the record. Serialization of concurrent appends uses
//! the app row itself as the write lock, and the conversation a revision came
//! from is nullable provenance rather than ownership.
//!
//! Every mutation is keyed by a caller-minted identity so an ambiguous store
//! response can be retried without creating a second app or a second revision.

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};

use crate::error::{AgentError, Result};
use crate::id::{AppId, AppRevisionId};
use crate::local_app::{
    validate_app_manifest, AppManifest, AppRecord, AppRevision, CreateApp, NewAppRevision,
    MAX_APP_BUNDLE_BYTES, MAX_APP_REVISIONS,
};

use super::super::{entities, store_err, DbStore};
use super::turn::canonical_db_timestamp;

pub(in crate::db) async fn create_app(store: &DbStore, request: &CreateApp) -> Result<AppRecord> {
    let manifest_json = validate_revision(&request.revision)?;
    let created_at = canonical_db_timestamp(request.revision.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    if let Some(existing) = find_app_on(&transaction, request.id).await? {
        // An exact retry must return the original record rather than fail. A
        // reused id that describes different content is a caller bug.
        let exact = exact_app_on(&transaction, &existing, request, created_at).await?;
        transaction.rollback().await.map_err(store_err)?;
        return if exact {
            Ok(existing)
        } else {
            Err(AgentError::Store(format!(
                "app {} already exists with different content",
                request.id
            )))
        };
    }
    entities::app::ActiveModel {
        id: Set(request.id.0),
        name: Set(request.revision.manifest.name.clone()),
        current_revision_id: Set(request.revision.id.0),
        revision_count: Set(1),
        created_at: Set(created_at),
        updated_at: Set(created_at),
        deleted_at: Set(None),
    }
    .insert(&transaction)
    .await
    .map_err(store_err)?;
    insert_revision_on(
        &transaction,
        request.id,
        1,
        &request.revision,
        manifest_json,
        created_at,
    )
    .await?;
    let record = require_app_on(&transaction, request.id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

pub(in crate::db) async fn append_app_revision(
    store: &DbStore,
    app_id: AppId,
    revision: &NewAppRevision,
) -> Result<AppRecord> {
    let manifest_json = validate_revision(revision)?;
    let created_at = canonical_db_timestamp(revision.created_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    // Take the app row's write lock so two concurrent revisions cannot both
    // read the same ordinal and race to publish a current revision. There is
    // no owning chat to lock: the app row itself is the serialization point.
    if !acquire_app_write_lock(&transaction, app_id).await? {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("app {app_id} not found")));
    }
    let existing = require_app_on(&transaction, app_id).await?;
    if existing.deleted_at.is_some() {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!("app {app_id} is deleted")));
    }
    if let Some(recorded) = find_revision_on(&transaction, revision.id).await? {
        let exact = recorded.app_id == app_id && revision_matches(&recorded, revision, created_at);
        transaction.rollback().await.map_err(store_err)?;
        return if exact {
            require_app_on(&store.conn, app_id).await
        } else {
            Err(AgentError::Store(format!(
                "app revision {} already exists with different content",
                revision.id
            )))
        };
    }
    let ordinal = existing.revision_count + 1;
    if ordinal > MAX_APP_REVISIONS {
        transaction.rollback().await.map_err(store_err)?;
        return Err(AgentError::Store(format!(
            "app {app_id} has reached its {MAX_APP_REVISIONS}-revision limit"
        )));
    }
    insert_revision_on(
        &transaction,
        app_id,
        ordinal,
        revision,
        manifest_json,
        created_at,
    )
    .await?;
    entities::app::ActiveModel {
        id: Set(app_id.0),
        // The display name follows the current revision's manifest, so a
        // revision that renames the app renames the record with it.
        name: Set(revision.manifest.name.clone()),
        current_revision_id: Set(revision.id.0),
        revision_count: Set(i32::try_from(ordinal).map_err(|_| {
            AgentError::Store("app revision count is outside the database range".into())
        })?),
        updated_at: Set(created_at),
        ..Default::default()
    }
    .update(&transaction)
    .await
    .map_err(store_err)?;
    let record = require_app_on(&transaction, app_id).await?;
    transaction.commit().await.map_err(store_err)?;
    Ok(record)
}

pub(in crate::db) async fn get_app(store: &DbStore, id: AppId) -> Result<Option<AppRecord>> {
    find_app_on(&store.conn, id).await
}

pub(in crate::db) async fn list_apps(store: &DbStore, limit: u64) -> Result<Vec<AppRecord>> {
    entities::app::Entity::find()
        .filter(entities::app::Column::DeletedAt.is_null())
        .order_by_desc(entities::app::Column::UpdatedAt)
        .order_by_desc(entities::app::Column::Id)
        .limit(limit)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(app_from_model)
        .collect()
}

pub(in crate::db) async fn list_app_revisions(
    store: &DbStore,
    app_id: AppId,
) -> Result<Vec<AppRevision>> {
    entities::app_revision::Entity::find()
        .filter(entities::app_revision::Column::AppId.eq(app_id.0))
        .order_by_desc(entities::app_revision::Column::Ordinal)
        .all(&store.conn)
        .await
        .map_err(store_err)?
        .into_iter()
        .map(revision_from_model)
        .collect()
}

pub(in crate::db) async fn get_app_revision(
    store: &DbStore,
    id: AppRevisionId,
) -> Result<Option<AppRevision>> {
    find_revision_on(&store.conn, id).await
}

pub(in crate::db) async fn delete_app(
    store: &DbStore,
    id: AppId,
    deleted_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let deleted_at = canonical_db_timestamp(deleted_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_app_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    };
    if existing.deleted_at.is_some() {
        // Deleting twice is the same durable outcome, not a conflict.
        transaction.rollback().await.map_err(store_err)?;
        return Ok(true);
    }
    entities::app::ActiveModel {
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

pub(in crate::db) async fn restore_app(
    store: &DbStore,
    id: AppId,
    restored_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let restored_at = canonical_db_timestamp(restored_at)?;
    let transaction = store.conn.begin().await.map_err(store_err)?;
    let Some(existing) = find_app_on(&transaction, id).await? else {
        transaction.rollback().await.map_err(store_err)?;
        return Ok(false);
    };
    if existing.deleted_at.is_none() {
        // Restoring a live app is the same durable outcome, not a conflict.
        transaction.rollback().await.map_err(store_err)?;
        return Ok(true);
    }
    // Clearing the soft-delete is the exact inverse of `delete_app`; the
    // revision history is untouched. Surfacing the restored app as freshly
    // updated keeps a reversed deletion visible at the top of the library.
    entities::app::ActiveModel {
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

/// Acquire the shared cross-backend write lock for one app row.
///
/// Same shape as the chat/project locks: a self-assigning update serializes
/// read-then-write decisions (ordinal allocation, current-revision publication)
/// across server processes.
async fn acquire_app_write_lock<C>(conn: &C, app_id: AppId) -> Result<bool>
where
    C: ConnectionTrait,
{
    let locked = entities::app::Entity::update_many()
        .col_expr(
            entities::app::Column::Name,
            sea_orm::sea_query::Expr::col(entities::app::Column::Name),
        )
        .filter(entities::app::Column::Id.eq(app_id.0))
        .exec(conn)
        .await
        .map_err(store_err)?;
    Ok(locked.rows_affected == 1)
}

/// Validate a revision's manifest and bounds, returning the manifest JSON to
/// store.
fn validate_revision(revision: &NewAppRevision) -> Result<serde_json::Value> {
    let manifest_json = validate_app_manifest(&revision.manifest)
        .map_err(|message| AgentError::Store(format!("invalid app manifest: {message}")))?;
    if revision.byte_len == 0 {
        return Err(AgentError::Store("app bundle is empty".into()));
    }
    if revision.byte_len > MAX_APP_BUNDLE_BYTES as u64 {
        return Err(AgentError::Store(format!(
            "app bundle is too large (maximum {MAX_APP_BUNDLE_BYTES} bytes)"
        )));
    }
    // A revision records the foreground turn or the background run that
    // produced it, never both.
    if revision.turn_id.is_some() && revision.producing_run_id.is_some() {
        return Err(AgentError::Store(
            "app revision names both a producing turn and a producing run".into(),
        ));
    }
    Ok(manifest_json)
}

async fn insert_revision_on<C>(
    conn: &C,
    app_id: AppId,
    ordinal: u32,
    revision: &NewAppRevision,
    manifest_json: serde_json::Value,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<()>
where
    C: ConnectionTrait,
{
    entities::app_revision::ActiveModel {
        id: Set(revision.id.0),
        app_id: Set(app_id.0),
        ordinal: Set(i32::try_from(ordinal).map_err(|_| {
            AgentError::Store("app revision ordinal is outside the database range".into())
        })?),
        manifest_json: Set(manifest_json),
        byte_len: Set(i64::try_from(revision.byte_len).map_err(|_| {
            AgentError::Store("app revision length is outside the database range".into())
        })?),
        sha256: Set(revision.sha256.to_vec()),
        turn_id: Set(revision.turn_id.map(|turn_id| turn_id.0)),
        producing_run_id: Set(revision.producing_run_id.map(|run_id| run_id.0)),
        chat_id: Set(revision.chat_id.map(|chat_id| chat_id.0)),
        created_at: Set(created_at),
    }
    .insert(conn)
    .await
    .map_err(store_err)?;
    Ok(())
}

async fn exact_app_on<C>(
    conn: &C,
    stored: &AppRecord,
    request: &CreateApp,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Result<bool>
where
    C: ConnectionTrait,
{
    if stored.name != request.revision.manifest.name
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
            "app record has no current revision".into(),
        ));
    };
    Ok(revision.app_id == request.id && revision_matches(&revision, &request.revision, created_at))
}

fn revision_matches(
    stored: &AppRevision,
    request: &NewAppRevision,
    created_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    stored.id == request.id
        && stored.manifest == request.manifest
        && stored.byte_len == request.byte_len
        && stored.sha256 == request.sha256
        && stored.turn_id == request.turn_id
        && stored.producing_run_id == request.producing_run_id
        && stored.chat_id == request.chat_id
        && stored.created_at == created_at
}

async fn find_app_on<C>(conn: &C, id: AppId) -> Result<Option<AppRecord>>
where
    C: ConnectionTrait,
{
    entities::app::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(app_from_model)
        .transpose()
}

async fn require_app_on<C>(conn: &C, id: AppId) -> Result<AppRecord>
where
    C: ConnectionTrait,
{
    find_app_on(conn, id)
        .await?
        .ok_or_else(|| AgentError::Store(format!("app {id} not found")))
}

async fn find_revision_on<C>(conn: &C, id: AppRevisionId) -> Result<Option<AppRevision>>
where
    C: ConnectionTrait,
{
    entities::app_revision::Entity::find_by_id(id.0)
        .one(conn)
        .await
        .map_err(store_err)?
        .map(revision_from_model)
        .transpose()
}

fn app_from_model(model: entities::app::Model) -> Result<AppRecord> {
    Ok(AppRecord {
        id: AppId(model.id),
        name: model.name,
        current_revision: AppRevisionId(model.current_revision_id),
        revision_count: u32::try_from(model.revision_count)
            .map_err(|_| AgentError::Store("stored app revision count is negative".into()))?,
        created_at: model.created_at,
        updated_at: model.updated_at,
        deleted_at: model.deleted_at,
    })
}

fn revision_from_model(model: entities::app_revision::Model) -> Result<AppRevision> {
    let sha256: [u8; 32] = model
        .sha256
        .try_into()
        .map_err(|_| AgentError::Store("stored app revision digest is malformed".into()))?;
    let manifest: AppManifest = serde_json::from_value(model.manifest_json)
        .map_err(|_| AgentError::Store("stored app manifest is malformed".into()))?;
    Ok(AppRevision {
        id: AppRevisionId(model.id),
        app_id: AppId(model.app_id),
        ordinal: u32::try_from(model.ordinal)
            .map_err(|_| AgentError::Store("stored app revision ordinal is negative".into()))?,
        manifest,
        byte_len: u64::try_from(model.byte_len)
            .map_err(|_| AgentError::Store("stored app revision length is negative".into()))?,
        sha256,
        turn_id: model.turn_id.map(crate::id::TurnId),
        producing_run_id: model.producing_run_id.map(crate::id::AgentRunId),
        chat_id: model.chat_id.map(crate::id::ChatId),
        created_at: model.created_at,
    })
}
