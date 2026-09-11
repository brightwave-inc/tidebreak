//! Persist the last setup-script failure output on the workspace row.
use sea_orm_migration::prelude::*;

use super::idens::CodeWorkspace;

pub(super) struct WorkspaceSetupError;

impl MigrationName for WorkspaceSetupError {
    fn name(&self) -> &str {
        "m20260910_000027_workspace_setup_error"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for WorkspaceSetupError {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("code_workspace", "setup_error").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(CodeWorkspace::Table)
                    .add_column(ColumnDef::new(CodeWorkspace::SetupError).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("code_workspace", "setup_error").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(CodeWorkspace::Table)
                    .drop_column(CodeWorkspace::SetupError)
                    .to_owned(),
            )
            .await
    }
}
