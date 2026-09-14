//! Freeze inference separately from session ownership and forge identity.
use sea_orm_migration::prelude::*;
pub(super) struct SessionInference;
impl MigrationName for SessionInference {
    fn name(&self) -> &str {
        "m20260914_000001_session_inference"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for SessionInference {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_session_inference"))
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("selection"))
                            .json_binary()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_inference_session")
                            .from(
                                Alias::new("code_session_inference"),
                                Alias::new("session_id"),
                            )
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_inference_resolution"))
                    .col(
                        ColumnDef::new(Alias::new("root_session_id"))
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("provider")).string().not_null())
                    .col(
                        ColumnDef::new(Alias::new("resolution"))
                            .json_binary()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("root_session_id"))
                            .col(Alias::new("provider")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_inference_resolution_session")
                            .from(
                                Alias::new("code_inference_resolution"),
                                Alias::new("root_session_id"),
                            )
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
