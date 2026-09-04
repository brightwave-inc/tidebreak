//! Add session sharing and durable actor attribution.

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

pub(super) struct SessionAccessActor;

impl MigrationName for SessionAccessActor {
    fn name(&self) -> &str {
        "m20260904_000008_session_access_actor"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SessionAccessActor {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
ALTER TABLE "session" ADD COLUMN "visibility" text NOT NULL DEFAULT 'private'
    CHECK ("visibility" IN ('private', 'deployment'));
ALTER TABLE "turn" ADD COLUMN "actor" json;
ALTER TABLE "code_queued_turn" ADD COLUMN "actor" json;
ALTER TABLE "approval" ADD COLUMN "actor" json;
CREATE TABLE "session_access" (
    "session_id" uuid NOT NULL,
    "subject" text NOT NULL,
    "level" text NOT NULL CHECK ("level" IN ('view', 'contribute')),
    "granted_by" text NOT NULL,
    "created_at" timestamptz NOT NULL,
    PRIMARY KEY ("session_id", "subject"),
    FOREIGN KEY ("session_id") REFERENCES "session" ("id") ON DELETE CASCADE
);
CREATE INDEX "idx_session_access_subject" ON "session_access" ("subject", "session_id");
"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
