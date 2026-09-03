//! `m20260902_000002_one_journal`: the chat journal moves into `code_event`.
//!
//! Decision 0048 step 5, slice D2. After this migration every engine's
//! events live in one table. The chat lane's four recovery receipts —
//! `lease_token`, `attempt_event_ordinal`, `scan_token`, `terminal` — and the
//! `turn_id` they hang off move onto `code_event` with their indexes, checks,
//! and foreign keys, so `append_turn_events` and
//! `recover_exact_turn_terminal_event` keep the contract they had on `event`.
//! Every `event` row is copied to `code_event` for its session with its
//! sequence number preserved, its `AgentEvent` payload rewritten into the
//! `Event` vocabulary (`crate::chat_journal::journal_row`), and the five
//! tables whose foreign keys named `event` are pointed at `code_event`. Then
//! `event` is dropped.
//!
//! The rows an internal-engine session already had in `code_event` were the
//! bridge's translated copies of the same chat events (#3010); they are
//! replaced by the backfill so the session has one copy of its history.
//!
//! The payload rewrite runs in Rust, in pages of a thousand rows, on the
//! migration connection: the mapping renames variants and moves fields,
//! which no portable SQL expresses, and a chat journal is thousands of small
//! rows, not millions. A payload the current binary cannot read fails the
//! migration by name: the chat journal fixture is the compatibility contract
//! for every shape ever written, so an unreadable row is corruption to look
//! at, not a row to drop.
//!
//! Idempotent on the absence of the legacy `event`, which is why `DROP TABLE
//! event` is the last statement on both backends. The later universal-name
//! migration renames `code_event` back to `event`, so a table with
//! `session_id` and no `code_event` also means this migration has finished.
//! The SQLite branch runs autocommit steps, each of which skips work an
//! interrupted attempt already did, and nothing may come after the step that
//! flips the marker.

use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

use crate::chat_journal;

pub(super) struct OneJournal;

impl MigrationName for OneJournal {
    fn name(&self) -> &str {
        "m20260902_000002_one_journal"
    }
}

/// The tables whose foreign key named `event`, in the frozen baseline.
pub(super) const EVENT_REFERENCING_TABLES: &[&str] = &[
    "plan_request",
    "sandbox_spawn_checkpoint",
    "tool_call",
    "turn_agent_run_wait_set",
    "user_question_request",
];

/// How many `event` rows one page of the backfill reads and writes.
const BACKFILL_PAGE: usize = 1_000;

/// The unique indexes the receipts need, as they were on `event`.
pub(super) const RECEIPT_INDEXES: &[&str] = &[
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_event_attempt_ordinal" ON "code_event" ("lease_token", "attempt_event_ordinal")"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_event_scan_token" ON "code_event" ("scan_token")"#,
    r#"CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_event_one_terminal_per_turn" ON "code_event" ("turn_id") WHERE "terminal" = TRUE"#,
];

#[async_trait::async_trait]
impl MigrationTrait for OneJournal {
    fn use_transaction(&self) -> Option<bool> {
        // The SQLite branch rebuilds six tables, each under its own manually
        // managed transaction with foreign keys disabled; PostgreSQL runs
        // its own.
        None
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("event").await? {
            return Ok(());
        }
        if !manager.has_table("code_event").await?
            && manager.has_column("event", "session_id").await?
        {
            return Ok(());
        }
        match manager.get_database_backend() {
            DbBackend::Postgres => {
                let transaction = manager.begin().await?;
                let connection = transaction.get_connection();
                connection
                    .execute_unprepared(
                        r#"
ALTER TABLE "code_event"
    ADD COLUMN "turn_id" uuid,
    ADD COLUMN "lease_token" uuid,
    ADD COLUMN "attempt_event_ordinal" integer,
    ADD COLUMN "scan_token" uuid,
    ADD COLUMN "terminal" boolean NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT "fk_code_event_turn"
        FOREIGN KEY ("session_id", "turn_id") REFERENCES "turn_run" ("chat_id", "id")
        ON DELETE RESTRICT,
    ADD CONSTRAINT "fk_code_event_claim"
        FOREIGN KEY ("turn_id", "lease_token") REFERENCES "turn_claim" ("turn_id", "token")
        ON DELETE RESTRICT,
    ADD CONSTRAINT "chk_code_event_terminal_turn"
        CHECK ("terminal" = FALSE OR "turn_id" IS NOT NULL),
    ADD CONSTRAINT "chk_code_event_attempt_identity"
        CHECK (("lease_token" IS NULL AND "attempt_event_ordinal" IS NULL)
            OR ("lease_token" IS NOT NULL AND ("attempt_event_ordinal" IS NOT NULL AND "turn_id" IS NOT NULL))),
    ADD CONSTRAINT "chk_code_event_attempt_ordinal"
        CHECK ("attempt_event_ordinal" IS NULL OR "attempt_event_ordinal" >= 1),
    ADD CONSTRAINT "chk_code_event_turn_receipt"
        CHECK ("turn_id" IS NULL OR "terminal" = TRUE OR "lease_token" IS NOT NULL),
    ADD CONSTRAINT "chk_code_event_scan_terminal"
        CHECK ("scan_token" IS NULL OR "terminal" = TRUE);
"#,
                    )
                    .await?;
                backfill(connection).await?;
                connection
                    .execute_unprepared(
                        r#"
DO $repoint$
DECLARE
    reference record;
BEGIN
    FOR reference IN
        SELECT
            constraint_row.conrelid::regclass::text AS table_name,
            constraint_row.conname AS constraint_name,
            (
                SELECT string_agg(quote_ident(attribute_row.attname), ', ' ORDER BY key.ordinality)
                FROM unnest(constraint_row.conkey) WITH ORDINALITY AS key(attnum, ordinality)
                JOIN pg_attribute AS attribute_row
                  ON attribute_row.attrelid = constraint_row.conrelid
                 AND attribute_row.attnum = key.attnum
            ) AS columns,
            constraint_row.confdeltype AS delete_action
        FROM pg_constraint AS constraint_row
        WHERE constraint_row.confrelid = 'event'::regclass
          AND constraint_row.contype = 'f'
    LOOP
        EXECUTE format(
            'ALTER TABLE %s DROP CONSTRAINT %I',
            reference.table_name,
            reference.constraint_name
        );
        EXECUTE format(
            'ALTER TABLE %s ADD CONSTRAINT %I FOREIGN KEY (%s) REFERENCES "code_event" ("session_id", "seq")%s',
            reference.table_name,
            reference.constraint_name,
            reference.columns,
            CASE reference.delete_action
                WHEN 'c' THEN ' ON DELETE CASCADE'
                WHEN 'r' THEN ' ON DELETE RESTRICT'
                WHEN 'n' THEN ' ON DELETE SET NULL'
                WHEN 'd' THEN ' ON DELETE SET DEFAULT'
                ELSE ''
            END
        );
    END LOOP;
END
$repoint$;
"#,
                    )
                    .await?;
                for index in RECEIPT_INDEXES {
                    connection.execute_unprepared(index).await?;
                }
                connection
                    .execute_unprepared(r#"DROP TABLE "event""#)
                    .await?;
                transaction.commit().await
            }
            DbBackend::Sqlite => {
                super::rebuild_sqlite_table(manager, "code_event", add_receipt_columns).await?;
                let transaction = manager.begin().await?;
                backfill(transaction.get_connection()).await?;
                transaction.commit().await?;
                for table in EVENT_REFERENCING_TABLES {
                    super::rebuild_sqlite_table(manager, table, repoint_event_reference).await?;
                }
                // The drop is the marker, so it goes last: everything an
                // interrupted attempt could have skipped is in place before
                // the next start can take the early return above.
                let connection = manager.get_connection();
                for index in RECEIPT_INDEXES {
                    connection.execute_unprepared(index).await?;
                }
                connection
                    .execute_unprepared(r#"DROP TABLE "event""#)
                    .await
                    .map(|_| ())
            }
            backend => Err(DbErr::Custom(format!(
                "unsupported database backend for the one-journal migration: {backend:?}"
            ))),
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The chat rows now live in `code_event`; recreating `event` would
        // leave every conversation's history in a table nothing reads.
        Ok(())
    }
}

/// The old `event` table, as this migration reads it. The live entity is
/// gone with the table, so the shape is kept here for the one read that
/// still needs it.
mod legacy_event {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub chat_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub seq: i64,
        pub turn_id: Option<Uuid>,
        pub lease_token: Option<Uuid>,
        pub attempt_event_ordinal: Option<i32>,
        pub scan_token: Option<Uuid>,
        pub terminal: bool,
        #[sea_orm(column_type = "JsonBinary")]
        pub payload: Json,
        pub created_at: DateTimeUtc,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The `code_session` columns this migration reads before the universal
/// table rename runs.
mod code_session {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_session")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub owner: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// The `code_event` row this migration writes before the universal table
/// rename runs.
mod code_event {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "code_event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub session_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub seq: i64,
        pub owner: String,
        #[sea_orm(column_type = "JsonBinary")]
        pub event: Json,
        pub created_at: DateTimeUtc,
        pub turn_id: Option<Uuid>,
        pub lease_token: Option<Uuid>,
        pub attempt_event_ordinal: Option<i32>,
        pub scan_token: Option<Uuid>,
        pub terminal: bool,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

/// Copy every `event` row into `code_event`, sequence preserved, payload
/// rewritten. Runs inside the caller's transaction.
///
/// Reads and writes go through the entities so every backend stores the ids
/// the way the live store binds them.
async fn backfill<C>(conn: &C) -> Result<(), DbErr>
where
    C: ConnectionTrait,
{
    use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set};

    // An internal-engine session's existing rows are the bridge's translated
    // copies of the chat events about to be copied in; the backfill is the
    // one copy the session keeps.
    conn.execute_unprepared(
        r#"DELETE FROM "code_event" WHERE "session_id" IN (SELECT DISTINCT "chat_id" FROM "event")"#,
    )
    .await?;
    let mut owners: HashMap<uuid::Uuid, String> = HashMap::new();
    let mut after: Option<(uuid::Uuid, i64)> = None;
    loop {
        let mut query = legacy_event::Entity::find()
            .order_by_asc(legacy_event::Column::ChatId)
            .order_by_asc(legacy_event::Column::Seq)
            .limit(BACKFILL_PAGE as u64);
        if let Some((chat_id, seq)) = after {
            query = query.filter(
                Condition::any()
                    .add(legacy_event::Column::ChatId.gt(chat_id))
                    .add(
                        Condition::all()
                            .add(legacy_event::Column::ChatId.eq(chat_id))
                            .add(legacy_event::Column::Seq.gt(seq)),
                    ),
            );
        }
        let rows = query.all(conn).await?;
        if rows.is_empty() {
            return Ok(());
        }
        let missing = rows
            .iter()
            .map(|row| row.chat_id)
            .filter(|chat_id| !owners.contains_key(chat_id))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            // Name the columns rather than reading the live entity: this
            // migration is historical, and the entity's model grows with the
            // schema of the chain's end, which does not exist yet here.
            use sea_orm::QuerySelect;
            for (id, owner) in code_session::Entity::find()
                .select_only()
                .column(code_session::Column::Id)
                .column(code_session::Column::Owner)
                .filter(code_session::Column::Id.is_in(missing))
                .into_tuple::<(uuid::Uuid, String)>()
                .all(conn)
                .await?
            {
                owners.insert(id, owner);
            }
        }
        let page = rows.len();
        let mut inserts = Vec::with_capacity(page);
        for row in rows {
            let owner = owners.get(&row.chat_id).cloned().ok_or_else(|| {
                DbErr::Custom(format!(
                    "event ({}, {}) names a session that does not exist",
                    row.chat_id, row.seq
                ))
            })?;
            let chat_event: crate::AgentEvent =
                serde_json::from_value(row.payload).map_err(|error| {
                    DbErr::Custom(format!(
                        "event ({}, {}) does not read as a chat event: {error}",
                        row.chat_id, row.seq
                    ))
                })?;
            let event =
                serde_json::to_value(chat_journal::journal_row(&chat_event)).map_err(|error| {
                    DbErr::Custom(format!("event ({}, {}): {error}", row.chat_id, row.seq))
                })?;
            after = Some((row.chat_id, row.seq));
            inserts.push(code_event::ActiveModel {
                owner: Set(owner),
                session_id: Set(row.chat_id),
                seq: Set(row.seq),
                event: Set(event),
                created_at: Set(row.created_at),
                turn_id: Set(row.turn_id),
                lease_token: Set(row.lease_token),
                attempt_event_ordinal: Set(row.attempt_event_ordinal),
                scan_token: Set(row.scan_token),
                terminal: Set(row.terminal),
            });
        }
        code_event::Entity::insert_many(inserts).exec(conn).await?;
        if page < BACKFILL_PAGE {
            return Ok(());
        }
    }
}

/// Add the chat lane's receipt columns, checks, and foreign keys to a stored
/// SQLite `code_event` definition. A definition that already carries them
/// comes back unchanged.
pub(super) fn add_receipt_columns(create: &str) -> Result<String, DbErr> {
    const TAIL: &str = r#""created_at" timestamp_with_timezone_text NOT NULL, PRIMARY KEY ("session_id", "seq"), FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"), CHECK ("seq" >= 1) )"#;
    const EXTENDED: &str = r#""created_at" timestamp_with_timezone_text NOT NULL, "turn_id" uuid_text, "lease_token" uuid_text, "attempt_event_ordinal" integer, "scan_token" uuid_text, "terminal" boolean NOT NULL DEFAULT FALSE, PRIMARY KEY ("session_id", "seq"), FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"), FOREIGN KEY ("session_id", "turn_id") REFERENCES "turn_run" ("chat_id", "id") ON DELETE RESTRICT, FOREIGN KEY ("turn_id", "lease_token") REFERENCES "turn_claim" ("turn_id", "token") ON DELETE RESTRICT, CHECK ("seq" >= 1), CHECK ("terminal" = FALSE OR "turn_id" IS NOT NULL), CHECK (("lease_token" IS NULL AND "attempt_event_ordinal" IS NULL) OR ("lease_token" IS NOT NULL AND ("attempt_event_ordinal" IS NOT NULL AND "turn_id" IS NOT NULL))), CHECK ("attempt_event_ordinal" IS NULL OR "attempt_event_ordinal" >= 1), CHECK ("turn_id" IS NULL OR "terminal" = TRUE OR "lease_token" IS NOT NULL), CHECK ("scan_token" IS NULL OR "terminal" = TRUE) )"#;

    if create.contains(r#""lease_token" uuid_text"#) {
        return Ok(create.to_owned());
    }
    // A definition SQLite stored from a rendered statement is one line; one
    // restored from the release fixtures keeps that fixture's line breaks.
    // Both describe the same table, so match on the tokens.
    let flat = create.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.matches(TAIL).count() {
        1 => Ok(flat.replacen(TAIL, EXTENDED, 1)),
        count => Err(DbErr::Custom(format!(
            "SQLite code_event definition has an unexpected shape ({count} tail matches): {create}"
        ))),
    }
}

/// Point a stored SQLite definition's `event` foreign key at `code_event`.
/// A definition that no longer names `event` comes back unchanged.
pub(super) fn repoint_event_reference(create: &str) -> Result<String, DbErr> {
    const EVENT: &str = r#"REFERENCES "event" ("chat_id", "seq")"#;
    const JOURNAL: &str = r#"REFERENCES "code_event" ("session_id", "seq")"#;

    let flat = create.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.matches(EVENT).count() {
        0 => Ok(create.to_owned()),
        1 => Ok(flat.replacen(EVENT, JOURNAL, 1)),
        count => Err(DbErr::Custom(format!(
            "SQLite definition names the event table {count} times: {create}"
        ))),
    }
}
