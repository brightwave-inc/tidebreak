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
    QueryFilter, QueryOrder, Set,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value;

use crate::error::{AgentError, Result};
use crate::event::{AgentEvent, SequencedEvent};
use crate::id::{MessageId, SessionId, TurnId};
use crate::model::{Message, Role, Session};
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

impl DbStore {
    /// Connect to `url` and run migrations. For a SQLite file that should be
    /// created if missing, include `?mode=rwc` (e.g.
    /// `sqlite:///path/openwave.db?mode=rwc`).
    pub async fn connect(url: &str) -> Result<Self> {
        let conn = Database::connect(url).await.map_err(store_err)?;
        // WAL lets a reader (e.g. the UI listing sessions) proceed concurrently
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
    async fn create_session(&self, session: &Session) -> Result<()> {
        entities::session::ActiveModel {
            id: Set(session.id.0),
            title: Set(session.title.clone()),
            workspace_dir: Set(session.workspace_dir.to_string_lossy().into_owned()),
            created_at: Set(session.created_at),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(())
    }

    async fn get_session(&self, id: SessionId) -> Result<Option<Session>> {
        Ok(entities::session::Entity::find_by_id(id.0)
            .one(&self.conn)
            .await
            .map_err(store_err)?
            .map(session_from_model))
    }

    async fn list_sessions(&self) -> Result<Vec<Session>> {
        Ok(entities::session::Entity::find()
            .order_by_desc(entities::session::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(session_from_model)
            .collect())
    }

    async fn append_message(&self, message: &Message) -> Result<()> {
        entities::message::ActiveModel {
            id: Set(message.id.0),
            session_id: Set(message.session_id.0),
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

    async fn list_messages(&self, session_id: SessionId) -> Result<Vec<Message>> {
        entities::message::Entity::find()
            .filter(entities::message::Column::SessionId.eq(session_id.0))
            .order_by_asc(entities::message::Column::CreatedAt)
            .all(&self.conn)
            .await
            .map_err(store_err)?
            .into_iter()
            .map(message_from_model)
            .collect()
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

    async fn append_event(&self, session_id: SessionId, event: &AgentEvent) -> Result<i64> {
        // Next seq for this session. A session's events are written by one turn
        // at a time (single writer), so read-then-insert is race-free in
        // practice; the composite (session_id, seq) primary key is the backstop
        // that turns any concurrent double-write into an error, not a silent dup.
        let last = entities::event::Entity::find()
            .filter(entities::event::Column::SessionId.eq(session_id.0))
            .order_by_desc(entities::event::Column::Seq)
            .one(&self.conn)
            .await
            .map_err(store_err)?;
        let seq = last.map_or(0, |model| model.seq) + 1;

        entities::event::ActiveModel {
            session_id: Set(session_id.0),
            seq: Set(seq),
            payload: Set(serde_json::to_value(event)?),
            created_at: Set(Utc::now()),
        }
        .insert(&self.conn)
        .await
        .map_err(store_err)?;
        Ok(seq)
    }

    async fn list_events(&self, session_id: SessionId, after: i64) -> Result<Vec<SequencedEvent>> {
        entities::event::Entity::find()
            .filter(entities::event::Column::SessionId.eq(session_id.0))
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

fn session_from_model(model: entities::session::Model) -> Session {
    Session {
        id: SessionId(model.id),
        title: model.title,
        workspace_dir: PathBuf::from(model.workspace_dir),
        created_at: model.created_at,
    }
}

fn message_from_model(model: entities::message::Model) -> Result<Message> {
    Ok(Message {
        id: MessageId(model.id),
        session_id: SessionId(model.session_id),
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

/// SeaORM entity models. Kept internal — the public `Store` API speaks the domain
/// types (`Session`, `Message`), never these, so the ORM never leaks into the
/// crate's contract.
mod entities {
    pub mod session {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "session")]
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

    pub mod message {
        use sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "message")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub id: Uuid,
            pub session_id: Uuid,
            pub turn_id: Uuid,
            pub role: String,
            pub content: String,
            pub created_at: DateTimeUtc,
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

        // Composite primary key `(session_id, seq)`: `seq` is monotonic *per
        // session*, and the pair both enforces uniqueness and indexes the
        // "this session's events after a cursor" replay query.
        #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
        #[sea_orm(table_name = "event")]
        pub struct Model {
            #[sea_orm(primary_key, auto_increment = false)]
            pub session_id: Uuid,
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

    pub struct Migrator;

    #[async_trait::async_trait]
    impl MigratorTrait for Migrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![Box::new(Init), Box::new(AddEventJournal)]
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
                        .table(Session::Table)
                        .if_not_exists()
                        .col(ColumnDef::new(Session::Id).uuid().not_null().primary_key())
                        .col(ColumnDef::new(Session::Title).text())
                        .col(ColumnDef::new(Session::WorkspaceDir).text().not_null())
                        .col(
                            ColumnDef::new(Session::CreatedAt)
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
                        .col(ColumnDef::new(Message::SessionId).uuid().not_null())
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
                                .name("fk_message_session")
                                .from(Message::Table, Message::SessionId)
                                .to(Session::Table, Session::Id),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_message_session")
                        .table(Message::Table)
                        .col(Message::SessionId)
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
                .drop_table(Table::drop().table(Session::Table).to_owned())
                .await?;
            Ok(())
        }
    }

    /// Adds the per-session event journal that clients replay from on connect.
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
                        .col(ColumnDef::new(Event::SessionId).uuid().not_null())
                        .col(ColumnDef::new(Event::Seq).big_integer().not_null())
                        .col(ColumnDef::new(Event::Payload).json_binary().not_null())
                        .col(
                            ColumnDef::new(Event::CreatedAt)
                                .timestamp_with_time_zone()
                                .not_null(),
                        )
                        .primary_key(Index::create().col(Event::SessionId).col(Event::Seq))
                        .foreign_key(
                            ForeignKey::create()
                                .name("fk_event_session")
                                .from(Event::Table, Event::SessionId)
                                .to(Session::Table, Session::Id),
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

    #[derive(DeriveIden)]
    enum Session {
        Table,
        Id,
        Title,
        WorkspaceDir,
        CreatedAt,
    }

    #[derive(DeriveIden)]
    enum Message {
        Table,
        Id,
        SessionId,
        TurnId,
        Role,
        Content,
        CreatedAt,
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
        SessionId,
        Seq,
        Payload,
        CreatedAt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    async fn temp_store() -> (tempfile::TempDir, DbStore) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let store = DbStore::connect(&url).await.unwrap();
        (dir, store)
    }

    fn sample_session() -> Session {
        Session {
            id: SessionId::new(),
            title: Some("hello".into()),
            workspace_dir: PathBuf::from("/tmp/ws"),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        }
    }

    #[tokio::test]
    async fn sessions_and_messages_roundtrip() {
        let (_dir, store) = temp_store().await;
        let session = sample_session();
        store.create_session(&session).await.unwrap();

        assert_eq!(
            store.get_session(session.id).await.unwrap().as_ref(),
            Some(&session)
        );
        assert_eq!(store.list_sessions().await.unwrap(), vec![session.clone()]);
        assert_eq!(store.get_session(SessionId::new()).await.unwrap(), None);

        let msg = Message {
            id: MessageId::new(),
            session_id: session.id,
            turn_id: TurnId::new(),
            role: Role::User,
            content: "hi there".into(),
            created_at: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
        };
        store.append_message(&msg).await.unwrap();
        assert_eq!(store.list_messages(session.id).await.unwrap(), vec![msg]);
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
    async fn list_sessions_is_newest_first_and_messages_oldest_first() {
        let (_dir, store) = temp_store().await;
        let mut older = sample_session();
        older.created_at = DateTime::<Utc>::from_timestamp(1_000, 0).unwrap();
        let mut newer = sample_session();
        newer.created_at = DateTime::<Utc>::from_timestamp(2_000, 0).unwrap();
        store.create_session(&older).await.unwrap();
        store.create_session(&newer).await.unwrap();
        // list_sessions is newest-first.
        assert_eq!(
            store.list_sessions().await.unwrap(),
            vec![newer.clone(), older.clone()]
        );

        // Messages come back oldest-first regardless of insert order.
        let msg = |ts: i64| Message {
            id: MessageId::new(),
            session_id: newer.id,
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
    async fn event_journal_assigns_per_session_seq_and_replays_after_cursor() {
        use crate::event::AgentEvent;
        use crate::id::TurnId;
        use crate::provider::{StopReason, Usage};

        let (_dir, store) = temp_store().await;
        let session = sample_session();
        store.create_session(&session).await.unwrap();

        let started = AgentEvent::TurnStarted {
            turn_id: TurnId::new(),
        };
        let completed = AgentEvent::TurnCompleted {
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        };
        assert_eq!(store.append_event(session.id, &started).await.unwrap(), 1);
        assert_eq!(store.append_event(session.id, &completed).await.unwrap(), 2);

        // From the start: both events, in order, with their seq.
        let all = store.list_events(session.id, 0).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].seq, all[1].seq), (1, 2));
        assert_eq!(all[0].event, started);

        // After a cursor: only the newer event (what a reconnecting client needs).
        let tail = store.list_events(session.id, 1).await.unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 2);
        assert_eq!(tail[0].event, completed);

        // A second session's seq restarts at 1 and its journal is isolated.
        let other = sample_session();
        store.create_session(&other).await.unwrap();
        assert_eq!(store.append_event(other.id, &started).await.unwrap(), 1);
        assert_eq!(store.list_events(session.id, 0).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn all_roles_round_trip() {
        let (_dir, store) = temp_store().await;
        let session = sample_session();
        store.create_session(&session).await.unwrap();
        let roles = [Role::System, Role::User, Role::Assistant, Role::Tool];
        for (i, role) in roles.iter().enumerate() {
            store
                .append_message(&Message {
                    id: MessageId::new(),
                    session_id: session.id,
                    turn_id: TurnId::new(),
                    role: *role,
                    content: String::new(),
                    created_at: DateTime::<Utc>::from_timestamp(i as i64, 0).unwrap(),
                })
                .await
                .unwrap();
        }
        let got: Vec<Role> = store
            .list_messages(session.id)
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.role)
            .collect();
        assert_eq!(got, roles);
    }
}
