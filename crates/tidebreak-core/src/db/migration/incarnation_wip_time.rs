//! Record when an incarnation's last WIP checkpoint was pushed.
//!
//! A remote workspace's Files and diff views read that checkpoint. While the
//! sandbox runs, the supervisor pushes it about once a minute, so the reader
//! needs its age to tell a fresh live checkpoint from a stale one.
use sea_orm_migration::prelude::*;

use super::idens::CodeSessionIncarnation;

pub(super) struct IncarnationWipTime;

impl MigrationName for IncarnationWipTime {
    fn name(&self) -> &str {
        "m20260922_000001_incarnation_wip_time"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for IncarnationWipTime {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("code_session_incarnation", "last_wip_at")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(CodeSessionIncarnation::Table)
                    .add_column(
                        ColumnDef::new(CodeSessionIncarnation::LastWipAt)
                            .timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("code_session_incarnation", "last_wip_at")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(CodeSessionIncarnation::Table)
                    .drop_column(CodeSessionIncarnation::LastWipAt)
                    .to_owned(),
            )
            .await
    }
}
