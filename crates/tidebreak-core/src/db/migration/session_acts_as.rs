//! `m20260908_000014_session_acts_as`: whose forge identity a session borrows.
//!
//! A null value means the owner-kind default: a service acts as the bot, a
//! person as themselves. The stored choice never changes for the session's
//! life (decision 0090).

use sea_orm_migration::prelude::*;

pub(super) struct SessionActsAs;

impl MigrationName for SessionActsAs {
    fn name(&self) -> &str {
        "m20260908_000014_session_acts_as"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SessionActsAs {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("session", "acts_as").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("session"))
                    .add_column(ColumnDef::new(Alias::new("acts_as")).string())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
