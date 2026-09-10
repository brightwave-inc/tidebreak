//! Preserve native tool identities and results across sandbox event replay.
use sea_orm_migration::prelude::*;

pub(super) struct NativeToolReceipts;
impl MigrationName for NativeToolReceipts {
    fn name(&self) -> &str {
        "m20260910_000025_native_tool_receipts"
    }
}
#[derive(DeriveIden)]
enum Receipt {
    #[sea_orm(iden = "code_native_tool_receipt")]
    Table,
    Id,
    Owner,
    SessionId,
    IncarnationId,
    GrantId,
    RequestId,
    CallId,
    Tool,
    Arguments,
    Result,
    Status,
    Delivered,
}
#[async_trait::async_trait]
impl MigrationTrait for NativeToolReceipts {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut table = Table::create();
        table
            .table(Receipt::Table)
            .if_not_exists()
            .col(ColumnDef::new(Receipt::Id).uuid().not_null().primary_key());
        for column in [
            Receipt::SessionId,
            Receipt::IncarnationId,
            Receipt::GrantId,
            Receipt::CallId,
        ] {
            table.col(ColumnDef::new(column).uuid().not_null());
        }
        for column in [
            Receipt::Owner,
            Receipt::RequestId,
            Receipt::Tool,
            Receipt::Status,
        ] {
            table.col(ColumnDef::new(column).text().not_null());
        }
        table
            .col(ColumnDef::new(Receipt::Arguments).json_binary().not_null())
            .col(ColumnDef::new(Receipt::Result).json_binary())
            .col(
                ColumnDef::new(Receipt::Delivered)
                    .boolean()
                    .not_null()
                    .default(false),
            );
        manager.create_table(table.to_owned()).await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_native_tool_receipt_request")
                    .table(Receipt::Table)
                    .col(Receipt::Owner)
                    .col(Receipt::SessionId)
                    .col(Receipt::IncarnationId)
                    .col(Receipt::RequestId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_native_tool_receipt_pending")
                    .table(Receipt::Table)
                    .col(Receipt::Owner)
                    .col(Receipt::SessionId)
                    .col(Receipt::IncarnationId)
                    .col(Receipt::Delivered)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        // Removing receipts could repeat a side effect after rollback.
        Ok(())
    }
}
