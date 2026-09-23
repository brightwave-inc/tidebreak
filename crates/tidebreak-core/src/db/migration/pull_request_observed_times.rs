//! Record when each live field group on a pull request was observed.
//!
//! Every writer of pull-request state lands its read through one merge, and
//! the merge orders reads one field group at a time: checks, review
//! decision, mergeability, auto-merge, and queue membership. Each group needs
//! the time its stored value was observed, so a late or partial read cannot
//! replace a newer one. Existing rows start with no times, so the first read
//! after the upgrade counts as newer than anything they hold.
use sea_orm_migration::prelude::*;

use super::idens::CodePullRequest;

pub(super) struct PullRequestObservedTimes;

impl MigrationName for PullRequestObservedTimes {
    fn name(&self) -> &str {
        "m20260923_000004_pull_request_observed_times"
    }
}

const COLUMNS: [(&str, CodePullRequest); 5] = [
    ("checks_observed_at", CodePullRequest::ChecksObservedAt),
    ("review_observed_at", CodePullRequest::ReviewObservedAt),
    (
        "mergeability_observed_at",
        CodePullRequest::MergeabilityObservedAt,
    ),
    (
        "auto_merge_observed_at",
        CodePullRequest::AutoMergeObservedAt,
    ),
    ("queue_observed_at", CodePullRequest::QueueObservedAt),
];

#[async_trait::async_trait]
impl MigrationTrait for PullRequestObservedTimes {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite accepts one ADD COLUMN per ALTER, so each column is its own
        // guarded statement.
        for (name, column) in COLUMNS {
            if manager.has_column("code_pull_request", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(CodePullRequest::Table)
                        .add_column(ColumnDef::new(column).timestamp_with_time_zone())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, column) in COLUMNS {
            if !manager.has_column("code_pull_request", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(CodePullRequest::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
