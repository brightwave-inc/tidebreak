//! Keep human waits separate from tool execution leases.
use sea_orm_migration::prelude::*;

pub(super) struct ManagedDecisions;
impl MigrationName for ManagedDecisions {
    fn name(&self) -> &str {
        "m20260912_000001_managed_decisions"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for ManagedDecisions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Alias::new("code_managed_decision"))
            .if_not_exists()
            .col(
                ColumnDef::new(Alias::new("approval_id"))
                    .uuid()
                    .not_null()
                    .primary_key(),
            );
        for column in [
            "session_id",
            "turn_id",
            "incarnation_id",
            "grant_id",
            "runtime_id",
        ] {
            table.col(ColumnDef::new(Alias::new(column)).uuid().not_null());
        }
        for column in ["owner", "request_id", "tool"] {
            table.col(ColumnDef::new(Alias::new(column)).text().not_null());
        }
        table
            .col(
                ColumnDef::new(Alias::new("native_turn"))
                    .big_integer()
                    .not_null(),
            )
            .col(
                ColumnDef::new(Alias::new("arguments"))
                    .json_binary()
                    .not_null(),
            )
            .col(ColumnDef::new(Alias::new("result")).json_binary());
        for column in ["abandoned", "delivered"] {
            table.col(
                ColumnDef::new(Alias::new(column))
                    .boolean()
                    .not_null()
                    .default(false),
            );
        }
        manager.create_table(table.to_owned()).await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_managed_decision_request")
                    .table(Alias::new("code_managed_decision"))
                    .col(Alias::new("owner"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("incarnation_id"))
                    .col(Alias::new("request_id"))
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
