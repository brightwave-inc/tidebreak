//! `m20260923_000005_conversation_list_state`: where a conversation sits in
//! its owner's list of work.
//!
//! Four nullable timestamps on `session`:
//!
//! - `last_activity_at` orders the list. A turn that starts or ends, and a
//!   rename, move it forward. `NULL` means nothing has happened since the
//!   conversation was created, so its creation time stands in.
//! - `pinned_at` keeps a conversation at the top of the list.
//! - `archived_at` takes it out of the list without deleting it.
//! - `unread_since` records a turn that finished while the reader was
//!   elsewhere. Opening the conversation clears it.
//!
//! The backfill gives every existing session the time of its latest turn, so
//! an upgraded list keeps the order the reader last saw. Nothing starts
//! pinned, archived, or unread.
use sea_orm_migration::prelude::*;

pub(super) struct ConversationListState;

impl MigrationName for ConversationListState {
    fn name(&self) -> &str {
        "m20260923_000005_conversation_list_state"
    }
}

const COLUMNS: [&str; 4] = [
    "last_activity_at",
    "pinned_at",
    "archived_at",
    "unread_since",
];

#[async_trait::async_trait]
impl MigrationTrait for ConversationListState {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite adds one column per statement, so each is its own alter.
        for column in COLUMNS {
            if manager.has_column("session", column).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("session"))
                        .add_column(ColumnDef::new(Alias::new(column)).timestamp_with_time_zone())
                        .to_owned(),
                )
                .await?;
        }
        // Both backends accept the correlated subquery. A session with no
        // turns keeps `NULL`, which reads as its creation time.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
UPDATE "session" SET "last_activity_at" = (
    SELECT MAX(COALESCE("turn"."ended_at", "turn"."started_at"))
    FROM "turn"
    WHERE "turn"."session_id" = "session"."id"
)
WHERE "last_activity_at" IS NULL
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in COLUMNS {
            if !manager.has_column("session", column).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("session"))
                        .drop_column(Alias::new(column))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
