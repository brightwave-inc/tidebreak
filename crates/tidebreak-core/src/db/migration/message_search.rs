//! `m20260924_000004_message_search`: one full-text index over what people
//! and the assistant wrote, in Work chats and in code sessions.
//!
//! `message_search` holds one row per indexed piece of a conversation: a chat
//! message, a code turn's input, or a code journal event (an assistant
//! message, a steer, or what a tool call acted on). A row carries only where
//! the text lives, never the text itself, so the index adds no second copy of
//! anyone's words. `source_key` names the piece within its session, and the
//! unique `(session_id, source_key)` index keeps a piece from being indexed
//! twice.
//!
//! The words are indexed per backend:
//!
//! - SQLite: `message_search_fts`, a contentless FTS5 table keyed by the row's
//!   `id`. The `ascii` tokenizer splits on the spaces between the folded
//!   terms `tidebreak_core::message_search` writes, and the prefix indexes
//!   answer a two- or three-letter prefix without scanning every term. A
//!   trigger removes a row's terms when the row goes, including when deleting
//!   its session cascades to it.
//! - PostgreSQL: a `tsvector` column on the row itself, cast from the same
//!   folded terms, under a GIN index.
//!
//! Rows die with their session. Deleting a conversation or a workspace's
//! sessions therefore empties their part of the index in the same statement.
//!
//! `message_search_backfill` lists the sessions whose history predates the
//! index. The server works through it in the background, newest first, and
//! the search route reports how many of the caller's conversations remain.
//! Only sessions with at least one turn are listed: a session with no turn
//! has nothing to index.
use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

pub(super) struct MessageSearch;

impl MigrationName for MessageSearch {
    fn name(&self) -> &str {
        "m20260924_000004_message_search"
    }
}

const TABLE: &str = "message_search";
const BACKFILL: &str = "message_search_backfill";

#[async_trait::async_trait]
impl MigrationTrait for MessageSearch {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        let table = || Alias::new(TABLE);
        let column = |name: &str| Alias::new(name);

        // SQLite's FTS5 rowid is this column, so it has to be the table's
        // rowid alias; PostgreSQL gets a 64-bit identity.
        let mut id = ColumnDef::new(column("id"));
        if backend == DbBackend::Sqlite {
            id.integer();
        } else {
            id.big_integer();
        }
        id.not_null().auto_increment().primary_key();
        let mut create = Table::create();
        create
            .table(table())
            .if_not_exists()
            .col(id)
            .col(ColumnDef::new(column("owner")).text().not_null())
            .col(ColumnDef::new(column("session_id")).uuid().not_null())
            .col(ColumnDef::new(column("source_key")).text().not_null())
            .col(ColumnDef::new(column("source")).text().not_null())
            .col(ColumnDef::new(column("turn_id")).uuid())
            .col(ColumnDef::new(column("message_id")).uuid())
            .col(ColumnDef::new(column("event_seq")).big_integer())
            .col(
                ColumnDef::new(column("created_at_micros"))
                    .big_integer()
                    .not_null(),
            );
        if backend == DbBackend::Postgres {
            create.col(
                ColumnDef::new(column("search_vector"))
                    .custom(Alias::new("tsvector"))
                    .not_null(),
            );
        }
        create
            .foreign_key(
                ForeignKey::create()
                    .name("fk_message_search_session")
                    .from(table(), column("session_id"))
                    .to(Alias::new("session"), Alias::new("id"))
                    .on_delete(ForeignKeyAction::Cascade),
            )
            .check(Expr::col(column("source")).is_in(["user", "assistant", "tool"]));
        manager.create_table(create.to_owned()).await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_message_search_source")
                    .table(table())
                    .col(column("session_id"))
                    .col(column("source_key"))
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_message_search_owner_recent")
                    .table(table())
                    .col(column("owner"))
                    .col(column("created_at_micros"))
                    .col(column("id"))
                    .to_owned(),
            )
            .await?;

        let connection = manager.get_connection();
        match backend {
            DbBackend::Sqlite => {
                connection
                    .execute_unprepared(
                        "CREATE VIRTUAL TABLE IF NOT EXISTS \"message_search_fts\" USING fts5(\
                         terms, content='', contentless_delete=1, tokenize='ascii', \
                         prefix='2 3')",
                    )
                    .await?;
                connection
                    .execute_unprepared(
                        "CREATE TRIGGER IF NOT EXISTS \"message_search_fts_delete\" \
                         AFTER DELETE ON \"message_search\" BEGIN \
                         DELETE FROM \"message_search_fts\" WHERE rowid = old.\"id\"; END",
                    )
                    .await?;
            }
            DbBackend::Postgres => {
                connection
                    .execute_unprepared(
                        "CREATE INDEX IF NOT EXISTS \"idx_message_search_vector\" \
                         ON \"message_search\" USING GIN (\"search_vector\")",
                    )
                    .await?;
            }
            other => {
                return Err(DbErr::Migration(format!(
                    "message search has no index for {other:?}"
                )));
            }
        }

        manager
            .create_table(
                Table::create()
                    .table(Alias::new(BACKFILL))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(column("session_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_message_search_backfill_session")
                            .from(Alias::new(BACKFILL), column("session_id"))
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // `WHERE` keeps SQLite from reading `ON CONFLICT` as a join
        // constraint of the `SELECT`.
        connection
            .execute_unprepared(
                r#"
INSERT INTO "message_search_backfill" ("session_id")
SELECT "session"."id" FROM "session"
WHERE EXISTS (SELECT 1 FROM "turn" WHERE "turn"."session_id" = "session"."id")
ON CONFLICT DO NOTHING
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let connection = manager.get_connection();
        if manager.get_database_backend() == DbBackend::Sqlite {
            connection
                .execute_unprepared("DROP TRIGGER IF EXISTS \"message_search_fts_delete\"")
                .await?;
            connection
                .execute_unprepared("DROP TABLE IF EXISTS \"message_search_fts\"")
                .await?;
        }
        for name in [BACKFILL, TABLE] {
            manager
                .drop_table(Table::drop().table(Alias::new(name)).if_exists().to_owned())
                .await?;
        }
        Ok(())
    }
}
