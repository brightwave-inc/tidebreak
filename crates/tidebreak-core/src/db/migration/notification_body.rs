//! Give each agent notification a body under its title.
//!
//! The body is one line: why the turn failed, or how the agent's closing
//! message began. Rows written before this column have none, so it is
//! nullable rather than defaulted.
use sea_orm_migration::prelude::*;

use super::idens::Notification;

pub(super) struct NotificationBody;

impl MigrationName for NotificationBody {
    fn name(&self) -> &str {
        "m20260923_000003_notification_body"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for NotificationBody {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("notification", "body").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Notification::Table)
                    .add_column(ColumnDef::new(Notification::Body).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("notification", "body").await? {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Notification::Table)
                    .drop_column(Notification::Body)
                    .to_owned(),
            )
            .await
    }
}
