//! Record a channel's first-turn context opt-in on its binding.

use sea_orm_migration::prelude::*;

pub(super) struct ExternalThreadContext;

impl MigrationName for ExternalThreadContext {
    fn name(&self) -> &str {
        "m20260908_000017_external_thread_context"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ExternalThreadContext {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("code_external_binding", "context_opt_in")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("code_external_binding"))
                        .add_column(
                            ColumnDef::new(Alias::new("context_opt_in"))
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Keep the historical consent record when rolling back application code.
        Ok(())
    }
}
