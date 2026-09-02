//! `m20260903_000001_one_turn_lane`: one turn row, one durable lease.
//!
//! Decision 0048 step 5, slice D4a. `code_turn` absorbs `turn_run`. The
//! admission ledger retires; identity lives on the turn row as
//! `fingerprint`. `turn_claim` is renamed to `code_turn_claim` rather than
//! dropped — seven composite foreign keys make append idempotence and
//! heartbeat fencing a property of the schema. `queued_turn` rows move onto
//! `code_queued_turn`. `turn_steer` and `turn_failure` are re-keyed.
//! `message_attachment` merges onto `code_turn_attachment` with a nullable
//! `message_id`. Document attachments become `code_turn_document_attachment`.
//!
//! Idempotent on `code_turn.lease_token` existing **and** `turn_run` being
//! absent. The SQLite path is not one transaction, so a retry checks both.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sea_orm_migration::prelude::*;

pub(super) struct OneTurnLane;

impl MigrationName for OneTurnLane {
    fn name(&self) -> &str {
        "m20260903_000001_one_turn_lane"
    }
}

/// Status tokens the merged turn row accepts.
const MERGED_STATUS: &str = "'queued', 'running', 'waiting', 'cancelling', \
     'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', \
     'resuming', 'retry_wait', 'completed', 'failed', 'interrupted', 'cancelled'";
const LIVE_STATUS: &str = "'queued', 'running', 'waiting', 'cancelling', \
     'waiting_for_client', 'waiting_for_agent_run', 'cancelling_client', \
     'resuming', 'retry_wait'";

/// Child tables whose foreign keys named `turn_run` or `turn_claim`.
const TURN_REFERENCING_TABLES: &[&str] = &["code_event", "task_plan"];

#[async_trait::async_trait]
impl MigrationTrait for OneTurnLane {
    fn use_transaction(&self) -> Option<bool> {
        None
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("code_turn", "lease_token").await?
            && !manager.has_table("turn_run").await?
        {
            return Ok(());
        }
        match manager.get_database_backend() {
            DbBackend::Postgres => postgres_up(manager).await,
            DbBackend::Sqlite => sqlite_up(manager).await,
            backend => Err(DbErr::Custom(format!(
                "unsupported database backend for the one-turn-lane migration: {backend:?}"
            ))),
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

async fn postgres_up(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let transaction = manager.begin().await?;
    let connection = transaction.get_connection();
    connection
        .execute_unprepared(&format!(
            r#"
DO $status$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_turn'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'status'
    LOOP
        EXECUTE format('ALTER TABLE "code_turn" DROP CONSTRAINT %I', old_constraint);
    END LOOP;
END
$status$;
ALTER TABLE "code_turn"
    ADD COLUMN IF NOT EXISTS "attempt_count" integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "max_attempts" integer NOT NULL DEFAULT 5,
    ADD COLUMN IF NOT EXISTS "claim_count" integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "model_steps" integer NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "input_tokens" bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "output_tokens" bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "cache_read_input_tokens" bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "cache_creation_input_tokens" bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "available_at" timestamptz,
    ADD COLUMN IF NOT EXISTS "lease_token" uuid,
    ADD COLUMN IF NOT EXISTS "lease_expires_at" timestamptz,
    ADD COLUMN IF NOT EXISTS "last_error_code" varchar(128),
    ADD COLUMN IF NOT EXISTS "last_error_detail" varchar(4096),
    ADD COLUMN IF NOT EXISTS "steer_revision" bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS "last_steer_applied_at" timestamptz,
    ADD COLUMN IF NOT EXISTS "invoked_skills" jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS "voice_input_used" boolean NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS "input_message_id" uuid,
    ADD COLUMN IF NOT EXISTS "output_message_id" uuid,
    ADD COLUMN IF NOT EXISTS "updated_at" timestamptz,
    ADD COLUMN IF NOT EXISTS "fingerprint" bytea,
    ADD CONSTRAINT "code_turn_status_check"
        CHECK ("status" IN ({MERGED_STATUS}));
ALTER TABLE "code_turn"
    ADD CONSTRAINT "code_turn_attempt_check"
        CHECK ("attempt_count" >= 0 AND "max_attempts" >= 1 AND "attempt_count" <= "max_attempts"),
    ADD CONSTRAINT "code_turn_claim_check"
        CHECK ("claim_count" >= "attempt_count"),
    ADD CONSTRAINT "code_turn_lease_check"
        CHECK ("input_message_id" IS NULL OR (("status" IN ('running', 'cancelling') AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL) OR ("status" <> 'running' AND "status" <> 'cancelling' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL))),
    ADD CONSTRAINT "code_turn_error_check"
        CHECK ("input_message_id" IS NULL OR (("status" IN ('retry_wait', 'failed') AND "last_error_code" IS NOT NULL) OR ("status" NOT IN ('retry_wait', 'failed') AND "last_error_code" IS NULL AND "last_error_detail" IS NULL))),
    ADD CONSTRAINT "code_turn_ended_check"
        CHECK ("input_message_id" IS NULL OR (("status" IN ('completed', 'failed', 'interrupted', 'cancelled') AND "ended_at" IS NOT NULL) OR ("status" NOT IN ('completed', 'failed', 'interrupted', 'cancelled') AND "ended_at" IS NULL))),
    ADD CONSTRAINT "code_turn_usage_check"
        CHECK ("model_steps" >= 0 AND "model_steps" <= 2147483647 AND "input_tokens" >= 0 AND "input_tokens" <= 4294967295 AND "output_tokens" >= 0 AND "output_tokens" <= 4294967295 AND "cache_read_input_tokens" >= 0 AND "cache_read_input_tokens" <= 4294967295 AND "cache_creation_input_tokens" >= 0 AND "cache_creation_input_tokens" <= 4294967295),
    ADD CONSTRAINT "code_turn_output_check"
        CHECK ("input_message_id" IS NULL OR (("status" = 'completed' AND "output_message_id" IS NOT NULL) OR ("status" IN ('interrupted', 'cancelled')) OR ("status" NOT IN ('completed', 'interrupted', 'cancelled') AND "output_message_id" IS NULL))),
    ADD CONSTRAINT "code_turn_steer_check"
        CHECK ("steer_revision" >= 0 AND (("steer_revision" = 0 AND "last_steer_applied_at" IS NULL) OR ("steer_revision" >= 1 AND "last_steer_applied_at" IS NOT NULL))),
    ADD CONSTRAINT "code_turn_model_check"
        CHECK ("model" IS NULL OR (LENGTH("model") BETWEEN 1 AND 512));
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_session_identity"
    ON "code_turn" ("session_id", "id");
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_lease_token"
    ON "code_turn" ("lease_token");
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_one_active"
    ON "code_turn" ("session_id") WHERE "status" IN ({LIVE_STATUS}) AND "input_message_id" IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_input_message"
    ON "code_turn" ("input_message_id");
CREATE INDEX IF NOT EXISTS "idx_code_turn_due"
    ON "code_turn" ("status", "available_at", "started_at");
CREATE INDEX IF NOT EXISTS "idx_code_turn_stale_lease"
    ON "code_turn" ("status", "lease_expires_at");
"#
        ))
        .await?;

    if manager.has_table("turn_claim").await? {
        connection
            .execute_unprepared(
                r#"
ALTER TABLE "turn_claim" RENAME TO "code_turn_claim";
ALTER TABLE "code_turn_claim" ADD COLUMN IF NOT EXISTS "owner" text;
UPDATE "code_turn_claim" AS claim
SET "owner" = session.owner
FROM "turn_run" AS turn
JOIN "code_session" AS session ON session.id = turn.chat_id
WHERE turn.id = claim.turn_id AND (claim.owner IS NULL OR claim.owner = '');
UPDATE "code_turn_claim" SET "owner" = 'local' WHERE "owner" IS NULL;
ALTER TABLE "code_turn_claim" ALTER COLUMN "owner" SET NOT NULL;
"#,
            )
            .await?;
    }
    if manager.has_table("turn_steer").await? {
        connection
            .execute_unprepared(
                r#"
ALTER TABLE "turn_steer" RENAME TO "code_turn_steer";
ALTER TABLE "code_turn_steer" RENAME COLUMN "chat_id" TO "session_id";
ALTER TABLE "code_turn_steer" ADD COLUMN IF NOT EXISTS "owner" text;
UPDATE "code_turn_steer" AS steer
SET "owner" = session.owner
FROM "code_session" AS session
WHERE session.id = steer.session_id AND (steer.owner IS NULL OR steer.owner = '');
UPDATE "code_turn_steer" SET "owner" = 'local' WHERE "owner" IS NULL;
ALTER TABLE "code_turn_steer" ALTER COLUMN "owner" SET NOT NULL;
"#,
            )
            .await?;
    }
    if manager.has_table("turn_failure").await? {
        connection
            .execute_unprepared(
                r#"
ALTER TABLE "turn_failure" RENAME TO "code_turn_failure";
ALTER TABLE "code_turn_failure" ADD COLUMN IF NOT EXISTS "owner" text;
UPDATE "code_turn_failure" AS failure
SET "owner" = session.owner
FROM "turn_run" AS turn
JOIN "code_session" AS session ON session.id = turn.chat_id
WHERE turn.id = failure.turn_id AND (failure.owner IS NULL OR failure.owner = '');
UPDATE "code_turn_failure" SET "owner" = 'local' WHERE "owner" IS NULL;
ALTER TABLE "code_turn_failure" ALTER COLUMN "owner" SET NOT NULL;
"#,
            )
            .await?;
    }

    if manager.has_table("turn_run").await? {
        let collisions = connection
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                r#"SELECT count(*)::bigint AS n FROM "code_turn" JOIN "turn_run" USING ("id")"#
                    .to_owned(),
            ))
            .await?;
        let n = collisions
            .and_then(|row| row.try_get::<i64>("", "n").ok())
            .unwrap_or(0);
        if n != 0 {
            return Err(DbErr::Custom(format!(
                "one-turn-lane: {n} code_turn ids collide with turn_run; refusing to merge"
            )));
        }
        connection
            .execute_unprepared(
                r#"
INSERT INTO "code_turn" (
    "id", "owner", "session_id", "ordinal", "status", "model", "fast_mode",
    "user_input", "started_at", "ended_at",
    "attempt_count", "max_attempts", "claim_count", "model_steps",
    "input_tokens", "output_tokens", "cache_read_input_tokens",
    "cache_creation_input_tokens", "available_at", "lease_token",
    "lease_expires_at", "last_error_code", "last_error_detail",
    "steer_revision", "last_steer_applied_at", "invoked_skills",
    "voice_input_used", "input_message_id", "output_message_id",
    "updated_at", "fingerprint"
)
SELECT
    turn.id,
    session.owner,
    turn.chat_id,
    COALESCE(sess.max_ord, 0) + ROW_NUMBER() OVER (
        PARTITION BY turn.chat_id ORDER BY turn.started_at, turn.id
    ),
    CASE turn.status WHEN 'cancelled' THEN 'interrupted' ELSE turn.status END,
    turn.model,
    FALSE,
    COALESCE(message.content, ''),
    COALESCE(turn.started_at, turn.created_at),
    turn.finished_at,
    turn.attempt_count,
    turn.max_attempts,
    turn.claim_count,
    turn.model_steps,
    turn.input_tokens,
    turn.output_tokens,
    turn.cache_read_input_tokens,
    turn.cache_creation_input_tokens,
    turn.available_at,
    turn.lease_token,
    turn.lease_expires_at,
    turn.last_error_code,
    turn.last_error_detail,
    turn.steer_revision,
    turn.last_steer_applied_at,
    turn.invoked_skills,
    turn.voice_input_used,
    turn.input_message_id,
    turn.output_message_id,
    turn.updated_at,
    admission.fingerprint
FROM "turn_run" AS turn
JOIN "code_session" AS session ON session.id = turn.chat_id
LEFT JOIN "message" AS message ON message.id = turn.input_message_id
LEFT JOIN "turn_admission" AS admission ON admission.id = turn.id
LEFT JOIN (
    SELECT session_id, MAX(ordinal) AS max_ord FROM code_turn GROUP BY session_id
) AS sess ON sess.session_id = turn.chat_id
WHERE NOT EXISTS (SELECT 1 FROM code_turn existing WHERE existing.id = turn.id);
"#,
            )
            .await?;
    }

    connection
        .execute_unprepared(
            r#"
ALTER TABLE "code_queued_turn"
    ADD COLUMN IF NOT EXISTS "file_attachments_json" text NOT NULL DEFAULT '[]',
    ADD COLUMN IF NOT EXISTS "invoked_skills_json" text NOT NULL DEFAULT '[]',
    ADD COLUMN IF NOT EXISTS "voice_input_used" boolean NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS "fingerprint" bytea;
"#,
        )
        .await?;
    if manager.has_table("queued_turn").await? {
        connection
            .execute_unprepared(
                r#"
INSERT INTO "code_queued_turn" (
    "id", "owner", "session_id", "message", "attachments_json",
    "file_attachments_json", "invoked_skills_json", "voice_input_used",
    "fingerprint", "position", "created_at", "updated_at"
)
SELECT
    queued.id,
    session.owner,
    queued.chat_id,
    queued.content,
    queued.attachments_json,
    queued.file_attachments_json,
    queued.invoked_skills_json,
    queued.voice_input_used,
    admission.fingerprint,
    queued.position,
    queued.created_at,
    queued.updated_at
FROM "queued_turn" AS queued
JOIN "code_session" AS session ON session.id = queued.chat_id
LEFT JOIN "turn_admission" AS admission ON admission.id = queued.id
WHERE NOT EXISTS (
    SELECT 1 FROM code_queued_turn existing WHERE existing.id = queued.id
);
"#,
            )
            .await?;
    }

    connection
        .execute_unprepared(
            r#"
ALTER TABLE "code_turn_attachment"
    ADD COLUMN IF NOT EXISTS "message_id" uuid;
CREATE INDEX IF NOT EXISTS "idx_code_turn_attachment_message"
    ON "code_turn_attachment" ("message_id");
"#,
        )
        .await?;
    if manager.has_table("message_attachment").await? {
        connection
            .execute_unprepared(
                r#"
INSERT INTO "code_turn_attachment" (
    "turn_id", "ordinal", "owner", "blob_id", "media_type",
    "width", "height", "byte_len", "message_id"
)
SELECT
    message.turn_id,
    ROW_NUMBER() OVER (
        PARTITION BY message.turn_id ORDER BY attachment.message_id, attachment.ordinal
    ) - 1,
    session.owner,
    attachment.blob_id,
    attachment.media_type,
    attachment.width,
    attachment.height,
    attachment.byte_len,
    attachment.message_id
FROM "message_attachment" AS attachment
JOIN "message" ON message.id = attachment.message_id
JOIN "code_session" AS session ON session.id = message.chat_id
WHERE message.turn_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM code_turn_attachment existing
      WHERE existing.turn_id = message.turn_id
        AND existing.message_id = attachment.message_id
        AND existing.ordinal = attachment.ordinal
  );
"#,
            )
            .await?;
    }

    connection
        .execute_unprepared(
            r#"
CREATE TABLE IF NOT EXISTS "code_turn_document_attachment" (
    "turn_id" uuid NOT NULL,
    "ordinal" integer NOT NULL,
    "owner" text NOT NULL,
    "message_id" uuid,
    "document_id" uuid NOT NULL,
    "created_at" timestamptz NOT NULL,
    PRIMARY KEY ("turn_id", "ordinal"),
    FOREIGN KEY ("turn_id") REFERENCES "code_turn" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("document_id") REFERENCES "document" ("id") ON DELETE CASCADE
);
"#,
        )
        .await?;
    if manager.has_table("message_document_attachment").await? {
        connection
            .execute_unprepared(
                r#"
INSERT INTO "code_turn_document_attachment" (
    "turn_id", "ordinal", "owner", "message_id", "document_id", "created_at"
)
SELECT
    message.turn_id,
    ROW_NUMBER() OVER (
        PARTITION BY message.turn_id ORDER BY attachment.message_id, attachment.ordinal
    ) - 1,
    session.owner,
    attachment.message_id,
    attachment.document_id,
    attachment.created_at
FROM "message_document_attachment" AS attachment
JOIN "message" ON message.id = attachment.message_id
JOIN "code_session" AS session ON session.id = message.chat_id
WHERE message.turn_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM code_turn_document_attachment existing
      WHERE existing.turn_id = message.turn_id
        AND existing.message_id = attachment.message_id
        AND existing.ordinal = attachment.ordinal
  );
"#,
            )
            .await?;
    }

    connection
        .execute_unprepared(
            r#"
DO $rename$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'sandbox_spawn_checkpoint' AND column_name = 'chat_id'
    ) THEN
        ALTER TABLE "sandbox_spawn_checkpoint" RENAME COLUMN "chat_id" TO "session_id";
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'turn_client_wait' AND column_name = 'chat_id'
    ) THEN
        ALTER TABLE "turn_client_wait" RENAME COLUMN "chat_id" TO "session_id";
    END IF;
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_name = 'turn_agent_run_wait_set' AND column_name = 'chat_id'
    ) THEN
        ALTER TABLE "turn_agent_run_wait_set" RENAME COLUMN "chat_id" TO "session_id";
    END IF;
END
$rename$;
"#,
        )
        .await?;

    connection
        .execute_unprepared(
            r#"
DO $repoint$
DECLARE
    reference record;
    target_table text;
    target_columns text;
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
            constraint_row.confrelid::regclass::text AS old_target,
            (
                SELECT string_agg(quote_ident(attribute_row.attname), ', ' ORDER BY key.ordinality)
                FROM unnest(constraint_row.confkey) WITH ORDINALITY AS key(attnum, ordinality)
                JOIN pg_attribute AS attribute_row
                  ON attribute_row.attrelid = constraint_row.confrelid
                 AND attribute_row.attnum = key.attnum
            ) AS old_target_columns,
            constraint_row.confdeltype AS delete_action
        FROM pg_constraint AS constraint_row
        WHERE constraint_row.contype = 'f'
          AND constraint_row.confrelid IN (
                'turn_run'::regclass,
                'turn_claim'::regclass,
                'code_turn_claim'::regclass,
                'queued_turn'::regclass,
                'message_attachment'::regclass
              )
    LOOP
        IF reference.old_target IN ('turn_run', '"turn_run"') THEN
            target_table := '"code_turn"';
            IF reference.old_target_columns LIKE '%agent_run_id%' THEN
                target_columns := '"id", "session_id"';
            ELSIF reference.old_target_columns LIKE '%chat_id%' THEN
                target_columns := '"session_id", "id"';
            ELSE
                target_columns := '"id"';
            END IF;
        ELSIF reference.old_target IN ('turn_claim', '"turn_claim"', 'code_turn_claim', '"code_turn_claim"') THEN
            target_table := '"code_turn_claim"';
            target_columns := reference.old_target_columns;
        ELSE
            CONTINUE;
        END IF;
        EXECUTE format(
            'ALTER TABLE %s DROP CONSTRAINT %I',
            reference.table_name,
            reference.constraint_name
        );
        BEGIN
            EXECUTE format(
                'ALTER TABLE %s ADD CONSTRAINT %I FOREIGN KEY (%s) REFERENCES %s (%s)%s',
                reference.table_name,
                reference.constraint_name,
                CASE
                    WHEN reference.old_target_columns LIKE '%agent_run_id%'
                        THEN regexp_replace(reference.columns, ', [^,]*$', '')
                    ELSE reference.columns
                END,
                target_table,
                target_columns,
                CASE reference.delete_action
                    WHEN 'c' THEN ' ON DELETE CASCADE'
                    WHEN 'r' THEN ' ON DELETE RESTRICT'
                    WHEN 'n' THEN ' ON DELETE SET NULL'
                    WHEN 'd' THEN ' ON DELETE SET DEFAULT'
                    ELSE ''
                END
            );
        EXCEPTION WHEN undefined_object OR invalid_foreign_key THEN
            -- A child whose column list no longer matches is rebuilt below.
            NULL;
        END;
    END LOOP;
END
$repoint$;
"#,
        )
        .await?;

    connection
        .execute_unprepared(
            r#"
DROP TABLE IF EXISTS "turn_run";
DROP TABLE IF EXISTS "turn_admission";
DROP TABLE IF EXISTS "queued_turn";
DROP TABLE IF EXISTS "message_attachment";
DROP TABLE IF EXISTS "message_document_attachment";
"#,
        )
        .await?;
    transaction.commit().await
}

async fn sqlite_up(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let connection = manager.get_connection();
    if manager.has_table("turn_claim").await? {
        connection
            .execute_unprepared(r#"ALTER TABLE "turn_claim" RENAME TO "code_turn_claim""#)
            .await?;
    }
    if manager.has_table("code_turn_claim").await? {
        super::rebuild_sqlite_table(manager, "code_turn_claim", rewrite_code_turn_claim).await?;
        connection
            .execute_unprepared(
                r#"
UPDATE "code_turn_claim"
SET "owner" = COALESCE(
    (SELECT "owner" FROM "code_turn" WHERE "code_turn"."id" = "code_turn_claim"."turn_id"),
    (SELECT "code_session"."owner" FROM "turn_run"
     JOIN "code_session" ON "code_session"."id" = "turn_run"."chat_id"
     WHERE "turn_run"."id" = "code_turn_claim"."turn_id"),
    'local'
)
WHERE "owner" IS NULL OR "owner" = '';
"#,
            )
            .await?;
    }
    if manager.has_table("turn_steer").await? {
        connection
            .execute_unprepared(r#"ALTER TABLE "turn_steer" RENAME TO "code_turn_steer""#)
            .await?;
    }
    if manager.has_table("code_turn_steer").await? {
        super::rebuild_sqlite_table(manager, "code_turn_steer", rewrite_code_turn_steer).await?;
        connection
            .execute_unprepared(
                r#"
UPDATE "code_turn_steer"
SET "owner" = COALESCE(
    (SELECT "owner" FROM "code_session" WHERE "code_session"."id" = "code_turn_steer"."session_id"),
    'local'
)
WHERE "owner" IS NULL OR "owner" = '';
"#,
            )
            .await?;
    }
    if manager.has_table("turn_failure").await? {
        connection
            .execute_unprepared(r#"ALTER TABLE "turn_failure" RENAME TO "code_turn_failure""#)
            .await?;
    }
    if manager.has_table("code_turn_failure").await? {
        super::rebuild_sqlite_table(manager, "code_turn_failure", rewrite_code_turn_failure)
            .await?;
        connection
            .execute_unprepared(
                r#"
UPDATE "code_turn_failure"
SET "owner" = COALESCE(
    (SELECT "owner" FROM "code_turn" WHERE "code_turn"."id" = "code_turn_failure"."turn_id"),
    (SELECT "code_session"."owner" FROM "turn_run"
     JOIN "code_session" ON "code_session"."id" = "turn_run"."chat_id"
     WHERE "turn_run"."id" = "code_turn_failure"."turn_id"),
    'local'
)
WHERE "owner" IS NULL OR "owner" = '';
"#,
            )
            .await?;
    }

    super::rebuild_sqlite_table(manager, "code_turn", rewrite_code_turn).await?;
    connection
        .execute_unprepared(&format!(
            r#"
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_session_identity"
    ON "code_turn" ("session_id", "id");
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_lease_token"
    ON "code_turn" ("lease_token");
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_one_active"
    ON "code_turn" ("session_id") WHERE "status" IN ({LIVE_STATUS}) AND "input_message_id" IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS "idx_code_turn_input_message"
    ON "code_turn" ("input_message_id");
"#
        ))
        .await?;
    super::rebuild_sqlite_table(manager, "code_queued_turn", rewrite_code_queued_turn).await?;
    super::rebuild_sqlite_table(
        manager,
        "code_turn_attachment",
        rewrite_code_turn_attachment,
    )
    .await?;

    for table in TURN_REFERENCING_TABLES {
        if manager.has_table(table).await? {
            super::rebuild_sqlite_table(manager, table, rewrite_turn_references).await?;
        }
    }
    if manager.has_table("sandbox_spawn_checkpoint").await? {
        super::rebuild_sqlite_table(
            manager,
            "sandbox_spawn_checkpoint",
            rewrite_sandbox_spawn_checkpoint,
        )
        .await?;
    }
    if manager.has_table("turn_client_wait").await? {
        super::rebuild_sqlite_table(manager, "turn_client_wait", rewrite_turn_client_wait).await?;
    }
    if manager.has_table("turn_agent_run_wait_set").await? {
        super::rebuild_sqlite_table(
            manager,
            "turn_agent_run_wait_set",
            rewrite_turn_agent_run_wait_set,
        )
        .await?;
    }
    if manager.has_table("turn_agent_run_wait_member").await? {
        super::rebuild_sqlite_table(
            manager,
            "turn_agent_run_wait_member",
            rewrite_turn_agent_run_wait_member,
        )
        .await?;
    }

    if manager.has_table("turn_run").await? {
        let collisions = connection
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                r#"SELECT count(*) AS n FROM "code_turn" JOIN "turn_run" USING ("id")"#.to_owned(),
            ))
            .await?;
        let n = collisions
            .and_then(|row| row.try_get::<i64>("", "n").ok())
            .unwrap_or(0);
        if n != 0 {
            return Err(DbErr::Custom(format!(
                "one-turn-lane: {n} code_turn ids collide with turn_run; refusing to merge"
            )));
        }
        let copy = manager.begin().await?;
        copy.get_connection()
            .execute_unprepared(
                r#"
INSERT INTO "code_turn" (
    "id", "owner", "session_id", "ordinal", "status", "model", "fast_mode",
    "user_input", "started_at", "ended_at",
    "attempt_count", "max_attempts", "claim_count", "model_steps",
    "input_tokens", "output_tokens", "cache_read_input_tokens",
    "cache_creation_input_tokens", "available_at", "lease_token",
    "lease_expires_at", "last_error_code", "last_error_detail",
    "steer_revision", "last_steer_applied_at", "invoked_skills",
    "voice_input_used", "input_message_id", "output_message_id",
    "updated_at", "fingerprint"
)
SELECT
    turn.id,
    session.owner,
    turn.chat_id,
    COALESCE(
        (SELECT MAX(ordinal) FROM code_turn WHERE session_id = turn.chat_id),
        0
    ) + (
        SELECT COUNT(*) FROM turn_run earlier
        WHERE earlier.chat_id = turn.chat_id
          AND (COALESCE(earlier.started_at, earlier.created_at) < COALESCE(turn.started_at, turn.created_at)
               OR (COALESCE(earlier.started_at, earlier.created_at) = COALESCE(turn.started_at, turn.created_at) AND earlier.id <= turn.id))
    ),
    CASE turn.status WHEN 'cancelled' THEN 'interrupted' ELSE turn.status END,
    turn.model,
    0,
    COALESCE((SELECT content FROM message WHERE message.id = turn.input_message_id), ''),
    COALESCE(turn.started_at, turn.created_at),
    turn.finished_at,
    turn.attempt_count,
    turn.max_attempts,
    turn.claim_count,
    turn.model_steps,
    turn.input_tokens,
    turn.output_tokens,
    turn.cache_read_input_tokens,
    turn.cache_creation_input_tokens,
    turn.available_at,
    turn.lease_token,
    turn.lease_expires_at,
    turn.last_error_code,
    turn.last_error_detail,
    turn.steer_revision,
    turn.last_steer_applied_at,
    turn.invoked_skills,
    turn.voice_input_used,
    turn.input_message_id,
    turn.output_message_id,
    turn.updated_at,
    (SELECT fingerprint FROM turn_admission WHERE turn_admission.id = turn.id)
FROM "turn_run" AS turn
JOIN "code_session" AS session ON session.id = turn.chat_id
WHERE NOT EXISTS (SELECT 1 FROM code_turn existing WHERE existing.id = turn.id);
"#,
            )
            .await?;
        copy.commit().await?;
    }

    if manager.has_table("queued_turn").await? {
        let copy = manager.begin().await?;
        copy.get_connection()
            .execute_unprepared(
                r#"
INSERT INTO "code_queued_turn" (
    "id", "owner", "session_id", "message", "attachments_json",
    "file_attachments_json", "invoked_skills_json", "voice_input_used",
    "fingerprint", "position", "created_at", "updated_at"
)
SELECT
    queued.id,
    session.owner,
    queued.chat_id,
    queued.content,
    queued.attachments_json,
    queued.file_attachments_json,
    queued.invoked_skills_json,
    queued.voice_input_used,
    (SELECT fingerprint FROM turn_admission WHERE turn_admission.id = queued.id),
    queued.position,
    queued.created_at,
    queued.updated_at
FROM "queued_turn" AS queued
JOIN "code_session" AS session ON session.id = queued.chat_id
WHERE NOT EXISTS (
    SELECT 1 FROM code_queued_turn existing WHERE existing.id = queued.id
);
"#,
            )
            .await?;
        copy.commit().await?;
    }

    if !manager.has_table("code_turn_document_attachment").await? {
        connection
            .execute_unprepared(
                r#"
CREATE TABLE "code_turn_document_attachment" (
    "turn_id" uuid_text NOT NULL,
    "ordinal" integer NOT NULL,
    "owner" text NOT NULL,
    "message_id" uuid_text,
    "document_id" uuid_text NOT NULL,
    "created_at" timestamp_with_timezone_text NOT NULL,
    PRIMARY KEY ("turn_id", "ordinal"),
    FOREIGN KEY ("turn_id") REFERENCES "code_turn" ("id") ON DELETE CASCADE,
    FOREIGN KEY ("document_id") REFERENCES "document" ("id") ON DELETE CASCADE
);
"#,
            )
            .await?;
    }
    if manager.has_table("message_attachment").await? {
        let copy = manager.begin().await?;
        copy.get_connection()
            .execute_unprepared(
                r#"
INSERT INTO "code_turn_attachment" (
    "turn_id", "ordinal", "owner", "blob_id", "media_type",
    "width", "height", "byte_len", "message_id"
)
SELECT
    message.turn_id,
    (
        SELECT COUNT(*) FROM message_attachment earlier
        JOIN message earlier_message ON earlier_message.id = earlier.message_id
        WHERE earlier_message.turn_id = message.turn_id
          AND (earlier.message_id < attachment.message_id
               OR (earlier.message_id = attachment.message_id
                   AND earlier.ordinal <= attachment.ordinal))
    ) - 1,
    session.owner,
    attachment.blob_id,
    attachment.media_type,
    attachment.width,
    attachment.height,
    attachment.byte_len,
    attachment.message_id
FROM "message_attachment" AS attachment
JOIN "message" ON message.id = attachment.message_id
JOIN "code_session" AS session ON session.id = message.chat_id
WHERE message.turn_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM code_turn_attachment existing
      WHERE existing.turn_id = message.turn_id
        AND existing.message_id = attachment.message_id
        AND existing.blob_id = attachment.blob_id
  );
"#,
            )
            .await?;
        copy.commit().await?;
    }
    if manager.has_table("message_document_attachment").await? {
        let copy = manager.begin().await?;
        copy.get_connection()
            .execute_unprepared(
                r#"
INSERT INTO "code_turn_document_attachment" (
    "turn_id", "ordinal", "owner", "message_id", "document_id", "created_at"
)
SELECT
    message.turn_id,
    (
        SELECT COUNT(*) FROM message_document_attachment earlier
        JOIN message earlier_message ON earlier_message.id = earlier.message_id
        WHERE earlier_message.turn_id = message.turn_id
          AND (earlier.message_id < attachment.message_id
               OR (earlier.message_id = attachment.message_id
                   AND earlier.ordinal <= attachment.ordinal))
    ) - 1,
    session.owner,
    attachment.message_id,
    attachment.document_id,
    attachment.created_at
FROM "message_document_attachment" AS attachment
JOIN "message" ON message.id = attachment.message_id
JOIN "code_session" AS session ON session.id = message.chat_id
WHERE message.turn_id IS NOT NULL
  AND NOT EXISTS (
      SELECT 1 FROM code_turn_document_attachment existing
      WHERE existing.turn_id = message.turn_id
        AND existing.message_id = attachment.message_id
        AND existing.document_id = attachment.document_id
  );
"#,
            )
            .await?;
        copy.commit().await?;
    }

    for table in [
        "turn_run",
        "turn_admission",
        "queued_turn",
        "message_attachment",
        "message_document_attachment",
    ] {
        connection
            .execute_unprepared(&format!(r#"DROP TABLE IF EXISTS "{table}""#))
            .await?;
    }

    let violations = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await?;
    if !violations.is_empty() {
        return Err(DbErr::Custom(format!(
            "one-turn-lane left {} foreign-key violations",
            violations.len()
        )));
    }
    Ok(())
}

fn flatten(create: &str) -> String {
    create.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Keep parent-key column names that still live on `chat_id` while this
/// table's own `chat_id` becomes `session_id`.
fn protect_parent_chat_id(sql: &str) -> String {
    sql.replace(
        r#"REFERENCES "message" ("id", "chat_id", "turn_id")"#,
        r#"REFERENCES "message" ("id", "chat_id_parent", "turn_id")"#,
    )
    .replace(
        r#"REFERENCES "tool_call" ("id", "chat_id", "turn_id")"#,
        r#"REFERENCES "tool_call" ("id", "chat_id_parent", "turn_id")"#,
    )
    .replace(
        r#"REFERENCES "tool_call" ("id", "chat_id", "history_order")"#,
        r#"REFERENCES "tool_call" ("id", "chat_id_parent", "history_order")"#,
    )
    .replace(
        r#"REFERENCES "agent_run" ("id", "origin_turn_id", "parent_id", "chat_id")"#,
        r#"REFERENCES "agent_run" ("id", "origin_turn_id", "parent_id", "chat_id_parent")"#,
    )
}

fn restore_parent_chat_id(sql: &str) -> String {
    sql.replace(r#""chat_id_parent""#, r#""chat_id""#)
}

fn rename_local_chat_id(sql: &str) -> String {
    restore_parent_chat_id(&protect_parent_chat_id(sql).replace(r#""chat_id""#, r#""session_id""#))
}

/// Widen `code_turn` with the lane columns. A definition that already
/// carries `lease_token` comes back unchanged. The rebuild copies the
/// columns both definitions share, so a full CREATE is the retry-safe
/// shape: an interrupted attempt that already rebuilt this table skips
/// it, and one that has not still copies the live columns across.
pub(super) fn rewrite_code_turn(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""lease_token""#) {
        return Ok(create.to_owned());
    }
    let _ = flatten(create);
    Ok(format!(
        r#"CREATE TABLE "code_turn" ("id" uuid_text NOT NULL PRIMARY KEY, "owner" text NOT NULL DEFAULT 'local', "session_id" uuid_text NOT NULL, "ordinal" integer NOT NULL, "status" text NOT NULL, "model" text, "fast_mode" boolean NOT NULL DEFAULT FALSE, "user_input" text NOT NULL, "user_input_blob_id" uuid_text, "checkpoint_ref" text, "diffstat" jsonb_text, "usage" jsonb_text, "narrative" text, "rewrite" text, "started_at" timestamp_with_timezone_text NOT NULL, "ended_at" timestamp_with_timezone_text, "park_ref" text, "park_wait" jsonb_text, "attempt_count" integer NOT NULL DEFAULT 0, "max_attempts" integer NOT NULL DEFAULT 5, "claim_count" integer NOT NULL DEFAULT 0, "model_steps" integer NOT NULL DEFAULT 0, "input_tokens" integer NOT NULL DEFAULT 0, "output_tokens" integer NOT NULL DEFAULT 0, "cache_read_input_tokens" integer NOT NULL DEFAULT 0, "cache_creation_input_tokens" integer NOT NULL DEFAULT 0, "available_at" timestamp_with_timezone_text, "lease_token" uuid_text, "lease_expires_at" timestamp_with_timezone_text, "last_error_code" varchar(128), "last_error_detail" varchar(4096), "steer_revision" integer NOT NULL DEFAULT 0, "last_steer_applied_at" timestamp_with_timezone_text, "invoked_skills" jsonb_text NOT NULL DEFAULT '[]', "voice_input_used" boolean NOT NULL DEFAULT FALSE, "input_message_id" uuid_text, "output_message_id" uuid_text, "updated_at" timestamp_with_timezone_text, "fingerprint" blob, UNIQUE ("session_id", "id"), FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"), FOREIGN KEY ("input_message_id", "session_id", "id") REFERENCES "message" ("id", "chat_id", "turn_id") ON DELETE RESTRICT, FOREIGN KEY ("output_message_id", "session_id", "id") REFERENCES "message" ("id", "chat_id", "turn_id") ON DELETE RESTRICT, FOREIGN KEY ("lease_token", "id", "attempt_count", "claim_count") REFERENCES "code_turn_claim" ("token", "turn_id", "attempt_count", "claim_count") ON DELETE RESTRICT, CHECK ("status" IN ({MERGED_STATUS})), CHECK ("ordinal" >= 1), CHECK ("attempt_count" >= 0 AND "max_attempts" >= 1 AND "attempt_count" <= "max_attempts"), CHECK ("claim_count" >= "attempt_count"), CHECK ("input_message_id" IS NULL OR (("status" IN ('running', 'cancelling') AND "lease_token" IS NOT NULL AND "lease_expires_at" IS NOT NULL) OR ("status" <> 'running' AND "status" <> 'cancelling' AND "lease_token" IS NULL AND "lease_expires_at" IS NULL))), CHECK ("input_message_id" IS NULL OR (("status" IN ('retry_wait', 'failed') AND "last_error_code" IS NOT NULL) OR ("status" NOT IN ('retry_wait', 'failed') AND "last_error_code" IS NULL AND "last_error_detail" IS NULL))), CHECK ("input_message_id" IS NULL OR (("status" IN ('completed', 'failed', 'interrupted', 'cancelled') AND "ended_at" IS NOT NULL) OR ("status" NOT IN ('completed', 'failed', 'interrupted', 'cancelled') AND "ended_at" IS NULL))), CHECK ("steer_revision" >= 0), CHECK (("steer_revision" = 0 AND "last_steer_applied_at" IS NULL) OR ("steer_revision" >= 1 AND "last_steer_applied_at" IS NOT NULL)), CHECK ("input_message_id" IS NULL OR (("status" = 'completed' AND "output_message_id" IS NOT NULL) OR ("status" IN ('interrupted', 'cancelled')) OR ("status" NOT IN ('completed', 'interrupted', 'cancelled') AND "output_message_id" IS NULL))), CHECK ("model" IS NULL OR (LENGTH("model") BETWEEN 1 AND 512)), CHECK ("last_error_code" IS NULL OR (LENGTH("last_error_code") BETWEEN 1 AND 128)), CHECK ("last_error_detail" IS NULL OR (LENGTH("last_error_detail") BETWEEN 1 AND 4096)), CHECK ("model_steps" >= 0 AND "model_steps" <= 2147483647 AND "input_tokens" >= 0 AND "input_tokens" <= 4294967295 AND "output_tokens" >= 0 AND "output_tokens" <= 4294967295 AND "cache_read_input_tokens" >= 0 AND "cache_read_input_tokens" <= 4294967295 AND "cache_creation_input_tokens" >= 0 AND "cache_creation_input_tokens" <= 4294967295) )"#
    ))
}

pub(super) fn rewrite_code_queued_turn(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""file_attachments_json""#) {
        return Ok(create.to_owned());
    }
    let _ = flatten(create);
    Ok(
        r#"CREATE TABLE "code_queued_turn" ("id" uuid_text NOT NULL PRIMARY KEY, "owner" text NOT NULL DEFAULT 'local', "session_id" uuid_text NOT NULL, "message" text NOT NULL, "attachments_json" text NOT NULL DEFAULT '[]', "file_attachments_json" text NOT NULL DEFAULT '[]', "invoked_skills_json" text NOT NULL DEFAULT '[]', "voice_input_used" boolean NOT NULL DEFAULT FALSE, "fingerprint" blob, "position" integer NOT NULL, "created_at" timestamp_with_timezone_text NOT NULL, "updated_at" timestamp_with_timezone_text NOT NULL, FOREIGN KEY ("session_id") REFERENCES "code_session" ("id"), CHECK (LENGTH("message") > 0), CHECK ("position" >= 0) )"#
            .to_owned(),
    )
}

pub(super) fn rewrite_code_turn_attachment(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""message_id""#) {
        return Ok(create.to_owned());
    }
    let flat = flatten(create);
    let Some(idx) = flat.find("PRIMARY KEY") else {
        return Err(DbErr::Custom(
            "SQLite code_turn_attachment definition has an unexpected shape".to_owned(),
        ));
    };
    Ok(format!(
        r#"{} "message_id" uuid_text, {}"#,
        &flat[..idx],
        &flat[idx..]
    ))
}

pub(super) fn rewrite_code_turn_claim(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""owner""#) {
        return Ok(create.to_owned());
    }
    let flat = flatten(create).replace(
        r#"CREATE TABLE "turn_claim""#,
        r#"CREATE TABLE "code_turn_claim""#,
    );
    let Some(paren) = flat.find('(') else {
        return Err(DbErr::Custom(
            "SQLite code_turn_claim definition has an unexpected shape".to_owned(),
        ));
    };
    Ok(format!(
        r#"{} ("owner" text NOT NULL DEFAULT 'local', {}"#,
        &flat[..paren],
        &flat[paren + 1..]
    ))
}

pub(super) fn rewrite_code_turn_steer(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""owner""#) && create.contains(r#"REFERENCES "code_turn""#) {
        return Ok(create.to_owned());
    }
    let flat = rename_local_chat_id(
        &flatten(create)
            .replace(
                r#"CREATE TABLE "turn_steer""#,
                r#"CREATE TABLE "code_turn_steer""#,
            )
            .replace(
                r#"REFERENCES "turn_run" ("chat_id", "id")"#,
                r#"REFERENCES "code_turn" ("session_id", "id")"#,
            )
            .replace(
                r#"REFERENCES "turn_claim""#,
                r#"REFERENCES "code_turn_claim""#,
            ),
    );
    let Some(paren) = flat.find('(') else {
        return Err(DbErr::Custom(
            "SQLite code_turn_steer definition has an unexpected shape".to_owned(),
        ));
    };
    if flat.contains(r#""owner""#) {
        return Ok(flat);
    }
    Ok(format!(
        r#"{} ("owner" text NOT NULL DEFAULT 'local', {}"#,
        &flat[..paren],
        &flat[paren + 1..]
    ))
}

pub(super) fn rewrite_code_turn_failure(create: &str) -> Result<String, DbErr> {
    if create.contains(r#""owner""#) {
        return Ok(create.to_owned());
    }
    let flat = flatten(create)
        .replace(
            r#"CREATE TABLE "turn_failure""#,
            r#"CREATE TABLE "code_turn_failure""#,
        )
        .replace(
            r#"REFERENCES "turn_claim""#,
            r#"REFERENCES "code_turn_claim""#,
        );
    let Some(paren) = flat.find('(') else {
        return Err(DbErr::Custom(
            "SQLite code_turn_failure definition has an unexpected shape".to_owned(),
        ));
    };
    Ok(format!(
        r#"{} ("owner" text NOT NULL DEFAULT 'local', {}"#,
        &flat[..paren],
        &flat[paren + 1..]
    ))
}

pub(super) fn rewrite_turn_references(create: &str) -> Result<String, DbErr> {
    let flat = flatten(create);
    if !flat.contains(r#"REFERENCES "turn_run""#) && !flat.contains(r#"REFERENCES "turn_claim""#) {
        return Ok(create.to_owned());
    }
    Ok(flat
        .replace(
            r#"REFERENCES "turn_run" ("chat_id", "id")"#,
            r#"REFERENCES "code_turn" ("session_id", "id")"#,
        )
        .replace(
            r#"REFERENCES "turn_run" ("id")"#,
            r#"REFERENCES "code_turn" ("id")"#,
        )
        .replace(
            r#"REFERENCES "turn_claim""#,
            r#"REFERENCES "code_turn_claim""#,
        ))
}

pub(super) fn rewrite_sandbox_spawn_checkpoint(create: &str) -> Result<String, DbErr> {
    if !create.contains(r#""chat_id""#) && !create.contains(r#"REFERENCES "turn_claim""#) {
        return Ok(create.to_owned());
    }
    Ok(rename_local_chat_id(
        &flatten(create)
            .replace(
                r#"REFERENCES "turn_claim""#,
                r#"REFERENCES "code_turn_claim""#,
            )
            .replace(r#"REFERENCES "turn_run""#, r#"REFERENCES "code_turn""#),
    ))
}

pub(super) fn rewrite_turn_client_wait(create: &str) -> Result<String, DbErr> {
    if !create.contains(r#""chat_id""#) && !create.contains(r#"REFERENCES "turn_claim""#) {
        return Ok(create.to_owned());
    }
    Ok(rename_local_chat_id(
        &flatten(create)
            .replace(
                r#"REFERENCES "turn_claim""#,
                r#"REFERENCES "code_turn_claim""#,
            )
            .replace(r#"REFERENCES "turn_run""#, r#"REFERENCES "code_turn""#),
    ))
}

pub(super) fn rewrite_turn_agent_run_wait_set(create: &str) -> Result<String, DbErr> {
    let flat = flatten(create);
    if !flat.contains(r#"REFERENCES "turn_run""#) && !flat.contains(r#""chat_id""#) {
        return Ok(create.to_owned());
    }
    Ok(rename_local_chat_id(
        &flat
            .replace(
                r#"FOREIGN KEY ("turn_id", "chat_id", "parent_run_id") REFERENCES "turn_run" ("id", "chat_id", "agent_run_id")"#,
                r#"FOREIGN KEY ("session_id", "turn_id") REFERENCES "code_turn" ("session_id", "id")"#,
            )
            .replace(
                r#"REFERENCES "turn_claim""#,
                r#"REFERENCES "code_turn_claim""#,
            )
            .replace(r#"REFERENCES "turn_run""#, r#"REFERENCES "code_turn""#),
    ))
}

pub(super) fn rewrite_turn_agent_run_wait_member(create: &str) -> Result<String, DbErr> {
    let flat = flatten(create);
    if flat.contains(
        r#"REFERENCES "turn_agent_run_wait_set" ("id", "turn_id", "parent_run_id", "session_id")"#,
    ) {
        return Ok(create.to_owned());
    }
    Ok(flat.replace(
        r#"REFERENCES "turn_agent_run_wait_set" ("id", "turn_id", "parent_run_id", "chat_id")"#,
        r#"REFERENCES "turn_agent_run_wait_set" ("id", "turn_id", "parent_run_id", "session_id")"#,
    ))
}
