//! The default [`Store`], backed by SeaORM.
//!
//! One implementation and one migration set run on any SeaORM backend, chosen by
//! connection string — SQLite locally, Postgres for self-host. Types are native
//! per backend (uuid, timestamptz, jsonb on Postgres; the SQLite equivalents),
//! so nothing is stringly-encoded by hand. Enabled by the `sqlite` feature (which
//! compiles in the SQLite driver).

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, Database, DatabaseConnection, EntityTrait,
    FromQueryResult, QueryFilter, QueryOrder, QuerySelect, Set, TryInsertResult,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{CallId, ChatId, DocumentId, MessageId, ProjectId, TurnId};
use crate::model::{
    Chat, DocumentJobKind, DocumentJobStatus, DocumentListCursor, DocumentProcessingStatus,
    DocumentRecord, DocumentScope, DocumentSummaryRecord, DocumentUpsert, Message, Project, Role,
    ToolCallRecord,
};
use crate::storage::Store;

/// Map any SeaORM failure into an [`AgentError::Store`].
fn store_err(err: impl std::fmt::Display) -> AgentError {
    AgentError::Store(err.to_string())
}

/// A [`Store`] backed by a SeaORM connection (SQLite today, Postgres-ready).
#[derive(Clone)]
pub struct DbStore {
    conn: DatabaseConnection,
}

/// Projected row for metadata-only document listings. Keeping this distinct
/// from the entity model makes it impossible for this query to select the
/// canonical text or revision token by accident.
#[derive(Debug, FromQueryResult)]
struct DocumentSummaryRow {
    id: uuid::Uuid,
    project_id: Option<uuid::Uuid>,
    source_uri: Option<String>,
    media_type: String,
    title: Option<String>,
    content_revision: i64,
    processing_status: String,
    indexed_revision: Option<i64>,
    index_fingerprint: Option<String>,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    indexed_at: Option<chrono::DateTime<Utc>>,
}

impl DbStore {
    /// Connect to `url` and run migrations. For a SQLite file that should be
    /// created if missing, include `?mode=rwc` (e.g.
    /// `sqlite:///path/openwave.db?mode=rwc`).
    pub async fn connect(url: &str) -> Result<Self> {
        let conn = Database::connect(url).await.map_err(store_err)?;
        // WAL lets a reader (e.g. the UI listing chats) proceed concurrently
        // with a writer (a turn appending messages). SQLite-only; it's a
        // persistent, file-level setting, so running it once at connect suffices.
        if conn.get_database_backend() == sea_orm::DatabaseBackend::Sqlite {
            conn.execute_unprepared("PRAGMA journal_mode=WAL;")
                .await
                .map_err(store_err)?;
        }
        migration::Migrator::up(&conn, None)
            .await
            .map_err(store_err)?;
        Ok(Self { conn })
    }
}

#[async_trait]
impl Store for DbStore {
    async fn create_project(&self, project: &Project) -> Result<()> {
        entities::project::ActiveModel {
            id: Set(project.id.0),
            title: Set(project.title.clone()),
            workspace_dir: Set(project.workspace_dir.to_string_lossy().into_owned()),
            created_at: Set(project.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn get_project(&self, id: ProjectId) -> Result<Option<Project>> {
        Ok(entities::project::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(project_from_model))
    }

    async fn list_projects(&self) -> Result<Vec<Project>> {
        Ok(entities::project::Entity::find()
            .order_by_desc(entities::project::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(project_from_model)
            .collect())
    }

    async fn create_document(&self, document: &DocumentRecord) -> Result<()> {
        entities::document::ActiveModel {
            id: Set(document.id.0),
            project_id: Set(document.project_id.map(|id| id.0)),
            source_uri: Set(document.source_uri.clone()),
            media_type: Set(document.media_type.clone()),
            title: Set(document.title.clone()),
            canonical_text: Set(document.canonical_text.clone()),
            content_revision: Set(document.content_revision),
            revision_token: Set(uuid::Uuid::new_v4()),
            processing_status: Set(document.processing_status.as_str().into()),
            indexed_revision: Set(document.indexed_revision),
            index_fingerprint: Set(document.index_fingerprint.clone()),
            created_at: Set(document.created_at),
            updated_at: Set(document.updated_at),
            indexed_at: Set(document.indexed_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn get_document(&self, id: DocumentId) -> Result<Option<DocumentRecord>> {
        entities::document::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(document_from_model)
            .transpose()
    }

    async fn list_documents(&self, scope: DocumentScope) -> Result<Vec<DocumentRecord>> {
        let mut query = entities::document::Entity::find();
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
        };
        query
            .order_by_desc(entities::document::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(document_from_model)
            .collect()
    }

    async fn list_document_summaries(
        &self,
        scope: DocumentScope,
        after: Option<DocumentListCursor>,
        limit: u64,
    ) -> Result<Vec<DocumentSummaryRecord>> {
        let mut query = entities::document::Entity::find();
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
        };
        if let Some(cursor) = after {
            query = query.filter(
                sea_orm::Condition::any()
                    .add(entities::document::Column::CreatedAt.lt(cursor.created_at))
                    .add(
                        sea_orm::Condition::all()
                            .add(entities::document::Column::CreatedAt.eq(cursor.created_at))
                            .add(entities::document::Column::Id.lt(cursor.id.0)),
                    ),
            );
        }

        query
            .select_only()
            .columns([
                entities::document::Column::Id,
                entities::document::Column::ProjectId,
                entities::document::Column::SourceUri,
                entities::document::Column::MediaType,
                entities::document::Column::Title,
                entities::document::Column::ContentRevision,
                entities::document::Column::ProcessingStatus,
                entities::document::Column::IndexedRevision,
                entities::document::Column::IndexFingerprint,
                entities::document::Column::CreatedAt,
                entities::document::Column::UpdatedAt,
                entities::document::Column::IndexedAt,
            ])
            .order_by_desc(entities::document::Column::CreatedAt)
            .order_by_desc(entities::document::Column::Id)
            .limit(limit)
            .into_model::<DocumentSummaryRow>()
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(document_summary_from_row)
            .collect()
    }

    async fn list_document_ids(&self, scope: DocumentScope) -> Result<Vec<DocumentId>> {
        let mut query = entities::document::Entity::find()
            .select_only()
            .column(entities::document::Column::Id);
        query = match scope {
            DocumentScope::All => query,
            DocumentScope::Unscoped => {
                query.filter(entities::document::Column::ProjectId.is_null())
            }
            DocumentScope::Project(id) => {
                query.filter(entities::document::Column::ProjectId.eq(id.0))
            }
        };
        Ok(query
            .order_by_desc(entities::document::Column::CreatedAt)
            .into_tuple::<uuid::Uuid>()
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(DocumentId)
            .collect())
    }

    async fn delete_document(&self, id: DocumentId) -> Result<()> {
        entities::document::Entity::delete_by_id(id.0)
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn upsert_document(&self, document: &DocumentUpsert) -> Result<DocumentRecord> {
        // Optimistic compare-and-set makes the allocated revision itself the
        // result of this write. In particular, it avoids a write-then-select
        // race on SQLite where two callers can both observe the later revision.
        loop {
            let current = entities::document::Entity::find_by_id(document.id.0)
                .one(&self.conn)
                .await
                .map_err(store_err)?;

            if let Some(current) = current {
                let next_revision = current.content_revision.checked_add(1).ok_or_else(|| {
                    AgentError::Store(format!("document {} revision overflow", document.id))
                })?;
                let revision_token = uuid::Uuid::new_v4();
                let result = entities::document::Entity::update_many()
                    .col_expr(
                        entities::document::Column::ProjectId,
                        sea_orm::sea_query::Expr::value(document.project_id.map(|id| id.0)),
                    )
                    .col_expr(
                        entities::document::Column::SourceUri,
                        sea_orm::sea_query::Expr::value(document.source_uri.clone()),
                    )
                    .col_expr(
                        entities::document::Column::MediaType,
                        sea_orm::sea_query::Expr::value(document.media_type.clone()),
                    )
                    .col_expr(
                        entities::document::Column::Title,
                        sea_orm::sea_query::Expr::value(document.title.clone()),
                    )
                    .col_expr(
                        entities::document::Column::CanonicalText,
                        sea_orm::sea_query::Expr::value(document.canonical_text.clone()),
                    )
                    .col_expr(
                        entities::document::Column::ContentRevision,
                        sea_orm::sea_query::Expr::value(next_revision),
                    )
                    .col_expr(
                        entities::document::Column::RevisionToken,
                        sea_orm::sea_query::Expr::value(revision_token),
                    )
                    .col_expr(
                        entities::document::Column::ProcessingStatus,
                        sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
                    )
                    .col_expr(
                        entities::document::Column::IndexedRevision,
                        sea_orm::sea_query::Expr::value(Option::<i64>::None),
                    )
                    .col_expr(
                        entities::document::Column::IndexFingerprint,
                        sea_orm::sea_query::Expr::value(Option::<String>::None),
                    )
                    .col_expr(
                        entities::document::Column::UpdatedAt,
                        sea_orm::sea_query::Expr::value(document.updated_at),
                    )
                    .col_expr(
                        entities::document::Column::IndexedAt,
                        sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
                    )
                    .filter(entities::document::Column::Id.eq(document.id.0))
                    .filter(
                        entities::document::Column::ContentRevision.eq(current.content_revision),
                    )
                    .filter(entities::document::Column::RevisionToken.eq(current.revision_token))
                    .exec(&self.conn)
                    .await
                    .map_err(store_err)?;
                if result.rows_affected == 1 {
                    return Ok(document_from_upsert(
                        document,
                        current.created_at,
                        next_revision,
                        revision_token,
                    ));
                }
                continue;
            }

            let revision_token = uuid::Uuid::new_v4();
            let inserted = entities::document::Entity::insert(entities::document::ActiveModel {
                id: Set(document.id.0),
                project_id: Set(document.project_id.map(|id| id.0)),
                source_uri: Set(document.source_uri.clone()),
                media_type: Set(document.media_type.clone()),
                title: Set(document.title.clone()),
                canonical_text: Set(document.canonical_text.clone()),
                content_revision: Set(1),
                revision_token: Set(revision_token),
                processing_status: Set(DocumentProcessingStatus::Queued.as_str().into()),
                indexed_revision: Set(None),
                index_fingerprint: Set(None),
                created_at: Set(document.updated_at),
                updated_at: Set(document.updated_at),
                indexed_at: Set(None),
            })
            .on_conflict_do_nothing()
            .exec_without_returning(&self.conn)
            .await
            .map_err(store_err)?;
            if matches!(inserted, TryInsertResult::Inserted(1)) {
                return Ok(document_from_upsert(
                    document,
                    document.updated_at,
                    1,
                    revision_token,
                ));
            }
        }
    }

    async fn mark_document_indexed(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
        fingerprint: &str,
        indexed_at: chrono::DateTime<Utc>,
    ) -> Result<bool> {
        if fingerprint.is_empty()
            || fingerprint.chars().count() > crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
        {
            return Err(AgentError::Store(
                "document index fingerprint must contain 1 to 512 characters".into(),
            ));
        }
        let result = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::IndexedRevision,
                sea_orm::sea_query::Expr::value(Some(revision)),
            )
            .col_expr(
                entities::document::Column::IndexFingerprint,
                sea_orm::sea_query::Expr::value(Some(fingerprint.to_string())),
            )
            .col_expr(
                entities::document::Column::IndexedAt,
                sea_orm::sea_query::Expr::value(Some(indexed_at)),
            )
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Ready.as_str()),
            )
            .filter(entities::document::Column::Id.eq(id.0))
            .filter(entities::document::Column::ContentRevision.eq(revision))
            .filter(entities::document::Column::RevisionToken.eq(revision_token))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn clear_document_index(
        &self,
        id: DocumentId,
        revision: i64,
        revision_token: uuid::Uuid,
    ) -> Result<bool> {
        let result = entities::document::Entity::update_many()
            .col_expr(
                entities::document::Column::IndexedRevision,
                sea_orm::sea_query::Expr::value(Option::<i64>::None),
            )
            .col_expr(
                entities::document::Column::IndexFingerprint,
                sea_orm::sea_query::Expr::value(Option::<String>::None),
            )
            .col_expr(
                entities::document::Column::IndexedAt,
                sea_orm::sea_query::Expr::value(Option::<chrono::DateTime<Utc>>::None),
            )
            .col_expr(
                entities::document::Column::ProcessingStatus,
                sea_orm::sea_query::Expr::value(DocumentProcessingStatus::Queued.as_str()),
            )
            .filter(entities::document::Column::Id.eq(id.0))
            .filter(entities::document::Column::ContentRevision.eq(revision))
            .filter(entities::document::Column::RevisionToken.eq(revision_token))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(result.rows_affected == 1)
    }

    async fn create_chat(&self, chat: &Chat) -> Result<()> {
        entities::chat::ActiveModel {
            id: Set(chat.id.0),
            project_id: Set(chat.project_id.map(|p| p.0)),
            title: Set(chat.title.clone()),
            model: Set(chat.model.clone()),
            workspace_dir: Set(chat.workspace_dir.to_string_lossy().into_owned()),
            created_at: Set(chat.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn set_chat_model(&self, id: ChatId, model: Option<String>) -> Result<()> {
        entities::chat::Entity::update_many()
            .col_expr(
                entities::chat::Column::Model,
                sea_orm::sea_query::Expr::value(model),
            )
            .filter(entities::chat::Column::Id.eq(id.0))
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn get_chat(&self, id: ChatId) -> Result<Option<Chat>> {
        Ok(entities::chat::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(chat_from_model))
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        Ok(entities::chat::Entity::find()
            .order_by_desc(entities::chat::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(chat_from_model)
            .collect())
    }

    async fn append_message(&self, message: &Message) -> Result<()> {
        entities::message::ActiveModel {
            id: Set(message.id.0),
            chat_id: Set(message.chat_id.0),
            turn_id: Set(message.turn_id.0),
            role: Set(role_to_db(message.role).to_string()),
            content: Set(message.content.clone()),
            created_at: Set(message.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn list_messages(&self, chat_id: ChatId) -> Result<Vec<Message>> {
        entities::message::Entity::find()
            .filter(entities::message::Column::ChatId.eq(chat_id.0))
            .order_by_asc(entities::message::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(message_from_model)
            .collect()
    }

    async fn upsert_tool_call(&self, call: &ToolCallRecord) -> Result<()> {
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
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn list_tool_calls(&self, chat_id: ChatId) -> Result<Vec<ToolCallRecord>> {
        Ok(entities::tool_call::Entity::find()
            .filter(entities::tool_call::Column::ChatId.eq(chat_id.0))
            .order_by_asc(entities::tool_call::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(tool_call_from_model)
            .collect())
    }

    async fn get_setting(&self, key: &str) -> Result<Option<Value>> {
        Ok(entities::setting::Entity::find_by_id(key.to_string())
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(|model| model.value_json))
    }

    async fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        let model = entities::setting::ActiveModel {
            key: Set(key.to_string()),
            value_json: Set(value.clone()),
        };
        entities::setting::Entity::insert(model)
            .on_conflict(
                OnConflict::column(entities::setting::Column::Key)
                    .update_column(entities::setting::Column::ValueJson)
                    .to_owned(),
            )
            .exec(&self.conn)
            .await
            .map_err(store_err)?;
        Ok(())
    }

    async fn append_event(&self, chat_id: ChatId, event: &AgentEvent) -> Result<i64> {
        // Next seq for this chat. This assumes a single writer per chat —
        // the server enforces it by allowing only one active turn per chat at
        // a time (a concurrent message is refused, not queued behind a second
        // writer). Under that invariant read-then-insert is race-free; the
        // composite (chat_id, seq) primary key is the backstop that turns any
        // concurrent double-write into an error, never a silent dup or lost seq.
        let last = entities::event::Entity::find()
            .filter(entities::event::Column::ChatId.eq(chat_id.0))
            .order_by_desc(entities::event::Column::Seq)
            .one(&self.conn)
            .await
            .map_err(store_err)?;
        let seq = last.map_or(0, |model| model.seq) + 1;

        entities::event::ActiveModel {
            chat_id: Set(chat_id.0),
            seq: Set(seq),
            payload: Set(serde_json::to_value(event)?),
            created_at: Set(Utc::now()),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(seq)
    }

    async fn list_events(&self, chat_id: ChatId, after: i64) -> Result<Vec<SequencedEvent>> {
        entities::event::Entity::find()
            .filter(entities::event::Column::ChatId.eq(chat_id.0))
            .filter(entities::event::Column::Seq.gt(after))
            .order_by_asc(entities::event::Column::Seq)
            .all(&self.conn)
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
}

fn project_from_model(model: entities::project::Model) -> Project {
    Project {
        id: ProjectId(model.id),
        title: model.title,
        workspace_dir: PathBuf::from(model.workspace_dir),
        created_at: model.created_at,
    }
}

fn document_from_model(model: entities::document::Model) -> Result<DocumentRecord> {
    Ok(DocumentRecord {
        id: DocumentId(model.id),
        project_id: model.project_id.map(ProjectId),
        source_uri: model.source_uri,
        media_type: model.media_type,
        title: model.title,
        canonical_text: model.canonical_text,
        content_revision: model.content_revision,
        revision_token: model.revision_token,
        processing_status: document_processing_status_from_db(&model.processing_status)?,
        indexed_revision: model.indexed_revision,
        index_fingerprint: model.index_fingerprint,
        created_at: model.created_at,
        updated_at: model.updated_at,
        indexed_at: model.indexed_at,
    })
}

fn document_summary_from_row(row: DocumentSummaryRow) -> Result<DocumentSummaryRecord> {
    Ok(DocumentSummaryRecord {
        id: DocumentId(row.id),
        project_id: row.project_id.map(ProjectId),
        source_uri: row.source_uri,
        media_type: row.media_type,
        title: row.title,
        content_revision: row.content_revision,
        processing_status: document_processing_status_from_db(&row.processing_status)?,
        indexed_revision: row.indexed_revision,
        index_fingerprint: row.index_fingerprint,
        created_at: row.created_at,
        updated_at: row.updated_at,
        indexed_at: row.indexed_at,
    })
}

fn document_from_upsert(
    document: &DocumentUpsert,
    created_at: chrono::DateTime<Utc>,
    content_revision: i64,
    revision_token: uuid::Uuid,
) -> DocumentRecord {
    DocumentRecord {
        id: document.id,
        project_id: document.project_id,
        source_uri: document.source_uri.clone(),
        media_type: document.media_type.clone(),
        title: document.title.clone(),
        canonical_text: document.canonical_text.clone(),
        content_revision,
        revision_token,
        processing_status: DocumentProcessingStatus::Queued,
        indexed_revision: None,
        index_fingerprint: None,
        created_at,
        updated_at: document.updated_at,
        indexed_at: None,
    }
}

fn document_processing_status_from_db(text: &str) -> Result<DocumentProcessingStatus> {
    match text {
        "queued" => Ok(DocumentProcessingStatus::Queued),
        "processing" => Ok(DocumentProcessingStatus::Processing),
        "ready" => Ok(DocumentProcessingStatus::Ready),
        "failed" => Ok(DocumentProcessingStatus::Failed),
        other => Err(AgentError::Store(format!(
            "unknown document processing status: {other}"
        ))),
    }
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

/// SeaORM entity models. Kept internal — the public `Store` API speaks the domain
/// types (`Chat`, `Message`), never these, so the ORM never leaks into the
/// crate's contract.
mod entities {
    pub mod document {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "document")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub project_id: Option<Uuid>,
            pub source_uri: Option<String>,
            pub media_type: String,
            pub title: Option<String>,
            #[sea_orm(column_type = "Text")]
            pub canonical_text: String,
            pub content_revision: i64,
            pub revision_token: Uuid,
            pub processing_status: String,
            pub indexed_revision: Option<i64>,
            pub index_fingerprint: Option<String>,
            pub created_at: DateTimeUtc,
            pub updated_at: DateTimeUtc,
            pub indexed_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    #[cfg(test)]
    pub mod document_job {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "document_job")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub document_id: Uuid,
            pub content_revision: i64,
            pub revision_token: Uuid,
            pub kind: String,
            pub status: String,
            pub pipeline_fingerprint: String,
            pub attempt_count: i32,
            pub max_attempts: i32,
            pub available_at: DateTimeUtc,
            pub lease_token: Option<Uuid>,
            pub lease_expires_at: Option<DateTimeUtc>,
            pub started_at: Option<DateTimeUtc>,
            pub finished_at: Option<DateTimeUtc>,
            pub last_error_code: Option<String>,
            pub last_error_detail: Option<String>,
            pub created_at: DateTimeUtc,
            pub updated_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod project {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "project")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub title: Option<String>,
            pub workspace_dir: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod chat {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "chat")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub project_id: Option<Uuid>,
            pub title: Option<String>,
            pub model: Option<String>,
            pub workspace_dir: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod message {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "message")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub chat_id: Uuid,
            pub turn_id: Uuid,
            pub role: String,
            pub content: String,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod tool_call {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "tool_call")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub chat_id: Uuid,
            pub turn_id: Uuid,
            pub provider_id: String,
            pub name: String,
            #[sea_orm(column_type = "JsonBinary")]
            pub arguments: Json,
            pub result: Option<String>,
            pub is_error: bool,
            pub created_at: DateTimeUtc,
            pub completed_at: Option<DateTimeUtc>,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod setting {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "setting")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub key: String,
            // Matches the migration's `.json_binary()` (JSONB on Postgres).
            #[sea_orm(column_type = "JsonBinary")]
            pub value_json: Json,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }

    pub mod event {
        use sea_orm::entity::prelude::*;

        // Composite primary key `(chat_id, seq)`: `seq` is monotonic *per
        // chat*, and the pair both enforces uniqueness and indexes the
        // "this chat's events after a cursor" replay query.
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "event")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub chat_id: Uuid,
            #[sea_orm(primary_key, auto_increment = false)]
            pub seq: i64,
            #[sea_orm(column_type = "JsonBinary")]
            pub payload: Json,
            pub created_at: DateTimeUtc,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}
    }
}

/// Schema v1, defined once via SeaORM's schema builder; it emits dialect-correct
/// DDL for whichever backend is connected.
mod migration {
    use sea_orm_migration::prelude::*;

    use super::{DocumentJobKind, DocumentJobStatus, DocumentProcessingStatus};

    pub struct Migrator;

    #[async_trait::async_trait]
    impl MigratorTrait for Migrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![
                Box::new(Init),
                Box::new(AddEventJournal),
                Box::new(AddProjects),
                Box::new(AddChatModel),
                Box::new(AddToolCalls),
                Box::new(AddDocuments),
            ]
        }
    }

    struct Init;

    impl MigrationName for Init {
        fn name(&self) -> &str {
            "m0001_init"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for Init {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Chat::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Chat::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Chat::Title).text())
                        .col(ColumnDef::new(Chat::WorkspaceDir).text().not_null())
                        .col(
                            ColumnDef::new(Chat::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_table(
                    Table::create()
                        .table(Message::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Message::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Message::ChatId).uuid().not_null())
                        .col(ColumnDef::new(Message::TurnId).uuid().not_null())
                        .col(ColumnDef::new(Message::Role).text().not_null())
                        .col(ColumnDef::new(Message::Content).text().not_null())
                        .col(
                            ColumnDef::new(Message::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_message_chat")
                                .from(Message::Table, Message::ChatId)
                                .to(Chat::Table, Chat::Id),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_message_chat")
                        .table(Message::Table)
                        .col(Message::ChatId)
                        .col(Message::CreatedAt)
                        .to_owned(),
                )
                .await?;

            manager
                .create_table(
                    Table::create()
                        .table(Setting::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Setting::Key).text().not_null().primary_key())
                        .col(ColumnDef::new(Setting::ValueJson).json_binary().not_null())
                        .to_owned(),
                )
                .await?;

            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(Message::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Setting::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Chat::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    /// Adds the per-chat event journal that clients replay from on connect.
    struct AddEventJournal;

    impl MigrationName for AddEventJournal {
        fn name(&self) -> &str {
            "m0002_event_journal"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for AddEventJournal {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Event::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Event::ChatId).uuid().not_null())
                        .col(ColumnDef::new(Event::Seq).big_integer().not_null())
                        .col(ColumnDef::new(Event::Payload).json_binary().not_null())
                        .col(
                            ColumnDef::new(Event::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .primary_key(Index::create().col(Event::ChatId).col(Event::Seq))
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_event_chat")
                                .from(Event::Table, Event::ChatId)
                                .to(Chat::Table, Chat::Id),
                        )
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(Event::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    /// Adds the `project` table and the optional `chat.project_id` link.
    struct AddProjects;

    impl MigrationName for AddProjects {
        fn name(&self) -> &str {
            "m0003_projects"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for AddProjects {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Project::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Project::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Project::Title).text())
                        .col(ColumnDef::new(Project::WorkspaceDir).text().not_null())
                        .col(
                            ColumnDef::new(Project::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .to_owned(),
                )
                .await?;

            // A nullable link, no DB-level foreign key: SQLite can't add an FK to
            // an existing table, so membership is validated at the API edge (the
            // server checks the project exists before creating the chat).
            manager
                .alter_table(
                    Table::alter()
                        .table(Chat::Table)
                        .add_column(ColumnDef::new(Chat::ProjectId).uuid())
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(Chat::Table)
                        .drop_column(Chat::ProjectId)
                        .to_owned(),
                )
                .await?;
            manager
                .drop_table(Table::drop().table(Project::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    /// Adds the optional per-chat `model` override.
    struct AddChatModel;

    impl MigrationName for AddChatModel {
        fn name(&self) -> &str {
            "m0004_chat_model"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for AddChatModel {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(Chat::Table)
                        .add_column(ColumnDef::new(Chat::Model).text())
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .alter_table(
                    Table::alter()
                        .table(Chat::Table)
                        .drop_column(Chat::Model)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }
    }

    /// Structured tool-call rows (args + result), distinct from text messages.
    struct AddToolCalls;

    impl MigrationName for AddToolCalls {
        fn name(&self) -> &str {
            "m0005_tool_calls"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for AddToolCalls {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(ToolCall::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(ToolCall::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(ToolCall::ChatId).uuid().not_null())
                        .col(ColumnDef::new(ToolCall::TurnId).uuid().not_null())
                        .col(ColumnDef::new(ToolCall::ProviderId).text().not_null())
                        .col(ColumnDef::new(ToolCall::Name).text().not_null())
                        .col(ColumnDef::new(ToolCall::Arguments).json_binary().not_null())
                        .col(ColumnDef::new(ToolCall::Result).text())
                        .col(
                            ColumnDef::new(ToolCall::IsError)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .col(
                            ColumnDef::new(ToolCall::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(ColumnDef::new(ToolCall::CompletedAt).timestamp_with_time_zone())
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_tool_call_chat")
                                .from(ToolCall::Table, ToolCall::ChatId)
                                .to(Chat::Table, Chat::Id),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_tool_call_chat")
                        .table(ToolCall::Table)
                        .col(ToolCall::ChatId)
                        .col(ToolCall::CreatedAt)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(ToolCall::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    /// Adds authoritative documents and their durable processing jobs. The
    /// retrieval database remains derived state; lifecycle, retry, and lease
    /// ownership live in the operational database.
    struct AddDocuments;

    impl MigrationName for AddDocuments {
        fn name(&self) -> &str {
            "m0006_document_catalog"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for AddDocuments {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            let valid_index_revision = Expr::col(Document::IndexedRevision).is_null().or(
                Expr::col(Document::IndexedRevision).gte(1).and(
                    Expr::col(Document::IndexedRevision).lte(Expr::col(Document::ContentRevision)),
                ),
            );
            let watermark_absent = Expr::col(Document::IndexedRevision)
                .is_null()
                .and(Expr::col(Document::IndexFingerprint).is_null())
                .and(Expr::col(Document::IndexedAt).is_null());
            let watermark_present = Expr::col(Document::IndexedRevision)
                .is_not_null()
                .and(Expr::col(Document::IndexFingerprint).is_not_null().and(
                    Func::char_length(Expr::col(Document::IndexFingerprint)).between(
                        1,
                        crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN as i32,
                    ),
                ))
                .and(Expr::col(Document::IndexedAt).is_not_null());
            let processing_watermark_consistent = Expr::col(Document::ProcessingStatus)
                .eq(DocumentProcessingStatus::Ready.as_str())
                .and(Expr::col(Document::IndexedRevision).eq(Expr::col(Document::ContentRevision)))
                .and(watermark_present)
                .or(Expr::col(Document::ProcessingStatus)
                    .ne(DocumentProcessingStatus::Ready.as_str())
                    .and(watermark_absent));

            manager
                .create_table(
                    Table::create()
                        .table(Document::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Document::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Document::ProjectId).uuid())
                        .col(ColumnDef::new(Document::SourceUri).text())
                        .col(ColumnDef::new(Document::MediaType).text().not_null())
                        .col(ColumnDef::new(Document::Title).text())
                        .col(ColumnDef::new(Document::CanonicalText).text().not_null())
                        .col(
                            ColumnDef::new(Document::ContentRevision)
                                .big_integer()
                                .not_null()
                                .default(1),
                        )
                        .col(ColumnDef::new(Document::RevisionToken).uuid().not_null())
                        .col(
                            ColumnDef::new(Document::ProcessingStatus)
                                .text()
                                .not_null()
                                .default(DocumentProcessingStatus::Queued.as_str()),
                        )
                        .col(ColumnDef::new(Document::IndexedRevision).big_integer())
                        .col(ColumnDef::new(Document::IndexFingerprint).text())
                        .col(
                            ColumnDef::new(Document::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(Document::UpdatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(ColumnDef::new(Document::IndexedAt).timestamp_with_time_zone())
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_document_project")
                                .from(Document::Table, Document::ProjectId)
                                .to(Project::Table, Project::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .check(Expr::col(Document::MediaType).ne(""))
                        .check(
                            Expr::col(Document::SourceUri)
                                .is_null()
                                .or(Expr::col(Document::SourceUri).ne("")),
                        )
                        .check(Expr::col(Document::ContentRevision).gte(1))
                        .check(Expr::col(Document::ProcessingStatus).is_in([
                            DocumentProcessingStatus::Queued.as_str(),
                            DocumentProcessingStatus::Processing.as_str(),
                            DocumentProcessingStatus::Ready.as_str(),
                            DocumentProcessingStatus::Failed.as_str(),
                        ]))
                        .check(valid_index_revision)
                        .check(processing_watermark_consistent)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_document_project_created")
                        .table(Document::Table)
                        .col(Document::ProjectId)
                        .col(Document::CreatedAt)
                        .to_owned(),
                )
                .await?;

            let valid_job_status = Expr::col(DocumentJob::Status).is_in([
                DocumentJobStatus::Queued.as_str(),
                DocumentJobStatus::Running.as_str(),
                DocumentJobStatus::RetryWait.as_str(),
                DocumentJobStatus::Succeeded.as_str(),
                DocumentJobStatus::Failed.as_str(),
                DocumentJobStatus::Cancelled.as_str(),
            ]);
            let running_lease = Expr::col(DocumentJob::Status)
                .eq(DocumentJobStatus::Running.as_str())
                .and(Expr::col(DocumentJob::LeaseToken).is_not_null())
                .and(Expr::col(DocumentJob::LeaseExpiresAt).is_not_null());
            let no_lease = Expr::col(DocumentJob::Status)
                .ne(DocumentJobStatus::Running.as_str())
                .and(Expr::col(DocumentJob::LeaseToken).is_null())
                .and(Expr::col(DocumentJob::LeaseExpiresAt).is_null());
            let terminal_finished = Expr::col(DocumentJob::Status)
                .is_in([
                    DocumentJobStatus::Succeeded.as_str(),
                    DocumentJobStatus::Failed.as_str(),
                    DocumentJobStatus::Cancelled.as_str(),
                ])
                .and(Expr::col(DocumentJob::FinishedAt).is_not_null());
            let nonterminal_unfinished = Expr::col(DocumentJob::Status)
                .is_in([
                    DocumentJobStatus::Queued.as_str(),
                    DocumentJobStatus::Running.as_str(),
                    DocumentJobStatus::RetryWait.as_str(),
                ])
                .and(Expr::col(DocumentJob::FinishedAt).is_null());
            let queued_attempt = Expr::col(DocumentJob::Status)
                .eq(DocumentJobStatus::Queued.as_str())
                .and(Expr::col(DocumentJob::AttemptCount).eq(0))
                .and(Expr::col(DocumentJob::StartedAt).is_null());
            let running_attempt = Expr::col(DocumentJob::Status)
                .eq(DocumentJobStatus::Running.as_str())
                .and(Expr::col(DocumentJob::AttemptCount).gte(1))
                .and(Expr::col(DocumentJob::StartedAt).is_not_null());
            let retryable_attempt = Expr::col(DocumentJob::Status)
                .eq(DocumentJobStatus::RetryWait.as_str())
                .and(Expr::col(DocumentJob::AttemptCount).gte(1))
                .and(Expr::col(DocumentJob::AttemptCount).lt(Expr::col(DocumentJob::MaxAttempts)))
                .and(Expr::col(DocumentJob::StartedAt).is_not_null());
            let completed_attempt = Expr::col(DocumentJob::Status)
                .is_in([
                    DocumentJobStatus::Succeeded.as_str(),
                    DocumentJobStatus::Failed.as_str(),
                ])
                .and(Expr::col(DocumentJob::AttemptCount).gte(1))
                .and(Expr::col(DocumentJob::StartedAt).is_not_null());
            let cancelled_attempt = Expr::col(DocumentJob::Status)
                .eq(DocumentJobStatus::Cancelled.as_str())
                .and(
                    Expr::col(DocumentJob::AttemptCount)
                        .eq(0)
                        .and(Expr::col(DocumentJob::StartedAt).is_null())
                        .or(Expr::col(DocumentJob::AttemptCount)
                            .gte(1)
                            .and(Expr::col(DocumentJob::StartedAt).is_not_null())),
                );

            manager
                .create_table(
                    Table::create()
                        .table(DocumentJob::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(DocumentJob::Id)
                                .uuid()
                                .not_null()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(DocumentJob::DocumentId).uuid().not_null())
                        .col(
                            ColumnDef::new(DocumentJob::ContentRevision)
                                .big_integer()
                                .not_null(),
                        )
                        .col(ColumnDef::new(DocumentJob::RevisionToken).uuid().not_null())
                        .col(ColumnDef::new(DocumentJob::Kind).string_len(64).not_null())
                        .col(
                            ColumnDef::new(DocumentJob::Status)
                                .string_len(32)
                                .not_null()
                                .default(DocumentJobStatus::Queued.as_str()),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::PipelineFingerprint)
                                .string_len(512)
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::AttemptCount)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::MaxAttempts)
                                .integer()
                                .not_null()
                                .default(5),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::AvailableAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(ColumnDef::new(DocumentJob::LeaseToken).uuid())
                        .col(ColumnDef::new(DocumentJob::LeaseExpiresAt).timestamp_with_time_zone())
                        .col(ColumnDef::new(DocumentJob::StartedAt).timestamp_with_time_zone())
                        .col(ColumnDef::new(DocumentJob::FinishedAt).timestamp_with_time_zone())
                        .col(ColumnDef::new(DocumentJob::LastErrorCode).string_len(128))
                        .col(ColumnDef::new(DocumentJob::LastErrorDetail).string_len(4096))
                        .col(
                            ColumnDef::new(DocumentJob::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(DocumentJob::UpdatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_document_job_document")
                                .from(DocumentJob::Table, DocumentJob::DocumentId)
                                .to(Document::Table, Document::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .check(Expr::col(DocumentJob::ContentRevision).gte(1))
                        .check(
                            Expr::col(DocumentJob::Kind).is_in([DocumentJobKind::Index.as_str()]),
                        )
                        .check(
                            Func::char_length(Expr::col(DocumentJob::Kind))
                                .lte(64)
                                .and(
                                    Func::char_length(Expr::col(DocumentJob::PipelineFingerprint))
                                        .between(
                                            1,
                                            crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN
                                                as i32,
                                        ),
                                )
                                .and(
                                    Expr::col(DocumentJob::LastErrorCode).is_null().or(
                                        Func::char_length(Expr::col(DocumentJob::LastErrorCode))
                                            .between(1, 128),
                                    ),
                                )
                                .and(
                                    Expr::col(DocumentJob::LastErrorDetail).is_null().or(
                                        Func::char_length(Expr::col(DocumentJob::LastErrorDetail))
                                            .between(1, 4096),
                                    ),
                                ),
                        )
                        .check(valid_job_status)
                        .check(
                            Expr::col(DocumentJob::AttemptCount)
                                .gte(0)
                                .and(Expr::col(DocumentJob::MaxAttempts).gte(1))
                                .and(
                                    Expr::col(DocumentJob::AttemptCount)
                                        .lte(Expr::col(DocumentJob::MaxAttempts)),
                                ),
                        )
                        .check(running_lease.or(no_lease))
                        .check(terminal_finished.or(nonterminal_unfinished))
                        .check(
                            queued_attempt
                                .or(running_attempt)
                                .or(retryable_attempt)
                                .or(completed_attempt)
                                .or(cancelled_attempt),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_document_job_idempotency")
                        .table(DocumentJob::Table)
                        .col(DocumentJob::DocumentId)
                        .col(DocumentJob::RevisionToken)
                        .col(DocumentJob::Kind)
                        .col(DocumentJob::PipelineFingerprint)
                        .unique()
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_document_job_one_active")
                        .table(DocumentJob::Table)
                        .col(DocumentJob::DocumentId)
                        .unique()
                        .and_where(Expr::col(DocumentJob::Status).is_in([
                            DocumentJobStatus::Queued.as_str(),
                            DocumentJobStatus::Running.as_str(),
                            DocumentJobStatus::RetryWait.as_str(),
                        ]))
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_document_job_due")
                        .table(DocumentJob::Table)
                        .col(DocumentJob::Status)
                        .col(DocumentJob::AvailableAt)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_document_job_stale_lease")
                        .table(DocumentJob::Table)
                        .col(DocumentJob::Status)
                        .col(DocumentJob::LeaseExpiresAt)
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .name("idx_document_job_history")
                        .table(DocumentJob::Table)
                        .col(DocumentJob::DocumentId)
                        .col(DocumentJob::CreatedAt)
                        .to_owned(),
                )
                .await?;
            Ok(())
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(DocumentJob::Table).to_owned())
                .await?;
            manager
                .drop_table(Table::drop().table(Document::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    #[derive(DeriveIden)]
    enum Project {
        Table,
        Id,
        Title,
        WorkspaceDir,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum Document {
        Table,
        Id,
        ProjectId,
        SourceUri,
        MediaType,
        Title,
        CanonicalText,
        ContentRevision,
        RevisionToken,
        ProcessingStatus,
        IndexedRevision,
        IndexFingerprint,
        CreatedAt,
        UpdatedAt,
        IndexedAt,
    }

    #[derive(DeriveIden)]
    enum DocumentJob {
        Table,
        Id,
        DocumentId,
        ContentRevision,
        RevisionToken,
        Kind,
        Status,
        PipelineFingerprint,
        AttemptCount,
        MaxAttempts,
        AvailableAt,
        LeaseToken,
        LeaseExpiresAt,
        StartedAt,
        FinishedAt,
        LastErrorCode,
        LastErrorDetail,
        CreatedAt,
        UpdatedAt,
    }

    #[derive(DeriveIden)]
    enum Chat {
        Table,
        Id,
        ProjectId,
        Title,
        Model,
        WorkspaceDir,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum Message {
        Table,
        Id,
        ChatId,
        TurnId,
        Role,
        Content,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum ToolCall {
        Table,
        Id,
        ChatId,
        TurnId,
        ProviderId,
        Name,
        Arguments,
        Result,
        IsError,
        CreatedAt,
        CompletedAt,
    }

    #[derive(DeriveIden)]
    enum Setting {
        Table,
        Key,
        ValueJson,
    }

    #[derive(DeriveIden)]
    enum Event {
        Table,
        ChatId,
        Seq,
        Payload,
        CreatedAt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DocumentJobKind;
    use chrono::{DateTime, Utc};

    async fn temp_store() -> (tempfile::TempDir, DbStore) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let store = DbStore::connect(&url).await.unwrap();
        (dir, store)
    }

    fn sample_chat() -> Chat {
        Chat {
            id: ChatId::new(),
            project_id: None,
            title: Some("hello".into()),
            model: None,
            workspace_dir: PathBuf::from("/tmp/ws"),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn sample_project() -> Project {
        Project {
            id: ProjectId::new(),
            title: Some("proj".into()),
            workspace_dir: PathBuf::from("/tmp/proj"),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    fn sample_document(project_id: Option<ProjectId>) -> DocumentRecord {
        let created_at = DateTime::<Utc>::from_timestamp(1_700_000_100, 0).unwrap();
        DocumentRecord {
            id: DocumentId::new(),
            project_id,
            source_uri: Some("file:///資料/notes.md".into()),
            media_type: "text/markdown".into(),
            title: Some("Résumé 📈".into()),
            canonical_text: "# Résumé\n\n売上 grew by 10%.".into(),
            content_revision: 1,
            revision_token: uuid::Uuid::new_v4(),
            processing_status: DocumentProcessingStatus::Queued,
            indexed_revision: None,
            index_fingerprint: None,
            created_at,
            updated_at: created_at,
            indexed_at: None,
        }
    }

    #[tokio::test]
    async fn projects_roundtrip_and_a_chat_can_belong_to_one() {
        let (_dir, store) = temp_store().await;
        let project = sample_project();
        store.create_project(&project).await.unwrap();

        assert_eq!(
            store.get_project(project.id).await.unwrap().as_ref(),
            Some(&project)
        );
        assert_eq!(store.list_projects().await.unwrap(), vec![project.clone()]);
        assert_eq!(store.get_project(ProjectId::new()).await.unwrap(), None);

        // A chat carrying the project link round-trips it; a loose chat stays None.
        let mut in_project = sample_chat();
        in_project.project_id = Some(project.id);
        store.create_chat(&in_project).await.unwrap();
        assert_eq!(
            store
                .get_chat(in_project.id)
                .await
                .unwrap()
                .unwrap()
                .project_id,
            Some(project.id)
        );

        let loose = sample_chat();
        store.create_chat(&loose).await.unwrap();
        assert_eq!(
            store.get_chat(loose.id).await.unwrap().unwrap().project_id,
            None
        );

        // The project link survives a list, not just a by-id fetch.
        let listed = store.list_chats().await.unwrap();
        let listed_link = |id| {
            listed
                .iter()
                .find(|c| c.id == id)
                .and_then(|c| c.project_id)
        };
        assert_eq!(listed_link(in_project.id), Some(project.id));
        assert_eq!(listed_link(loose.id), None);
    }

    #[tokio::test]
    async fn set_chat_model_updates_then_clears() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);

        store
            .set_chat_model(chat.id, Some("claude-x".into()))
            .await
            .unwrap();
        assert_eq!(
            store
                .get_chat(chat.id)
                .await
                .unwrap()
                .unwrap()
                .model
                .as_deref(),
            Some("claude-x")
        );

        store.set_chat_model(chat.id, None).await.unwrap();
        assert_eq!(store.get_chat(chat.id).await.unwrap().unwrap().model, None);
    }

    #[tokio::test]
    async fn list_projects_is_newest_first() {
        let (_dir, store) = temp_store().await;
        let mut older = sample_project();
        older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut newer = sample_project();
        newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        store.create_project(&older).await.unwrap();
        store.create_project(&newer).await.unwrap();
        assert_eq!(store.list_projects().await.unwrap(), vec![newer, older]);
    }

    #[tokio::test]
    async fn documents_roundtrip_and_list_by_corpus_scope() {
        let (_dir, store) = temp_store().await;
        let project_a = sample_project();
        let mut project_b = sample_project();
        project_b.id = ProjectId::new();
        store.create_project(&project_a).await.unwrap();
        store.create_project(&project_b).await.unwrap();

        let mut unscoped = sample_document(None);
        unscoped.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut in_a = sample_document(Some(project_a.id));
        in_a.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        in_a.processing_status = DocumentProcessingStatus::Ready;
        in_a.indexed_revision = Some(1);
        in_a.index_fingerprint = Some("parser=v1;chunker=v1;embed=test".into());
        in_a.indexed_at = Some(DateTime::<Utc>::from_timestamp(2_001, 0).unwrap());
        let mut in_b = sample_document(Some(project_b.id));
        in_b.created_at = DateTime::<Utc>::from_timestamp(3_000, 0).unwrap();

        for document in [&unscoped, &in_a, &in_b] {
            store.create_document(document).await.unwrap();
        }
        unscoped = store.get_document(unscoped.id).await.unwrap().unwrap();
        in_a = store.get_document(in_a.id).await.unwrap().unwrap();
        in_b = store.get_document(in_b.id).await.unwrap().unwrap();

        assert_eq!(
            store.get_document(in_a.id).await.unwrap().as_ref(),
            Some(&in_a)
        );
        assert_eq!(store.get_document(DocumentId::new()).await.unwrap(), None);
        assert_eq!(
            store
                .list_documents(DocumentScope::Project(project_a.id))
                .await
                .unwrap(),
            vec![in_a.clone()]
        );
        assert_eq!(
            store
                .list_documents(DocumentScope::Project(project_b.id))
                .await
                .unwrap(),
            vec![in_b.clone()]
        );
        assert_eq!(
            store.list_documents(DocumentScope::Unscoped).await.unwrap(),
            vec![unscoped.clone()]
        );
        assert_eq!(
            store.list_documents(DocumentScope::All).await.unwrap(),
            vec![in_b, in_a, unscoped.clone()]
        );

        store.delete_document(unscoped.id).await.unwrap();
        store.delete_document(unscoped.id).await.unwrap();
        assert_eq!(store.get_document(unscoped.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn document_summaries_page_by_created_at_then_id_without_gaps() {
        let (_dir, store) = temp_store().await;
        // Keep both groups inside one microsecond so cursor implementations
        // that truncate sub-microsecond precision would skip the older group.
        let newer = DateTime::<Utc>::from_timestamp(2_000, 900).unwrap();
        let older = DateTime::<Utc>::from_timestamp(2_000, 700).unwrap();
        let fixtures = [
            (3_u128, newer, "newest tie"),
            (2, newer, "middle tie"),
            (1, newer, "last tie"),
            (5, older, "older high id"),
            (4, older, "older low id"),
        ];
        for (raw_id, created_at, title) in fixtures {
            let mut document = sample_document(None);
            document.id = DocumentId(uuid::Uuid::from_u128(raw_id));
            document.title = Some(title.into());
            document.canonical_text = format!("content that listings must not load: {title}");
            document.created_at = created_at;
            document.updated_at = created_at;
            store.create_document(&document).await.unwrap();
        }

        let first = store
            .list_document_summaries(DocumentScope::All, None, 2)
            .await
            .unwrap();
        assert_eq!(
            first
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(3), uuid::Uuid::from_u128(2)]
        );
        let second = store
            .list_document_summaries(
                DocumentScope::All,
                Some(DocumentListCursor {
                    created_at: first[1].created_at,
                    id: first[1].id,
                }),
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            second
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(5)]
        );
        let third = store
            .list_document_summaries(
                DocumentScope::All,
                Some(DocumentListCursor {
                    created_at: second[1].created_at,
                    id: second[1].id,
                }),
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            third
                .iter()
                .map(|document| document.id.0)
                .collect::<Vec<_>>(),
            vec![uuid::Uuid::from_u128(4)]
        );
        assert!(store
            .list_document_summaries(DocumentScope::All, None, 0)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn document_project_fk_rejects_orphans_and_cascades_delete() {
        let (_dir, store) = temp_store().await;
        let orphan = sample_document(Some(ProjectId::new()));
        assert!(store.create_document(&orphan).await.is_err());

        let project = sample_project();
        store.create_project(&project).await.unwrap();
        let document = sample_document(Some(project.id));
        store.create_document(&document).await.unwrap();
        entities::project::Entity::delete_by_id(project.id.0)
            .exec(&store.conn)
            .await
            .unwrap();
        assert_eq!(store.get_document(document.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn document_constraints_reject_invalid_catalog_state() {
        let (_dir, store) = temp_store().await;

        let mut empty_media_type = sample_document(None);
        empty_media_type.media_type.clear();
        assert!(store.create_document(&empty_media_type).await.is_err());

        let mut empty_source_uri = sample_document(None);
        empty_source_uri.source_uri = Some(String::new());
        assert!(store.create_document(&empty_source_uri).await.is_err());

        let mut invalid_revision = sample_document(None);
        invalid_revision.content_revision = 0;
        assert!(store.create_document(&invalid_revision).await.is_err());

        let mut future_index = sample_document(None);
        future_index.indexed_revision = Some(2);
        future_index.index_fingerprint = Some("v1".into());
        future_index.indexed_at = Some(Utc::now());
        assert!(store.create_document(&future_index).await.is_err());

        let mut partial_watermark = sample_document(None);
        partial_watermark.indexed_revision = Some(1);
        assert!(store.create_document(&partial_watermark).await.is_err());

        let mut empty_fingerprint = sample_document(None);
        empty_fingerprint.processing_status = DocumentProcessingStatus::Ready;
        empty_fingerprint.indexed_revision = Some(1);
        empty_fingerprint.index_fingerprint = Some(String::new());
        empty_fingerprint.indexed_at = Some(Utc::now());
        assert!(store.create_document(&empty_fingerprint).await.is_err());

        let mut oversized_fingerprint = sample_document(None);
        oversized_fingerprint.processing_status = DocumentProcessingStatus::Ready;
        oversized_fingerprint.indexed_revision = Some(1);
        oversized_fingerprint.index_fingerprint =
            Some("x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1));
        oversized_fingerprint.indexed_at = Some(Utc::now());
        assert!(store.create_document(&oversized_fingerprint).await.is_err());
    }

    #[tokio::test]
    async fn document_job_schema_enforces_delivery_and_idempotency_invariants() {
        let (_dir, store) = temp_store().await;
        let document = sample_document(None);
        store.create_document(&document).await.unwrap();
        let document = store.get_document(document.id).await.unwrap().unwrap();
        let now = DateTime::<Utc>::from_timestamp(1_752_148_800, 0).unwrap();
        let make_job =
            |document: &DocumentRecord, fingerprint: &str| entities::document_job::ActiveModel {
                id: Set(uuid::Uuid::new_v4()),
                document_id: Set(document.id.0),
                content_revision: Set(document.content_revision),
                revision_token: Set(document.revision_token),
                kind: Set(DocumentJobKind::Index.as_str().into()),
                status: Set(DocumentJobStatus::Queued.as_str().into()),
                pipeline_fingerprint: Set(fingerprint.into()),
                attempt_count: Set(0),
                max_attempts: Set(5),
                available_at: Set(now),
                lease_token: Set(None),
                lease_expires_at: Set(None),
                started_at: Set(None),
                finished_at: Set(None),
                last_error_code: Set(None),
                last_error_detail: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            };
        let first = make_job(&document, "pipeline-v1")
            .insert(&store.conn)
            .await
            .unwrap();

        // A document has only one nonterminal pipeline stage at a time.
        assert!(make_job(&document, "pipeline-v2")
            .insert(&store.conn)
            .await
            .is_err());

        // State-dependent attempt, lease, and timestamp rules are independent.
        let another_document = sample_document(None);
        store.create_document(&another_document).await.unwrap();
        let another_document = store
            .get_document(another_document.id)
            .await
            .unwrap()
            .unwrap();
        let mut running_without_lease = make_job(&another_document, "pipeline-v1");
        running_without_lease.status = Set(DocumentJobStatus::Running.as_str().into());
        running_without_lease.attempt_count = Set(1);
        running_without_lease.started_at = Set(Some(now));
        assert!(running_without_lease.insert(&store.conn).await.is_err());
        let mut running_without_attempt = make_job(&another_document, "pipeline-v1");
        running_without_attempt.status = Set(DocumentJobStatus::Running.as_str().into());
        running_without_attempt.lease_token = Set(Some(uuid::Uuid::new_v4()));
        running_without_attempt.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
        assert!(running_without_attempt.insert(&store.conn).await.is_err());
        let mut exhausted_retry = make_job(&another_document, "pipeline-v1");
        exhausted_retry.status = Set(DocumentJobStatus::RetryWait.as_str().into());
        exhausted_retry.attempt_count = Set(5);
        exhausted_retry.started_at = Set(Some(now));
        assert!(exhausted_retry.insert(&store.conn).await.is_err());
        let mut terminal_without_finish = make_job(&another_document, "pipeline-v1");
        terminal_without_finish.status = Set(DocumentJobStatus::Failed.as_str().into());
        terminal_without_finish.attempt_count = Set(5);
        terminal_without_finish.started_at = Set(Some(now));
        assert!(terminal_without_finish.insert(&store.conn).await.is_err());
        let mut terminal_without_attempt = make_job(&another_document, "pipeline-v1");
        terminal_without_attempt.status = Set(DocumentJobStatus::Succeeded.as_str().into());
        terminal_without_attempt.finished_at = Set(Some(now));
        assert!(terminal_without_attempt.insert(&store.conn).await.is_err());

        let mut unknown_kind = make_job(&another_document, "pipeline-v1");
        unknown_kind.kind = Set("unknown".into());
        assert!(unknown_kind.insert(&store.conn).await.is_err());
        assert!(make_job(&another_document, "")
            .insert(&store.conn)
            .await
            .is_err());
        assert!(make_job(&another_document, &"x".repeat(513))
            .insert(&store.conn)
            .await
            .is_err());
        let mut oversized_error = make_job(&another_document, "pipeline-v1");
        oversized_error.last_error_code = Set(Some("e".repeat(129)));
        assert!(oversized_error.insert(&store.conn).await.is_err());
        let mut empty_error = make_job(&another_document, "pipeline-v1");
        empty_error.last_error_code = Set(Some(String::new()));
        assert!(empty_error.insert(&store.conn).await.is_err());
        let mut empty_detail = make_job(&another_document, "pipeline-v1");
        empty_detail.last_error_detail = Set(Some(String::new()));
        assert!(empty_detail.insert(&store.conn).await.is_err());
        let mut oversized_detail = make_job(&another_document, "pipeline-v1");
        oversized_detail.last_error_detail = Set(Some("d".repeat(4097)));
        assert!(oversized_detail.insert(&store.conn).await.is_err());

        let valid_running_document = sample_document(None);
        store
            .create_document(&valid_running_document)
            .await
            .unwrap();
        let valid_running_document = store
            .get_document(valid_running_document.id)
            .await
            .unwrap()
            .unwrap();
        let mut valid_running = make_job(&valid_running_document, "pipeline-v1");
        valid_running.status = Set(DocumentJobStatus::Running.as_str().into());
        valid_running.attempt_count = Set(1);
        valid_running.started_at = Set(Some(now));
        valid_running.lease_token = Set(Some(uuid::Uuid::new_v4()));
        valid_running.lease_expires_at = Set(Some(now + chrono::Duration::minutes(5)));
        valid_running.insert(&store.conn).await.unwrap();

        entities::document_job::Entity::update_many()
            .col_expr(
                entities::document_job::Column::Status,
                sea_orm::sea_query::Expr::value(DocumentJobStatus::Succeeded.as_str()),
            )
            .col_expr(
                entities::document_job::Column::FinishedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .col_expr(
                entities::document_job::Column::AttemptCount,
                sea_orm::sea_query::Expr::value(1),
            )
            .col_expr(
                entities::document_job::Column::StartedAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(entities::document_job::Column::Id.eq(first.id))
            .exec(&store.conn)
            .await
            .unwrap();

        // Terminal history frees the active slot, but the same semantic job is
        // still deduplicated by exact revision, kind, and pipeline fingerprint.
        assert!(make_job(&document, "pipeline-v1")
            .insert(&store.conn)
            .await
            .is_err());
        make_job(&document, "pipeline-v2")
            .insert(&store.conn)
            .await
            .unwrap();

        store.delete_document(document.id).await.unwrap();
        let remaining = entities::document_job::Entity::find()
            .filter(entities::document_job::Column::DocumentId.eq(document.id.0))
            .all(&store.conn)
            .await
            .unwrap();
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn document_upsert_revisions_and_index_watermark_are_compare_and_set() {
        let (_dir, store) = temp_store().await;
        let id = DocumentId::derive("file:///report.txt");
        let first_at = DateTime::<Utc>::from_timestamp(10_000, 0).unwrap();
        let first = DocumentUpsert {
            id,
            project_id: None,
            source_uri: Some("file:///report.txt".into()),
            media_type: "text/plain".into(),
            title: Some("Report".into()),
            canonical_text: "first version".into(),
            updated_at: first_at,
        };

        let revision_one = store.upsert_document(&first).await.unwrap();
        assert_eq!(revision_one.content_revision, 1);
        assert_eq!(revision_one.created_at, first_at);
        assert_eq!(revision_one.indexed_revision, None);
        assert_eq!(
            revision_one.processing_status,
            DocumentProcessingStatus::Queued
        );
        assert!(store
            .mark_document_indexed(id, 1, revision_one.revision_token, "", first_at)
            .await
            .is_err());
        assert!(store
            .mark_document_indexed(
                id,
                1,
                revision_one.revision_token,
                &"x".repeat(crate::model::DocumentJob::MAX_PIPELINE_FINGERPRINT_LEN + 1),
                first_at,
            )
            .await
            .is_err());
        assert!(store
            .mark_document_indexed(id, 1, revision_one.revision_token, "index-v1", first_at,)
            .await
            .unwrap());

        let second_at = DateTime::<Utc>::from_timestamp(20_000, 0).unwrap();
        let second = DocumentUpsert {
            canonical_text: "second version".into(),
            updated_at: second_at,
            ..first
        };
        let revision_two = store.upsert_document(&second).await.unwrap();
        assert_eq!(revision_two.content_revision, 2);
        assert_eq!(revision_two.created_at, first_at);
        assert_eq!(revision_two.updated_at, second_at);
        assert_ne!(revision_two.revision_token, revision_one.revision_token);
        assert_eq!(revision_two.indexed_revision, None);
        assert_eq!(
            revision_two.processing_status,
            DocumentProcessingStatus::Queued
        );
        assert_eq!(revision_two.index_fingerprint, None);
        assert_eq!(revision_two.indexed_at, None);

        // A late indexer for revision one cannot mark revision two current.
        assert!(!store
            .mark_document_indexed(id, 1, revision_one.revision_token, "stale", second_at)
            .await
            .unwrap());
        assert!(store
            .mark_document_indexed(id, 2, revision_two.revision_token, "index-v2", second_at,)
            .await
            .unwrap());
        let indexed = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(indexed.indexed_revision, Some(2));
        assert_eq!(indexed.processing_status, DocumentProcessingStatus::Ready);
        assert_eq!(indexed.index_fingerprint.as_deref(), Some("index-v2"));
        assert_eq!(indexed.indexed_at, Some(second_at));
        assert!(!store
            .clear_document_index(id, 2, revision_one.revision_token)
            .await
            .unwrap());
        assert!(store
            .clear_document_index(id, 2, revision_two.revision_token)
            .await
            .unwrap());
        let cleared = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(cleared.indexed_revision, None);
        assert_eq!(cleared.processing_status, DocumentProcessingStatus::Queued);
        assert_eq!(cleared.index_fingerprint, None);
        assert_eq!(cleared.indexed_at, None);

        assert!(!store
            .mark_document_indexed(
                DocumentId::new(),
                1,
                uuid::Uuid::new_v4(),
                "missing",
                second_at,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn stale_revision_token_cannot_mark_a_recreated_document_indexed() {
        let (_dir, store) = temp_store().await;
        let first = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "old lifecycle".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let old = store.upsert_document(&first).await.unwrap();
        store.delete_document(first.id).await.unwrap();
        let recreated_at = DateTime::<Utc>::from_timestamp(2, 0).unwrap();
        store
            .create_document(&DocumentRecord {
                canonical_text: "new lifecycle".into(),
                created_at: recreated_at,
                updated_at: recreated_at,
                ..old.clone()
            })
            .await
            .unwrap();
        let recreated = store.get_document(first.id).await.unwrap().unwrap();

        assert_eq!(recreated.content_revision, 1);
        assert_ne!(recreated.revision_token, old.revision_token);
        assert!(!store
            .mark_document_indexed(
                recreated.id,
                old.content_revision,
                old.revision_token,
                "stale",
                recreated.updated_at,
            )
            .await
            .unwrap());
        assert!(store
            .mark_document_indexed(
                recreated.id,
                recreated.content_revision,
                recreated.revision_token,
                "current",
                recreated.updated_at,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn concurrent_first_document_upserts_allocate_distinct_revisions() {
        let (_dir, store) = temp_store().await;
        let first = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "a".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let second = DocumentUpsert {
            canonical_text: "b".into(),
            ..first.clone()
        };

        let (first, second) = tokio::join!(
            store.upsert_document(&first),
            store.upsert_document(&second)
        );
        let mut revisions = [
            first.unwrap().content_revision,
            second.unwrap().content_revision,
        ];
        revisions.sort_unstable();
        assert_eq!(revisions, [1, 2]);
    }

    #[tokio::test]
    async fn document_upsert_rolls_back_when_project_is_unknown() {
        let (_dir, store) = temp_store().await;
        let upsert = DocumentUpsert {
            id: DocumentId::new(),
            project_id: Some(ProjectId::new()),
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "content".into(),
            updated_at: Utc::now(),
        };
        assert!(store.upsert_document(&upsert).await.is_err());
        assert_eq!(store.get_document(upsert.id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn concurrent_document_upserts_allocate_distinct_revisions() {
        let (_dir, store) = temp_store().await;
        let base = DocumentUpsert {
            id: DocumentId::new(),
            project_id: None,
            source_uri: None,
            media_type: "text/plain".into(),
            title: None,
            canonical_text: "base".into(),
            updated_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
        };
        let id = base.id;
        assert_eq!(
            store.upsert_document(&base).await.unwrap().content_revision,
            1
        );
        let a = DocumentUpsert {
            canonical_text: "a".into(),
            updated_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
            ..base.clone()
        };
        let b = DocumentUpsert {
            canonical_text: "b".into(),
            updated_at: DateTime::<Utc>::from_timestamp(3, 0).unwrap(),
            ..base
        };

        let (a, b) = tokio::join!(store.upsert_document(&a), store.upsert_document(&b));
        let mut revisions = [a.unwrap().content_revision, b.unwrap().content_revision];
        revisions.sort_unstable();
        assert_eq!(revisions, [2, 3]);
        let current = store.get_document(id).await.unwrap().unwrap();
        assert_eq!(current.content_revision, 3);
        assert!(matches!(current.canonical_text.as_str(), "a" | "b"));
        assert_eq!(current.indexed_revision, None);
    }

    #[tokio::test]
    async fn high_contention_document_upserts_do_not_drop_writers() {
        let (_dir, store) = temp_store().await;
        let id = DocumentId::new();
        let writes = (0..64).map(|i| {
            let store = store.clone();
            async move {
                store
                    .upsert_document(&DocumentUpsert {
                        id,
                        project_id: None,
                        source_uri: None,
                        media_type: "text/plain".into(),
                        title: None,
                        canonical_text: format!("writer {i}"),
                        updated_at: DateTime::<Utc>::from_timestamp(i, 0).unwrap(),
                    })
                    .await
                    .unwrap()
                    .content_revision
            }
        });

        let mut revisions = futures::future::join_all(writes).await;
        revisions.sort_unstable();
        assert_eq!(revisions, (1..=64).collect::<Vec<_>>());
        assert_eq!(
            store
                .get_document(id)
                .await
                .unwrap()
                .unwrap()
                .content_revision,
            64
        );
    }

    #[tokio::test]
    async fn m0006_upgrades_an_existing_store_without_losing_records() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("upgrade.db").display()
        );
        let conn = Database::connect(&url).await.unwrap();
        conn.execute_unprepared("PRAGMA foreign_keys=ON;")
            .await
            .unwrap();
        migration::Migrator::up(&conn, Some(5)).await.unwrap();
        let store = DbStore { conn: conn.clone() };
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        migration::Migrator::up(&conn, None).await.unwrap();

        assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
        let mut document = sample_document(None);
        let supplied_token = document.revision_token;
        store.create_document(&document).await.unwrap();
        let stored = store.get_document(document.id).await.unwrap().unwrap();
        assert_ne!(stored.revision_token, supplied_token);
        document.revision_token = stored.revision_token;
        assert_eq!(stored, document);
    }

    #[tokio::test]
    async fn chats_and_messages_roundtrip() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        assert_eq!(store.get_chat(chat.id).await.unwrap().as_ref(), Some(&chat));
        assert_eq!(store.list_chats().await.unwrap(), vec![chat.clone()]);
        assert_eq!(store.get_chat(ChatId::new()).await.unwrap(), None);

        let msg = Message {
            id: MessageId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: "hi there".into(),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
        };
        store.append_message(&msg).await.unwrap();
        assert_eq!(store.list_messages(chat.id).await.unwrap(), vec![msg]);
    }

    #[tokio::test]
    async fn settings_roundtrip_and_overwrite() {
        let (_dir, store) = temp_store().await;
        assert_eq!(store.get_setting("model").await.unwrap(), None);
        store
            .set_setting("model", &serde_json::json!("claude"))
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("model").await.unwrap(),
            Some(serde_json::json!("claude"))
        );
        store
            .set_setting("model", &serde_json::json!("gpt"))
            .await
            .unwrap();
        assert_eq!(
            store.get_setting("model").await.unwrap(),
            Some(serde_json::json!("gpt"))
        );
    }

    #[tokio::test]
    async fn list_chats_is_newest_first_and_messages_oldest_first() {
        let (_dir, store) = temp_store().await;
        let mut older = sample_chat();
        older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut newer = sample_chat();
        newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        store.create_chat(&older).await.unwrap();
        store.create_chat(&newer).await.unwrap();
        // list_chats is newest-first.
        assert_eq!(
            store.list_chats().await.unwrap(),
            vec![newer.clone(), older.clone()]
        );

        // Messages come back oldest-first regardless of insert order.
        let msg = |ts: i64| Message {
            id: MessageId::new(),
            chat_id: newer.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: format!("t{ts}"),
            created_at: DateTime::<Utc>::from_timestamp(ts, 0).unwrap(),
        };
        let (m1, m2) = (msg(20), msg(10));
        store.append_message(&m1).await.unwrap();
        store.append_message(&m2).await.unwrap();
        let listed = store.list_messages(newer.id).await.unwrap();
        assert_eq!(listed, vec![m2, m1]);
    }

    #[tokio::test]
    async fn event_journal_assigns_per_chat_seq_and_replays_after_cursor() {
        use crate::event::AgentEvent;
        use crate::id::TurnId;
        use crate::provider::{StopReason, Usage};

        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        let started = AgentEvent::TurnStarted {
            turn_id: TurnId::new(),
        };
        let completed = AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        };
        assert_eq!(store.append_event(chat.id, &started).await.unwrap(), 1);
        assert_eq!(store.append_event(chat.id, &completed).await.unwrap(), 2);

        // From the start: both events, in order, with their seq.
        let all = store.list_events(chat.id, 0).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].seq, all[1].seq), (1, 2));
        assert_eq!(all[0].event, started);

        // After a cursor: only the newer event (what a reconnecting client needs).
        let tail = store.list_events(chat.id, 1).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 2);
        assert_eq!(tail[0].event, completed);

        // A second chat's seq restarts at 1 and its journal is isolated.
        let other = sample_chat();
        store.create_chat(&other).await.unwrap();
        assert_eq!(store.append_event(other.id, &started).await.unwrap(), 1);
        assert_eq!(store.list_events(chat.id, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn event_for_unknown_chat_is_rejected() {
        use crate::event::AgentEvent;

        let (_dir, store) = temp_store().await;
        // No create_chat first: the `event -> chat` foreign key must reject
        // the orphan write. (The in-memory MemStore test double does *not* model
        // this constraint, so orphan-rejection is only guaranteed by DbStore.)
        let event = AgentEvent::TurnStarted {
            turn_id: TurnId::new(),
        };
        assert!(store.append_event(ChatId::new(), &event).await.is_err());
    }

    #[tokio::test]
    async fn all_roles_round_trip() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();
        let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
        for (i, role) in roles.iter().enumerate() {
            store
                .append_message(&Message {
                    id: MessageId::new(),
                    chat_id: chat.id,
                    turn_id: TurnId::new(),
                    role: *role,
                    content: String::new(),
                    created_at: DateTime::<Utc>::from_timestamp(i as i64, 0).unwrap(),
                })
                .await
                .unwrap();
        }
        let got: Vec<Role> = store
            .list_messages(chat.id)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(got, roles);
    }

    #[tokio::test]
    async fn tool_calls_roundtrip_and_upsert_preserves_created_at() {
        let (_dir, store) = temp_store().await;
        let chat = sample_chat();
        store.create_chat(&chat).await.unwrap();

        let created = DateTime::<Utc>::from_timestamp(1_700_000_010, 0).unwrap();
        let call = ToolCallRecord {
            id: CallId::new(),
            chat_id: chat.id,
            turn_id: TurnId::new(),
            provider_id: "tu_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "note.txt"}),
            result: None,
            is_error: false,
            created_at: created,
            completed_at: None,
        };
        store.upsert_tool_call(&call).await.unwrap();

        let completed = DateTime::<Utc>::from_timestamp(1_700_000_011, 0).unwrap();
        store
            .upsert_tool_call(&ToolCallRecord {
                result: Some("hello".into()),
                is_error: false,
                created_at: Utc::now(), // must not overwrite the original
                completed_at: Some(completed),
                ..call.clone()
            })
            .await
            .unwrap();

        let listed = store.list_tool_calls(chat.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].created_at, created);
        assert_eq!(listed[0].completed_at, Some(completed));
        assert_eq!(listed[0].result.as_deref(), Some("hello"));
        assert_eq!(listed[0].arguments, serde_json::json!({"path": "note.txt"}));
    }
}
