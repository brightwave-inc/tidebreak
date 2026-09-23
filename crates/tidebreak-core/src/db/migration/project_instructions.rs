//! Give each project standing instructions for its conversations.
//!
//! The internal engine adds a project's instructions to the system prompt of
//! every conversation filed under it. A project that predates this column has
//! none, so the column is `NOT NULL` with an empty default.
use sea_orm_migration::prelude::*;

use super::idens::Project;

pub(super) struct ProjectInstructions;

impl MigrationName for ProjectInstructions {
    fn name(&self) -> &str {
        "m20260923_000001_project_instructions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for ProjectInstructions {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("project", "instructions").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .add_column(
                        ColumnDef::new(Project::Instructions)
                            .text()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("project", "instructions").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Project::Table)
                    .drop_column(Project::Instructions)
                    .to_owned(),
            )
            .await
    }
}
