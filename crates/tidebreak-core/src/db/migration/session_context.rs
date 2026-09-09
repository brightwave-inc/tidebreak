//! Retain a conversation's channel and its independently running children.
use sea_orm_migration::prelude::*;

pub(super) struct SessionContext;
impl MigrationName for SessionContext {
    fn name(&self) -> &str {
        "m20260909_000001_session_context"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SessionContext {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_session_context"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("channel_id")).string())
                    .col(ColumnDef::new(Alias::new("parent_session_id")).uuid())
                    .col(ColumnDef::new(Alias::new("request_key")).string())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_context_session")
                            .from(Alias::new("code_session_context"), Alias::new("session_id"))
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_context_parent")
                            .from(
                                Alias::new("code_session_context"),
                                Alias::new("parent_session_id"),
                            )
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .index(
                        Index::create()
                            .name("uq_session_context_request")
                            .col(Alias::new("parent_session_id"))
                            .col(Alias::new("request_key"))
                            .unique(),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
