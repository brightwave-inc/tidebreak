//! `m20260907_000013_session_owner_kind`: label service-owned sessions.
//!
//! A null value means the session belongs to a person, including every row
//! written before this migration. Only service principals write `service`.

use sea_orm_migration::prelude::*;

pub(super) struct SessionOwnerKind;

impl MigrationName for SessionOwnerKind {
    fn name(&self) -> &str {
        "m20260907_000013_session_owner_kind"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for SessionOwnerKind {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("session", "owner_kind").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("session"))
                    .add_column(ColumnDef::new(Alias::new("owner_kind")).string())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The value carries historical attribution, and SQLite cannot drop
        // the column in place. A nullable column costs a rolled-back database
        // nothing.
        Ok(())
    }
}
