//! `m20260908_000016_code_channel_repository_confirm`: admin gate per channel.
//!
//! Under a workspace grant, a channel may not run against a repository until
//! an admin confirms that pair. A later different repository supersedes a
//! pending row.

use sea_orm_migration::prelude::*;

pub(super) struct ChannelRepositoryConfirm;

impl MigrationName for ChannelRepositoryConfirm {
    fn name(&self) -> &str {
        "m20260908_000016_code_channel_repository_confirm"
    }
}

#[derive(DeriveIden)]
enum CodeChannelRepositoryConfirm {
    Table,
    GrantId,
    ChannelId,
    Repository,
    SetByIdentity,
    SetByDisplay,
    State,
    ConfirmedBy,
    CreatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for ChannelRepositoryConfirm {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CodeChannelRepositoryConfirm::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::GrantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::ChannelId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::Repository)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::SetByIdentity)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::SetByDisplay)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::State)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(CodeChannelRepositoryConfirm::ConfirmedBy).text())
                    .col(
                        ColumnDef::new(CodeChannelRepositoryConfirm::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(CodeChannelRepositoryConfirm::GrantId)
                            .col(CodeChannelRepositoryConfirm::ChannelId)
                            .col(CodeChannelRepositoryConfirm::Repository),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_channel_repository_confirm_grant_channel")
                    .table(CodeChannelRepositoryConfirm::Table)
                    .col(CodeChannelRepositoryConfirm::GrantId)
                    .col(CodeChannelRepositoryConfirm::ChannelId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
