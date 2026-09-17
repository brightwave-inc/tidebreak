//! Named children a parent is actually waiting on.
//!
//! Counts on the session tree are never inferred from every running child.
use sea_orm_migration::prelude::*;

pub(super) struct ParentWait;

impl MigrationName for ParentWait {
    fn name(&self) -> &str {
        "m20260917_000001_code_parent_wait"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ParentWait {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("code_parent_wait"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("parent_session_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("child_ids")).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_parent_wait_parent")
                            .from(
                                Alias::new("code_parent_wait"),
                                Alias::new("parent_session_id"),
                            )
                            .to(Alias::new("session"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("code_parent_wait"))
                    .to_owned(),
            )
            .await
    }
}
