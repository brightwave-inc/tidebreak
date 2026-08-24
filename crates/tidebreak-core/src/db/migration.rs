//! The database schema: a baseline, then an ordered chain.
//!
//! [`Baseline`] describes a fresh database. Anything that has to reach a
//! database the baseline already ran against is an appended migration in
//! [`Migrator::migrations`], in order.
//!
//! The ordering is the point. SeaORM records one name per migration and cannot
//! tell that the statements behind an already-recorded name changed, so an
//! in-place baseline edit reaches a fresh database and no existing one. The
//! desktop profile used to survive that by being deleted and rebuilt. It is
//! not deleted any more, and the self-host PostgreSQL store never was, so an
//! appended migration is the only thing that reaches either of them.
//! [`Baseline`] is frozen; `the_schema_baseline_is_pinned` fails on an edit.
//!
//! The two owner migrations below used to be branches inside `Baseline::up`,
//! each guarded by whether its column existed yet. One such branch works.
//! Several do not: the guard is the only thing saying what runs when, so
//! nothing records the order, and a fresh database and an upgraded one can
//! drift apart with no test to notice. That is what
//! `a_stepwise_upgrade_lands_on_the_fresh_schema` notices now.
//!
//! Squash this chain into a single clean snapshot before `1.0.0`, so that
//! release's first migration is a baseline rather than this development
//! history.

mod baseline;
mod idens;

#[cfg(test)]
pub(crate) use baseline::tables_for_test;

use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};
use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(Baseline),
            Box::new(AppOwner),
            Box::new(CodeOwner),
            Box::new(BaselineRepair),
            Box::new(CodeSessionFastMode),
            Box::new(CodeSessionImages),
            Box::new(TriggerFireOutbox),
            Box::new(TriggerDeliveryReceipts),
            Box::new(CodePullRequestFacts),
            Box::new(TriggerFactConditions),
            Box::new(CodeQueuedTurns),
            Box::new(CodePullRequestLiveTier),
            Box::new(CodePullRequestEtags),
            Box::new(CodeTurnModelSnapshot),
        ]
    }
}

/// Model and service-tier identity captured when a code turn starts.
///
/// Sessions may change either setting between turns. Keeping the snapshot on
/// the usage row lets analytics price new turns without rewriting older turns
/// from a session's latest selection.
struct CodeTurnModelSnapshot;

impl MigrationName for CodeTurnModelSnapshot {
    fn name(&self) -> &str {
        "m20260824_000014_code_turn_model_snapshot"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeTurnModelSnapshot {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("code_turn", "model").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeTurn::Table)
                        .add_column(ColumnDef::new(idens::CodeTurn::Model).text())
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("code_turn", "fast_mode").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeTurn::Table)
                        .add_column(
                            ColumnDef::new(idens::CodeTurn::FastMode)
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
        // The columns contain historical attribution. A downgrade keeps them.
        Ok(())
    }
}

/// Conditional-request ETags on pull-request facts (decision 66).
///
/// The row stores the ETag each fetch endpoint last answered with — the
/// pull request, its head's check runs, and its reviews — so the next read
/// sends `If-None-Match` and an unchanged answer costs a free 304.
struct CodePullRequestEtags;

impl MigrationName for CodePullRequestEtags {
    fn name(&self) -> &str {
        "m20260824_000013_code_pull_request_etags"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodePullRequestEtags {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite accepts one ADD COLUMN per ALTER, so each column is its own
        // guarded statement.
        for (name, column) in [
            ("pull_etag", idens::CodePullRequest::PullEtag),
            ("checks_etag", idens::CodePullRequest::ChecksEtag),
            ("reviews_etag", idens::CodePullRequest::ReviewsEtag),
        ] {
            if manager.has_column("code_pull_request", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodePullRequest::Table)
                        .add_column(ColumnDef::new(column).text())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse: nullable columns on a table whose own
        // migration's `down` drops it outright.
        Ok(())
    }
}

/// The live tier on pull-request facts (decision 66).
///
/// `code_pull_request` gains the volatile fields a digest read observes:
/// check rollup, review decision, mergeability, merge state, auto-merge
/// arming, queue membership, and when a read last wrote them. All nullable:
/// a row no live read has touched simply has no tier.
struct CodePullRequestLiveTier;

impl MigrationName for CodePullRequestLiveTier {
    fn name(&self) -> &str {
        "m20260824_000012_code_pull_request_live_tier"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodePullRequestLiveTier {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite accepts one ADD COLUMN per ALTER, so each live column is
        // its own guarded statement.
        for (name, column) in [
            ("checks_summary", idens::CodePullRequest::ChecksSummary),
            ("checks", idens::CodePullRequest::Checks),
            ("review_decision", idens::CodePullRequest::ReviewDecision),
            ("mergeable", idens::CodePullRequest::Mergeable),
            (
                "merge_state_status",
                idens::CodePullRequest::MergeStateStatus,
            ),
        ] {
            if manager.has_column("code_pull_request", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodePullRequest::Table)
                        .add_column(ColumnDef::new(column).text())
                        .to_owned(),
                )
                .await?;
        }
        for (name, column) in [
            (
                "auto_merge_enabled",
                idens::CodePullRequest::AutoMergeEnabled,
            ),
            ("in_merge_queue", idens::CodePullRequest::InMergeQueue),
        ] {
            if manager.has_column("code_pull_request", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodePullRequest::Table)
                        .add_column(ColumnDef::new(column).boolean())
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("code_pull_request", "live_observed_at")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodePullRequest::Table)
                        .add_column(
                            ColumnDef::new(idens::CodePullRequest::LiveObservedAt)
                                .timestamp_with_time_zone(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse: the columns belong to `code_pull_request`,
        // whose own migration's `down` drops the table outright, and
        // nullable columns cost a rolled-back database nothing.
        Ok(())
    }
}

/// Durable pull-request facts and workspace attribution (decision 62).
///
/// `code_pull_request` records confirmed observations of pull requests,
/// keyed by full repository identity so a pull request in a repository with
/// no local checkout is representable. `code_pull_request_attribution` ties
/// a workspace to a pull request it authored or contributed to. `code_repo`
/// gains nullable origin identity columns so facts join to local
/// repositories without a git subprocess per read.
struct CodePullRequestFacts;

impl MigrationName for CodePullRequestFacts {
    fn name(&self) -> &str {
        "m20260822_000009_code_pull_request_facts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodePullRequestFacts {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite accepts one ADD COLUMN per ALTER, so each origin column is
        // its own guarded statement.
        for (name, column) in [
            ("origin_host", idens::CodeRepo::OriginHost),
            ("origin_owner", idens::CodeRepo::OriginOwner),
            ("origin_name", idens::CodeRepo::OriginName),
        ] {
            if manager.has_column("code_repo", name).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeRepo::Table)
                        .add_column(ColumnDef::new(column).text())
                        .to_owned(),
                )
                .await?;
        }

        manager
            .create_table(
                Table::create()
                    .table(idens::CodePullRequest::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Host)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::RepoOwner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::RepoName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Number)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Url)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Title)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::State)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::Draft)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(idens::CodePullRequest::Author).text())
                    .col(
                        ColumnDef::new(idens::CodePullRequest::HeadBranch)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::BaseBranch)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodePullRequest::HeadSha).text())
                    .col(
                        ColumnDef::new(idens::CodePullRequest::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::MergedAt).timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::ClosedAt).timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::FirstSeenAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequest::LastSeenAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(idens::CodePullRequest::Number).gte(1))
                    .check(
                        Expr::col(idens::CodePullRequest::State)
                            .is_in(["open", "merged", "closed"]),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .unique()
                    .name("uq_code_pull_request_identity")
                    .table(idens::CodePullRequest::Table)
                    .col(idens::CodePullRequest::Owner)
                    .col(idens::CodePullRequest::Host)
                    .col(idens::CodePullRequest::RepoOwner)
                    .col(idens::CodePullRequest::RepoName)
                    .col(idens::CodePullRequest::Number)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_code_pull_request_owner_updated")
                    .table(idens::CodePullRequest::Table)
                    .col(idens::CodePullRequest::Owner)
                    .col(idens::CodePullRequest::UpdatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(idens::CodePullRequestAttribution::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::PullRequestId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::WorkspaceId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::Relation)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::DiscoveredVia)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodePullRequestAttribution::SessionId).uuid())
                    .col(ColumnDef::new(idens::CodePullRequestAttribution::ParentCallId).text())
                    .col(
                        ColumnDef::new(idens::CodePullRequestAttribution::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(idens::CodePullRequestAttribution::PullRequestId)
                            .col(idens::CodePullRequestAttribution::WorkspaceId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_pull_request_attribution_pr")
                            .from(
                                idens::CodePullRequestAttribution::Table,
                                idens::CodePullRequestAttribution::PullRequestId,
                            )
                            .to(idens::CodePullRequest::Table, idens::CodePullRequest::Id),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_pull_request_attribution_workspace")
                            .from(
                                idens::CodePullRequestAttribution::Table,
                                idens::CodePullRequestAttribution::WorkspaceId,
                            )
                            .to(idens::CodeWorkspace::Table, idens::CodeWorkspace::Id),
                    )
                    .check(
                        Expr::col(idens::CodePullRequestAttribution::Relation)
                            .is_in(["authored", "contributed"]),
                    )
                    .check(
                        Expr::col(idens::CodePullRequestAttribution::DiscoveredVia)
                            .is_in(["command", "reconcile"]),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_code_pull_request_attribution_workspace")
                    .table(idens::CodePullRequestAttribution::Table)
                    .col(idens::CodePullRequestAttribution::WorkspaceId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The `code_repo` origin columns stay: SQLite cannot drop them in
        // place, and nullable columns cost a rolled-back database nothing.
        manager
            .drop_table(
                Table::drop()
                    .table(idens::CodePullRequestAttribution::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(idens::CodePullRequest::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

/// Persist the acceptance boundary for every trigger delivery sink.
struct TriggerDeliveryReceipts;

impl MigrationName for TriggerDeliveryReceipts {
    fn name(&self) -> &str {
        "m20260822_000008_trigger_delivery_receipts"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TriggerDeliveryReceipts {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeTriggerDeliveryReceipt::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::DeliveryId)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::Owner)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::Sink)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeTriggerDeliveryReceipt::TurnId).uuid())
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::AcceptanceToken)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeTriggerDeliveryReceipt::AcceptedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(
                        Expr::col(idens::CodeTriggerDeliveryReceipt::DeliveryId)
                            .ne(uuid::Uuid::nil()),
                    )
                    .check(
                        Expr::col(idens::CodeTriggerDeliveryReceipt::AcceptanceToken)
                            .ne(uuid::Uuid::nil()),
                    )
                    .check(Expr::col(idens::CodeTriggerDeliveryReceipt::Sink).is_in([
                        "turn",
                        "steer",
                        "attention",
                    ]))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(idens::CodeTriggerDeliveryReceipt::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

struct Baseline;

impl MigrationName for Baseline {
    fn name(&self) -> &str {
        "m20260814_000001_baseline"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Baseline {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // A self-host database that recorded the pre-squash chain already
        // holds these tables under a different migration name, so
        // `CREATE TABLE` would fail. Leave its rows alone and let the
        // migrations after this one bring it forward.
        if manager.has_table("app").await? {
            return Ok(());
        }
        for entry in baseline::tables() {
            manager.create_table(entry.table).await?;
            for index in entry.indexes {
                manager.create_index(index).await?;
            }
        }
        for seed in baseline::SEED_STATEMENTS {
            manager.get_connection().execute_unprepared(seed).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse creation order, so no table is dropped while another still
        // references it.
        for entry in baseline::tables().into_iter().rev() {
            let name = entry
                .table
                .get_table_name()
                .expect("every baseline table statement names its table")
                .clone();
            manager
                .drop_table(Table::drop().table(name).to_owned())
                .await?;
        }
        Ok(())
    }
}

/// Unfolded from the pre-public `add_app_owner` upgrade. A self-host database
/// that recorded the August 4 baseline may still lack the column; the current
/// baseline declares it, so on a fresh database this is a no-op.
struct AppOwner;

impl MigrationName for AppOwner {
    fn name(&self) -> &str {
        "m20260814_000002_app_owner"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AppOwner {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("app", "owner").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::App::Table)
                        .add_column(
                            ColumnDef::new(idens::App::Owner)
                                .text()
                                .not_null()
                                .default("local"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_app_owner_updated")
                    .table(idens::App::Table)
                    .col(idens::App::Owner)
                    .col(idens::App::UpdatedAt)
                    .col(idens::App::Id)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse: the baseline declares this column and index, so
        // dropping them would leave a fresh database short of its own
        // snapshot. `Baseline::down` drops the table outright.
        Ok(())
    }
}

/// Owner columns for the code-mode tables, for self-host databases that
/// recorded a baseline written before code mode joined the owner-scoped
/// regime (decision 47, decision 48 step 1). Every row on such a database
/// belongs to the local owner, which is what the column default says.
///
/// The owner-scoped repository index is created here too. A database written
/// under the older baseline also carries a unique constraint on
/// `code_repo.root_path` alone, which SQLite cannot drop in place; on those
/// databases two owners cannot register the same path until the store is
/// recreated. Fresh databases get the per-owner uniqueness the baseline
/// declares.
struct CodeOwner;

impl MigrationName for CodeOwner {
    fn name(&self) -> &str {
        "m20260814_000003_code_owner"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeOwner {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        const CODE_TABLES: [&str; 7] = [
            "code_repo",
            "code_workspace",
            "code_session",
            "code_turn",
            "code_turn_attachment",
            "code_event",
            "code_approval",
        ];
        for name in CODE_TABLES {
            if !manager.has_table(name).await? {
                continue;
            }
            if manager.has_column(name, "owner").await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(name))
                        .add_column(
                            ColumnDef::new(idens::CodeWorkspace::Owner)
                                .text()
                                .not_null()
                                .default("local"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if manager.has_table("code_workspace").await? {
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name("idx_code_workspace_owner_created")
                        .table(idens::CodeWorkspace::Table)
                        .col(idens::CodeWorkspace::Owner)
                        .col(idens::CodeWorkspace::CreatedAt)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse, for the reason on `AppOwner::down`.
        Ok(())
    }
}

/// Repair the known gap for a self-host database that predates the frozen
/// baseline. Such a database makes [`Baseline`] return when it sees `app`, so
/// tables added to the baseline later never reached it. Create every missing
/// baseline table and index in dependency order. This repairs additive schema
/// changes; changed columns still require their own ordered migration.
struct BaselineRepair;

impl MigrationName for BaselineRepair {
    fn name(&self) -> &str {
        "m20260822_000004_baseline_repair"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for BaselineRepair {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for entry in baseline::tables() {
            let name = entry
                .table
                .get_table_name()
                .expect("every baseline table statement names its table")
                .sea_orm_table()
                .to_string();
            if manager.has_table(&name).await? {
                continue;
            }
            let mut table = entry.table;
            table.if_not_exists();
            manager.create_table(table).await?;
            for mut index in entry.indexes {
                index.if_not_exists();
                manager.create_index(index).await?;
            }
        }
        for seed in baseline::SEED_STATEMENTS {
            manager.get_connection().execute_unprepared(seed).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The repair only creates baseline objects that were missing. Removing
        // them would also remove objects owned by the baseline on a fresh
        // database.
        Ok(())
    }
}

struct CodeSessionFastMode;

impl MigrationName for CodeSessionFastMode {
    fn name(&self) -> &str {
        "m20260822_000005_code_session_fast_mode"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeSessionFastMode {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("code_session").await? {
            return Ok(());
        }
        if manager.has_column("code_session", "fast_mode").await? {
            return Ok(());
        }
        // NOT NULL with a false default rather than a nullable tri-state:
        // fast mode is a spend switch, off is what every engine does unasked,
        // and so every session that predates this column was not in it. There
        // is no "no opinion" to record, unlike the effort column beside it.
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("code_session"))
                    .add_column(
                        ColumnDef::new(idens::CodeSession::FastMode)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse: the column belongs to `code_session`, and
        // `Baseline::down` drops that table outright. Unlike `AppOwner`, the
        // frozen baseline does not declare this column, so a rolled-back
        // database gets it again from this migration's `up` on the way back.
        Ok(())
    }
}

/// Bring databases created by v0.60.0 to the image schema that v0.61.0 added
/// to the baseline. Desktop SQLite normally took the last epoch reset for this
/// change, but durable PostgreSQL databases did not.
struct CodeSessionImages;

impl MigrationName for CodeSessionImages {
    fn name(&self) -> &str {
        "m20260822_000006_code_session_images"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeSessionImages {
    fn use_transaction(&self) -> Option<bool> {
        // SQLite table replacement must either retain every legacy row and
        // install the migration record, or leave the old table untouched.
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        ensure_code_turn_attachment_dimensions(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Nothing to reverse: the baseline declares this table and these
        // columns. `Baseline::down` drops the tables during a full refresh.
        Ok(())
    }
}

const LEGACY_UNKNOWN_IMAGE_DIMENSION: i32 = 1;

async fn ensure_code_turn_attachment_dimensions(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    const TABLE: &str = "code_turn_attachment";

    if !manager.has_table(TABLE).await? {
        return Err(DbErr::Custom(
            "baseline repair did not create code_turn_attachment".to_owned(),
        ));
    }

    let has_width = manager.has_column(TABLE, "width").await?;
    let has_height = manager.has_column(TABLE, "height").await?;
    if has_width && has_height {
        return Ok(());
    }

    match manager.get_database_backend() {
        DbBackend::Sqlite => {
            rebuild_sqlite_code_turn_attachment(manager, has_width, has_height).await
        }
        DbBackend::Postgres => {
            add_postgres_code_turn_attachment_dimensions(manager, has_width, has_height).await
        }
        backend => Err(DbErr::Custom(format!(
            "code session image migration does not support {}",
            backend.as_str()
        ))),
    }
}

async fn rebuild_sqlite_code_turn_attachment(
    manager: &SchemaManager<'_>,
    has_width: bool,
    has_height: bool,
) -> Result<(), DbErr> {
    const TABLE: &str = "code_turn_attachment";
    const TEMP_TABLE: &str = "code_turn_attachment_v061_upgrade";

    let entry = baseline::code_turn_attachment();
    let create = entry.table.to_string(SqliteQueryBuilder).replacen(
        &format!("\"{TABLE}\""),
        &format!("\"{TEMP_TABLE}\""),
        1,
    );
    manager.get_connection().execute_unprepared(&create).await?;

    // v0.60.0 retained the blob, media type, and byte length, but not image
    // dimensions. The smallest valid dimension preserves every row and keeps
    // the descriptor readable without inventing a larger layout.
    let width = if has_width { "\"width\"" } else { "1" };
    let height = if has_height { "\"height\"" } else { "1" };
    manager
        .get_connection()
        .execute_unprepared(&format!(
            "INSERT INTO \"{TEMP_TABLE}\" (\"owner\", \"turn_id\", \"ordinal\", \
             \"blob_id\", \"media_type\", \"width\", \"height\", \"byte_len\") \
             SELECT \"owner\", \"turn_id\", \"ordinal\", \"blob_id\", \"media_type\", \
             {width}, {height}, \"byte_len\" FROM \"{TABLE}\""
        ))
        .await?;
    manager
        .drop_table(Table::drop().table(Alias::new(TABLE)).to_owned())
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(Alias::new(TEMP_TABLE), Alias::new(TABLE))
                .to_owned(),
        )
        .await?;
    for index in entry.indexes {
        manager.create_index(index).await?;
    }
    Ok(())
}

async fn add_postgres_code_turn_attachment_dimensions(
    manager: &SchemaManager<'_>,
    has_width: bool,
    has_height: bool,
) -> Result<(), DbErr> {
    for (missing, column) in [(!has_width, "width"), (!has_height, "height")] {
        if !missing {
            continue;
        }
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE \"code_turn_attachment\" \
                 ADD COLUMN \"{column}\" integer NOT NULL DEFAULT {LEGACY_UNKNOWN_IMAGE_DIMENSION} \
                 CHECK (\"{column}\" BETWEEN 1 AND {})",
                crate::image::MAX_IMAGE_DIMENSION
            ))
            .await?;
        manager
            .get_connection()
            .execute_unprepared(&format!(
                "ALTER TABLE \"code_turn_attachment\" \
                 ALTER COLUMN \"{column}\" DROP DEFAULT"
            ))
            .await?;
    }
    Ok(())
}

/// Turn trigger fire fingerprints into a durable delivery outbox.
///
/// The preceding schema suppresses a second sweep as soon as a row exists, so
/// a process crash can strand an undelivered side effect forever. This rebuild
/// gives every row a stable delivery id, explicit state, and a fenced lease.
struct TriggerFireOutbox;

impl MigrationName for TriggerFireOutbox {
    fn name(&self) -> &str {
        "m20260822_000007_trigger_fire_outbox"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TriggerFireOutbox {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        rebuild_code_trigger_fire_outbox(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The durable delivery state cannot be collapsed back to a fingerprint
        // without losing pending work.
        Ok(())
    }
}

const TRIGGER_FIRE_OUTBOX_TEMP_TABLE: &str = "code_trigger_fire_outbox_upgrade";

/// The condition tokens the pre-fact-edge schema accepted (decision 60).
const BASELINE_TRIGGER_CONDITIONS: &[&str] = &[
    "checks_failed",
    "conflicts",
    "changes_requested",
    "review_required",
    "behind",
    "ready_to_merge",
    "merged",
    "closed",
];

/// The condition tokens including the fact edges (decision 62).
const FACT_TRIGGER_CONDITIONS: &[&str] = &[
    "checks_failed",
    "conflicts",
    "changes_requested",
    "review_required",
    "behind",
    "ready_to_merge",
    "merged",
    "closed",
    "pr_opened",
    "pr_updated",
];

/// Accept the fact-edge conditions `pr_opened` and `pr_updated` (decision 62).
///
/// Both condition CHECKs are table-level — `code_trigger.condition` from the
/// frozen baseline and `code_trigger_fire.delivery_condition` from the outbox
/// rebuild — and SQLite cannot alter a table-level CHECK in place, so both
/// tables rebuild. The order matters on PostgreSQL, which enforces the
/// fire→trigger cascade: the new fire table references the new trigger table
/// before either old table drops, and nothing is ever dropped while a
/// foreign key still points at it. SQLite rewrites the new fire table's
/// reference when the new trigger table takes its final name.
struct TriggerFactConditions;

impl MigrationName for TriggerFactConditions {
    fn name(&self) -> &str {
        "m20260823_000010_trigger_fact_conditions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TriggerFactConditions {
    fn use_transaction(&self) -> Option<bool> {
        // Either both rebuilt tables land with every row, or neither does.
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        widen_trigger_condition_checks(manager).await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Narrowing the vocabulary would refuse rows that already exist.
        Ok(())
    }
}

const TRIGGER_WIDEN_TEMP_TABLE: &str = "code_trigger_widen_upgrade";
const TRIGGER_FIRE_WIDEN_TEMP_TABLE: &str = "code_trigger_fire_widen_upgrade";

fn code_trigger_widened_table(name: &str, conditions: &[&str]) -> TableCreateStatement {
    Table::create()
        .table(Alias::new(name))
        .col(
            ColumnDef::new(idens::CodeTrigger::Id)
                .uuid()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(idens::CodeTrigger::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .col(ColumnDef::new(idens::CodeTrigger::RepoId).uuid().not_null())
        .col(
            ColumnDef::new(idens::CodeTrigger::Condition)
                .text()
                .not_null(),
        )
        .col(ColumnDef::new(idens::CodeTrigger::Action).text().not_null())
        .col(
            ColumnDef::new(idens::CodeTrigger::Enabled)
                .boolean()
                .not_null()
                .default(true),
        )
        .col(
            ColumnDef::new(idens::CodeTrigger::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTrigger::UpdatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_trigger_repo")
                .from(Alias::new(name), idens::CodeTrigger::RepoId)
                .to(idens::CodeRepo::Table, idens::CodeRepo::Id),
        )
        .check(Expr::col(idens::CodeTrigger::Condition).is_in(conditions.iter().copied()))
        .check(Expr::col(idens::CodeTrigger::Action).is_in(["deliver", "notify"]))
        .to_owned()
}

async fn widen_trigger_condition_checks(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    for (table, temp) in [
        ("code_trigger", TRIGGER_WIDEN_TEMP_TABLE),
        ("code_trigger_fire", TRIGGER_FIRE_WIDEN_TEMP_TABLE),
    ] {
        if !manager.has_table(table).await? {
            return Err(DbErr::Custom(format!(
                "the migration chain did not create {table}"
            )));
        }
        if manager.has_table(temp).await? {
            manager
                .drop_table(Table::drop().table(Alias::new(temp)).to_owned())
                .await?;
        }
    }

    manager
        .create_table(code_trigger_widened_table(
            TRIGGER_WIDEN_TEMP_TABLE,
            FACT_TRIGGER_CONDITIONS,
        ))
        .await?;
    let copy_triggers = "INSERT INTO {temp} \
         (id, owner, repo_id, condition, action, enabled, created_at, updated_at) \
         SELECT id, owner, repo_id, condition, action, enabled, created_at, updated_at \
         FROM code_trigger"
        .replace("{temp}", TRIGGER_WIDEN_TEMP_TABLE);
    manager
        .get_connection()
        .execute_unprepared(&copy_triggers)
        .await?;

    manager
        .create_table(code_trigger_fire_outbox_table(
            TRIGGER_FIRE_WIDEN_TEMP_TABLE,
            TRIGGER_WIDEN_TEMP_TABLE,
            FACT_TRIGGER_CONDITIONS,
        ))
        .await?;
    let copy_fires = "INSERT INTO {temp} \
         (owner, trigger_id, workspace_id, pr_number, head_sha, fired_at, delivery_id, \
          delivery_condition, delivery_action, delivery_message, state, attempt_count, \
          lease_token, lease_expires_at, next_attempt_at, last_error, delivered_at, \
          cancelled_at) \
         SELECT owner, trigger_id, workspace_id, pr_number, head_sha, fired_at, delivery_id, \
          delivery_condition, delivery_action, delivery_message, state, attempt_count, \
          lease_token, lease_expires_at, next_attempt_at, last_error, delivered_at, \
          cancelled_at \
         FROM code_trigger_fire"
        .replace("{temp}", TRIGGER_FIRE_WIDEN_TEMP_TABLE);
    manager
        .get_connection()
        .execute_unprepared(&copy_fires)
        .await?;

    // Old fire first (nothing references it), then the old trigger table,
    // which by now has zero inbound foreign keys on either backend.
    manager
        .drop_table(
            Table::drop()
                .table(Alias::new("code_trigger_fire"))
                .to_owned(),
        )
        .await?;
    manager
        .drop_table(Table::drop().table(Alias::new("code_trigger")).to_owned())
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(
                    Alias::new(TRIGGER_WIDEN_TEMP_TABLE),
                    Alias::new("code_trigger"),
                )
                .to_owned(),
        )
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(
                    Alias::new(TRIGGER_FIRE_WIDEN_TEMP_TABLE),
                    Alias::new("code_trigger_fire"),
                )
                .to_owned(),
        )
        .await?;

    manager
        .create_index(
            Index::create()
                .name("idx_code_trigger_repo")
                .table(idens::CodeTrigger::Table)
                .col(idens::CodeTrigger::RepoId)
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .unique()
                .name("uq_code_trigger_rule")
                .table(idens::CodeTrigger::Table)
                .col(idens::CodeTrigger::Owner)
                .col(idens::CodeTrigger::RepoId)
                .col(idens::CodeTrigger::Condition)
                .to_owned(),
        )
        .await?;
    for index in code_trigger_fire_outbox_indexes() {
        manager.create_index(index).await?;
    }
    Ok(())
}

fn trigger_fire_uuid_expr(
    row: &QueryResult,
    backend: DbBackend,
    column: &str,
) -> Result<SimpleExpr, DbErr> {
    if backend == DbBackend::Sqlite {
        let value = row.try_get::<String>("", column)?;
        let value = value.parse::<uuid::Uuid>().map_err(|error| {
            DbErr::Custom(format!("invalid code_trigger_fire.{column}: {error}"))
        })?;
        return Ok(Expr::value(value.to_string()));
    }

    Ok(Expr::value(row.try_get::<uuid::Uuid>("", column)?))
}

async fn rebuild_code_trigger_fire_outbox(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    const TABLE: &str = "code_trigger_fire";

    if !manager.has_table(TABLE).await? {
        return Err(DbErr::Custom(
            "baseline repair did not create code_trigger_fire".to_owned(),
        ));
    }

    manager
        .create_table(code_trigger_fire_outbox_table(
            TRIGGER_FIRE_OUTBOX_TEMP_TABLE,
            "code_trigger",
            BASELINE_TRIGGER_CONDITIONS,
        ))
        .await?;

    let backend = manager.get_database_backend();
    let rows = manager
        .get_connection()
        .query_all_raw(Statement::from_string(
            backend,
            "SELECT owner, trigger_id, workspace_id, pr_number, head_sha, fired_at \
             FROM code_trigger_fire"
                .to_owned(),
        ))
        .await?;
    for row in rows {
        let owner = row.try_get::<String>("", "owner")?;
        let trigger_id = trigger_fire_uuid_expr(&row, backend, "trigger_id")?;
        let workspace_id = trigger_fire_uuid_expr(&row, backend, "workspace_id")?;
        let pr_number = row.try_get::<i64>("", "pr_number")?;
        let head_sha = row.try_get::<String>("", "head_sha")?;
        let fired_at = row.try_get::<chrono::DateTime<chrono::Utc>>("", "fired_at")?;
        let delivery_id = uuid::Uuid::new_v4();
        let insert = Query::insert()
            .into_table(Alias::new(TRIGGER_FIRE_OUTBOX_TEMP_TABLE))
            .columns([
                idens::CodeTriggerFire::Owner,
                idens::CodeTriggerFire::TriggerId,
                idens::CodeTriggerFire::WorkspaceId,
                idens::CodeTriggerFire::PrNumber,
                idens::CodeTriggerFire::HeadSha,
                idens::CodeTriggerFire::FiredAt,
                idens::CodeTriggerFire::DeliveryId,
                idens::CodeTriggerFire::DeliveryCondition,
                idens::CodeTriggerFire::DeliveryAction,
                idens::CodeTriggerFire::DeliveryMessage,
                idens::CodeTriggerFire::State,
                idens::CodeTriggerFire::AttemptCount,
                idens::CodeTriggerFire::LeaseToken,
                idens::CodeTriggerFire::LeaseExpiresAt,
                idens::CodeTriggerFire::NextAttemptAt,
                idens::CodeTriggerFire::LastError,
                idens::CodeTriggerFire::DeliveredAt,
                idens::CodeTriggerFire::CancelledAt,
            ])
            .values_panic([
                Expr::value(owner),
                trigger_id,
                workspace_id,
                Expr::value(pr_number),
                Expr::value(head_sha),
                Expr::value(fired_at),
                Expr::value(delivery_id),
                Expr::value(Option::<String>::None),
                Expr::value(Option::<String>::None),
                Expr::value(Option::<String>::None),
                Expr::value("delivered"),
                Expr::value(1_i64),
                Expr::value(Option::<uuid::Uuid>::None),
                Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                Expr::value(Option::<String>::None),
                Expr::value(Some(fired_at)),
                Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            ])
            .to_owned();
        manager.get_connection().execute(&insert).await?;
    }

    manager
        .drop_table(Table::drop().table(Alias::new(TABLE)).to_owned())
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(
                    Alias::new(TRIGGER_FIRE_OUTBOX_TEMP_TABLE),
                    Alias::new(TABLE),
                )
                .to_owned(),
        )
        .await?;
    for index in code_trigger_fire_outbox_indexes() {
        manager.create_index(index).await?;
    }
    Ok(())
}

fn code_trigger_fire_outbox_table(
    name: &str,
    trigger_table: &str,
    conditions: &[&str],
) -> TableCreateStatement {
    let pending = Expr::col(idens::CodeTriggerFire::State)
        .eq("pending")
        .and(Expr::col(idens::CodeTriggerFire::DeliveredAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::CancelledAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::NextAttemptAt).is_not_null());
    let delivered = Expr::col(idens::CodeTriggerFire::State)
        .eq("delivered")
        .and(Expr::col(idens::CodeTriggerFire::DeliveredAt).is_not_null())
        .and(Expr::col(idens::CodeTriggerFire::CancelledAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::LeaseToken).is_null())
        .and(Expr::col(idens::CodeTriggerFire::LeaseExpiresAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::NextAttemptAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::LastError).is_null());
    let cancelled = Expr::col(idens::CodeTriggerFire::State)
        .eq("cancelled")
        .and(Expr::col(idens::CodeTriggerFire::DeliveredAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::CancelledAt).is_not_null())
        .and(Expr::col(idens::CodeTriggerFire::LeaseToken).is_null())
        .and(Expr::col(idens::CodeTriggerFire::LeaseExpiresAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::NextAttemptAt).is_null())
        .and(Expr::col(idens::CodeTriggerFire::LastError).is_null());
    let has_delivery_payload = Expr::col(idens::CodeTriggerFire::DeliveryCondition)
        .is_not_null()
        .and(Expr::col(idens::CodeTriggerFire::DeliveryAction).is_not_null())
        .and(Expr::col(idens::CodeTriggerFire::DeliveryMessage).is_not_null());
    let no_lease = Expr::col(idens::CodeTriggerFire::LeaseToken)
        .is_null()
        .and(Expr::col(idens::CodeTriggerFire::LeaseExpiresAt).is_null());
    let active_lease = Expr::col(idens::CodeTriggerFire::LeaseToken)
        .is_not_null()
        .and(Expr::col(idens::CodeTriggerFire::LeaseExpiresAt).is_not_null());

    Table::create()
        .table(Alias::new(name))
        .col(
            ColumnDef::new(idens::CodeTriggerFire::Owner)
                .text()
                .not_null()
                .default("local"),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::TriggerId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::WorkspaceId)
                .uuid()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::PrNumber)
                .big_integer()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::HeadSha)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::FiredAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::DeliveryId)
                .uuid()
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(idens::CodeTriggerFire::DeliveryCondition).string_len(32))
        .col(ColumnDef::new(idens::CodeTriggerFire::DeliveryAction).string_len(16))
        .col(ColumnDef::new(idens::CodeTriggerFire::DeliveryMessage).text())
        .col(
            ColumnDef::new(idens::CodeTriggerFire::State)
                .text()
                .not_null(),
        )
        .col(
            ColumnDef::new(idens::CodeTriggerFire::AttemptCount)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(ColumnDef::new(idens::CodeTriggerFire::LeaseToken).uuid())
        .col(ColumnDef::new(idens::CodeTriggerFire::LeaseExpiresAt).timestamp_with_time_zone())
        .col(ColumnDef::new(idens::CodeTriggerFire::NextAttemptAt).timestamp_with_time_zone())
        .col(
            ColumnDef::new(idens::CodeTriggerFire::LastError)
                .string_len(crate::code::CodeTriggerFire::MAX_LAST_ERROR_CHARS as u32),
        )
        .col(ColumnDef::new(idens::CodeTriggerFire::DeliveredAt).timestamp_with_time_zone())
        .col(ColumnDef::new(idens::CodeTriggerFire::CancelledAt).timestamp_with_time_zone())
        .primary_key(
            Index::create()
                .col(idens::CodeTriggerFire::TriggerId)
                .col(idens::CodeTriggerFire::WorkspaceId)
                .col(idens::CodeTriggerFire::PrNumber)
                .col(idens::CodeTriggerFire::HeadSha),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_trigger_fire_trigger")
                .from(Alias::new(name), idens::CodeTriggerFire::TriggerId)
                .to(Alias::new(trigger_table), idens::CodeTrigger::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_code_trigger_fire_workspace")
                .from(Alias::new(name), idens::CodeTriggerFire::WorkspaceId)
                .to(idens::CodeWorkspace::Table, idens::CodeWorkspace::Id),
        )
        .check(Expr::col(idens::CodeTriggerFire::PrNumber).gte(1))
        .check(Expr::col(idens::CodeTriggerFire::DeliveryId).ne(uuid::Uuid::nil()))
        .check(Expr::col(idens::CodeTriggerFire::State).is_in([
            "pending",
            "delivered",
            "cancelled",
        ]))
        .check(Expr::col(idens::CodeTriggerFire::AttemptCount).gte(0))
        .check(no_lease.or(active_lease))
        .check(pending.or(delivered).or(cancelled))
        .check(
            Expr::col(idens::CodeTriggerFire::State)
                .eq("delivered")
                .or(has_delivery_payload),
        )
        .check(
            Expr::col(idens::CodeTriggerFire::DeliveryCondition)
                .is_null()
                .or(Expr::col(idens::CodeTriggerFire::DeliveryCondition)
                    .is_in(conditions.iter().copied())),
        )
        .check(
            Expr::col(idens::CodeTriggerFire::DeliveryAction)
                .is_null()
                .or(Expr::col(idens::CodeTriggerFire::DeliveryAction).is_in(["deliver", "notify"])),
        )
        .check(
            Expr::col(idens::CodeTriggerFire::LastError)
                .is_null()
                .or(
                    Func::char_length(Expr::col(idens::CodeTriggerFire::LastError))
                        .between(1, crate::code::CodeTriggerFire::MAX_LAST_ERROR_CHARS as i32),
                ),
        )
        .to_owned()
}

fn code_trigger_fire_outbox_indexes() -> Vec<IndexCreateStatement> {
    vec![
        Index::create()
            .name("idx_code_trigger_fire_workspace")
            .table(idens::CodeTriggerFire::Table)
            .col(idens::CodeTriggerFire::WorkspaceId)
            .to_owned(),
        Index::create()
            .name("idx_code_trigger_fire_pending_due")
            .table(idens::CodeTriggerFire::Table)
            .col(idens::CodeTriggerFire::State)
            .col(idens::CodeTriggerFire::NextAttemptAt)
            .col(idens::CodeTriggerFire::LeaseExpiresAt)
            .to_owned(),
    ]
}

/// Durable per-session queued follow-ups (decision 69).
///
/// Mirrors the chat `queued_turn` contract onto code sessions: a message
/// accepted while the session or its workspace checkout is busy parks as a
/// row rather than in the worker's single in-memory slot, so the queue
/// survives restarts, holds more than one message, and supports list, edit,
/// reorder, and delete. The row id is the turn id the promoted turn is
/// inserted under, in the same transaction that deletes the row.
struct CodeQueuedTurns;

impl MigrationName for CodeQueuedTurns {
    fn name(&self) -> &str {
        "m20260824_000011_code_queued_turns"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeQueuedTurns {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeQueuedTurn::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::Message)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::AttachmentsJson)
                            .text()
                            .not_null()
                            .default("[]"),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::Position)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeQueuedTurn::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_queued_turn_session")
                            .from(
                                idens::CodeQueuedTurn::Table,
                                idens::CodeQueuedTurn::SessionId,
                            )
                            .to(idens::CodeSession::Table, idens::CodeSession::Id),
                    )
                    .check(Func::char_length(Expr::col(idens::CodeQueuedTurn::Message)).gt(0))
                    .check(Expr::col(idens::CodeQueuedTurn::Position).gte(0))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_queued_turn_session_position")
                    .table(idens::CodeQueuedTurn::Table)
                    .col(idens::CodeQueuedTurn::SessionId)
                    .col(idens::CodeQueuedTurn::Position)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would lose parked messages.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::prelude::{PostgresQueryBuilder, SqliteQueryBuilder};
    use sea_orm_migration::MigratorTrait;

    use super::Migrator;

    /// Every `CREATE TABLE` a fresh database runs comes from `sqlite_master`,
    /// which stores the statement verbatim and appends what `ALTER TABLE`
    /// adds. Comparing two databases through it therefore sees columns a
    /// migration added, not only tables it created.
    async fn schema_of(db: &sea_orm::DatabaseConnection) -> String {
        let rows = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT sql FROM sqlite_master WHERE sql IS NOT NULL \
                 ORDER BY type, name"
                    .to_owned(),
            ))
            .await
            .unwrap();
        rows.iter()
            .map(|row| row.try_get::<String>("", "sql").unwrap())
            .collect::<Vec<_>>()
            .join(";\n")
    }

    #[tokio::test]
    async fn a_fresh_database_records_the_whole_chain() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, None).await.unwrap();

        let versions = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT version FROM seaql_migrations ORDER BY version".to_owned(),
            ))
            .await
            .unwrap();
        let versions: Vec<String> = versions
            .iter()
            .map(|row| row.try_get::<String>("", "version").unwrap())
            .collect();
        assert_eq!(
            versions,
            [
                "m20260814_000001_baseline",
                "m20260814_000002_app_owner",
                "m20260814_000003_code_owner",
                "m20260822_000004_baseline_repair",
                "m20260822_000005_code_session_fast_mode",
                "m20260822_000006_code_session_images",
                "m20260822_000007_trigger_fire_outbox",
                "m20260822_000008_trigger_delivery_receipts",
                "m20260822_000009_code_pull_request_facts",
                "m20260823_000010_trigger_fact_conditions",
                "m20260824_000011_code_queued_turns",
                "m20260824_000012_code_pull_request_live_tier",
                "m20260824_000013_code_pull_request_etags",
                "m20260824_000014_code_turn_model_snapshot",
            ]
        );
        assert!(db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT owner FROM app LIMIT 1".to_owned(),
            ))
            .await
            .unwrap()
            .is_none());
    }

    /// A database that stopped at the current baseline and later took the rest
    /// of the chain must land on the schema a fresh database gets in one pass.
    ///
    /// This is the property the chain exists for. The desktop profile can be
    /// deleted and rebuilt; the self-host PostgreSQL store cannot, so it only
    /// ever sees the appended migrations. If those disagree with the baseline
    /// by even a default or a nullability, the two deployments run different
    /// schemas against the same queries — and nothing else notices, because
    /// each one is internally consistent.
    ///
    /// With this short chain the check is nearly free. It stops being free the
    /// first time an appended migration adds a column the baseline declares
    /// differently, which is the mistake the folded-in `ensure_*` branches
    /// were one edit away from making.
    ///
    /// This checks the internal ordering contract. The versioned release tests
    /// cover the older schema that users actually upgrade from, including the
    /// backend-specific statements in later migrations.
    #[tokio::test]
    async fn a_stepwise_upgrade_lands_on_the_fresh_schema() {
        let stepwise = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&stepwise, Some(1)).await.unwrap();
        Migrator::up(&stepwise, None).await.unwrap();

        let fresh = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&fresh, None).await.unwrap();

        assert_eq!(
            schema_of(&stepwise).await,
            schema_of(&fresh).await,
            "an upgraded database and a fresh one describe different schemas; \
             an appended migration disagrees with the baseline"
        );
    }

    /// The durable self-host path: a database that recorded the pre-squash
    /// chain keeps its code rows, skips the baseline, and gains the owner
    /// column from [`super::CodeOwner`] instead of having `CREATE TABLE`
    /// re-run against it.
    #[tokio::test]
    async fn an_existing_pre_owner_code_repo_is_backfilled_and_not_recreated() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE app (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                current_revision_id TEXT NOT NULL,
                revision_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT,
                owner TEXT NOT NULL DEFAULT 'local'
            )",
        )
        .await
        .unwrap();
        db.execute_unprepared(
            "CREATE TABLE code_repo (
                id TEXT PRIMARY KEY NOT NULL,
                root_path TEXT NOT NULL UNIQUE,
                display_name TEXT NOT NULL,
                default_base_ref TEXT NOT NULL,
                branch_prefix TEXT NOT NULL,
                setup_script TEXT,
                archive_script TEXT,
                quick_actions TEXT NOT NULL,
                created_at TEXT NOT NULL
            )",
        )
        .await
        .unwrap();
        db.execute_unprepared(concat!(
            "INSERT INTO code_repo (id, root_path, display_name, default_base_ref, ",
            "branch_prefix, quick_actions, created_at) VALUES ",
            "('00000000-0000-0000-0000-000000000003', '/srv/legacy', 'legacy', 'main', ",
            "'tidebreak/', '[]', '2026-08-14 00:00:00+00:00')"
        ))
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();

        let row = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT owner FROM code_repo WHERE display_name = 'legacy'".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "owner").unwrap(), "local");
    }

    #[tokio::test]
    async fn an_existing_pre_owner_app_is_backfilled_and_not_recreated() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE app (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                current_revision_id TEXT NOT NULL,
                revision_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            )",
        )
        .await
        .unwrap();
        db.execute_unprepared(
            concat!(
                "INSERT INTO app (id, name, current_revision_id, revision_count, created_at, updated_at, deleted_at) ",
                "VALUES ('00000000-0000-0000-0000-000000000001', 'legacy', ",
                "'00000000-0000-0000-0000-000000000002', 1, ",
                "'2026-08-14 00:00:00+00:00', '2026-08-14 00:00:00+00:00', NULL)"
            ),
        )
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();

        let row = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT owner FROM app WHERE name = 'legacy'".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "owner").unwrap(), "local");
    }

    /// A self-host database created before later code tables joined the
    /// baseline keeps its rows and gains every table that the current schema
    /// expects.
    #[tokio::test]
    async fn a_pre_pin_database_gains_tables_added_to_the_baseline() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE app (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                current_revision_id TEXT NOT NULL,
                revision_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT,
                owner TEXT NOT NULL DEFAULT 'local'
            )",
        )
        .await
        .unwrap();
        db.execute_unprepared(concat!(
            "INSERT INTO app (id, name, current_revision_id, revision_count, ",
            "created_at, updated_at, deleted_at, owner) VALUES ",
            "('00000000-0000-0000-0000-000000000001', 'legacy', ",
            "'00000000-0000-0000-0000-000000000002', 1, ",
            "'2026-08-14 00:00:00+00:00', '2026-08-14 00:00:00+00:00', NULL, 'local')"
        ))
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();

        for table in [
            "code_watch",
            "code_trigger",
            "code_trigger_fire",
            "code_session_image",
        ] {
            let exists = db
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!(
                        "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
                    ),
                ))
                .await
                .unwrap();
            assert!(exists.is_some(), "repair did not create {table}");
        }
        let legacy = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM app WHERE id = '00000000-0000-0000-0000-000000000001'".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(legacy.try_get::<String>("", "name").unwrap(), "legacy");
    }
    /// A v0.60.0 SQLite database keeps release rows while the current chain
    /// adds image dimensions and upgrades trigger fires into the outbox.
    #[tokio::test]
    async fn a_v060_sqlite_database_keeps_release_rows() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(include_str!("../../fixtures/schema-v0.60.0.sql"))
            .await
            .unwrap();
        Migrator::install(&db).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO seaql_migrations (version, applied_at) \
             VALUES ('m20260814_000001_baseline', 0)",
        )
        .await
        .unwrap();
        db.execute_unprepared(
            "INSERT INTO code_repo (
                id, owner, root_path, display_name, default_base_ref,
                branch_prefix, quick_actions, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000101', 'local', '/srv/release',
                'release', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_workspace (
                id, owner, repo_id, title, worktree_path, branch_name, base_ref,
                status, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000102', 'local',
                '00000000-0000-0000-0000-000000000101', 'release',
                '/srv/release-worktree', 'tidebreak/release', 'main', 'active',
                '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_trigger (
                id, owner, repo_id, condition, action, enabled, created_at, updated_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000106', 'local',
                '00000000-0000-0000-0000-000000000101', 'checks_failed',
                'deliver', TRUE, '2026-08-20T12:00:00Z', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_trigger_fire (
                owner, trigger_id, workspace_id, head_sha, pr_number, fired_at
             ) VALUES (
                'local', '00000000-0000-0000-0000-000000000106',
                '00000000-0000-0000-0000-000000000102', 'release-head', 42,
                '2026-08-20T12:01:00Z'
             );
             INSERT INTO code_session (
                id, owner, workspace_id, kind, harness_kind, permission_mode,
                lifecycle, attention_state, attention_source, created_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000103', 'local',
                '00000000-0000-0000-0000-000000000102', 'interactive',
                'claude_code', 'ask', 'idle', '{}', 'lifecycle',
                '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_turn (
                id, owner, session_id, ordinal, status, user_input, started_at
             ) VALUES (
                '00000000-0000-0000-0000-000000000104', 'local',
                '00000000-0000-0000-0000-000000000103', 1, 'completed',
                'keep this attachment', '2026-08-20T12:00:00Z'
             );
             INSERT INTO code_turn_attachment (
                owner, turn_id, ordinal, blob_id, media_type, byte_len
             ) VALUES (
                'local', '00000000-0000-0000-0000-000000000104', 0,
                '00000000-0000-0000-0000-000000000105', 'image/png', 8
             )",
        )
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();

        let attachment = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT blob_id, width, height, byte_len \
                 FROM code_turn_attachment WHERE turn_id = \
                 '00000000-0000-0000-0000-000000000104'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attachment.try_get::<String>("", "blob_id").unwrap(),
            "00000000-0000-0000-0000-000000000105"
        );
        assert_eq!(attachment.try_get::<i32>("", "width").unwrap(), 1);
        assert_eq!(attachment.try_get::<i32>("", "height").unwrap(), 1);
        assert_eq!(attachment.try_get::<i64>("", "byte_len").unwrap(), 8);

        let fire = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT delivery_id, state, attempt_count,
                        delivered_at = fired_at AS delivered_at_matches,
                        lease_token IS NULL AS lease_token_cleared,
                        lease_expires_at IS NULL AS lease_expiry_cleared,
                        next_attempt_at IS NULL AS next_attempt_cleared,
                        last_error IS NULL AS last_error_cleared
                 FROM code_trigger_fire
                 WHERE trigger_id = '00000000-0000-0000-0000-000000000106'
                   AND workspace_id = '00000000-0000-0000-0000-000000000102'
                   AND pr_number = 42
                   AND head_sha = 'release-head'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            fire.try_get::<uuid::Uuid>("", "delivery_id").unwrap(),
            uuid::Uuid::nil()
        );
        assert_eq!(fire.try_get::<String>("", "state").unwrap(), "delivered");
        assert_eq!(fire.try_get::<i64>("", "attempt_count").unwrap(), 1);
        for column in [
            "delivered_at_matches",
            "lease_token_cleared",
            "lease_expiry_cleared",
            "next_attempt_cleared",
            "last_error_cleared",
        ] {
            assert!(fire.try_get::<bool>("", column).unwrap());
        }

        let fire_primary_key = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM pragma_table_info('code_trigger_fire')
                 WHERE pk > 0 ORDER BY pk"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "name").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            fire_primary_key,
            ["trigger_id", "workspace_id", "pr_number", "head_sha"]
        );
        assert!(db
            .execute_unprepared(
                "UPDATE code_trigger_fire
                 SET state = 'pending', delivered_at = NULL, next_attempt_at = NULL
                 WHERE trigger_id = '00000000-0000-0000-0000-000000000106'"
            )
            .await
            .is_err());

        let columns = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info('code_turn_attachment')".to_owned(),
            ))
            .await
            .unwrap();
        for name in ["width", "height"] {
            let column = columns
                .iter()
                .find(|column| column.try_get::<String>("", "name").unwrap() == name)
                .unwrap();
            assert_eq!(column.try_get::<i32>("", "notnull").unwrap(), 1);
            assert_eq!(
                column.try_get::<Option<String>>("", "dflt_value").unwrap(),
                None
            );
        }

        for (kind, name) in [
            ("table", "code_session_image"),
            ("table", "code_trigger_delivery_receipt"),
            ("index", "idx_code_session_image_blob"),
            ("index", "idx_code_turn_attachment_blob"),
        ] {
            let object = db
                .query_one_raw(Statement::from_string(
                    DbBackend::Sqlite,
                    format!(
                        "SELECT 1 AS present FROM sqlite_master \
                         WHERE type = '{kind}' AND name = '{name}'"
                    ),
                ))
                .await
                .unwrap();
            assert!(object.is_some(), "upgrade did not create {name}");
        }

        let versions = db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT version FROM seaql_migrations ORDER BY version".to_owned(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "version").unwrap())
            .collect::<Vec<_>>();
        assert!(versions.contains(&"m20260822_000004_baseline_repair".to_owned()));
        assert!(versions.contains(&"m20260822_000005_code_session_fast_mode".to_owned()));
        assert!(versions.contains(&"m20260822_000006_code_session_images".to_owned()));
        assert!(versions.contains(&"m20260822_000007_trigger_fire_outbox".to_owned()));
        assert!(versions.contains(&"m20260822_000008_trigger_delivery_receipts".to_owned()));
    }

    /// The baseline is frozen, and this is what holds it still.
    ///
    /// `Migrator::up` records one name per migration and cannot tell that the
    /// statements behind an already-recorded name changed. So an edit here
    /// reaches a fresh database and no existing one — the two then run
    /// different schemas against the same queries, each internally consistent,
    /// with nothing to notice. That used to be survivable because an epoch
    /// bump deleted the local database. Nothing deletes it now.
    ///
    /// The fix for a diff here is therefore not a regenerated fixture. It is
    /// an appended migration in [`Migrator::migrations`], which reaches both.
    ///
    /// Both backends are rendered, because the two builders do not agree on
    /// everything: a type that collapses in SQLite, or a constraint it parses
    /// and ignores, is a real difference on PostgreSQL that a SQLite-only
    /// fixture cannot see.
    ///
    /// Regenerate with `UPDATE_SCHEMA_FIXTURE=1 cargo test -p tidebreak-core`
    /// only when squashing the chain for a release, which is the one time the
    /// baseline is supposed to move.
    #[test]
    fn the_schema_baseline_is_pinned() {
        // sea-query renders one statement per line. Break each column and
        // table constraint onto its own line so a one-column edit reviews as a
        // one-line diff rather than a rewritten table. Splitting on `, ` only
        // before a quoted identifier or a constraint keyword leaves list
        // literals inside a CHECK alone.
        fn readable(statement: &str) -> String {
            statement
                .replacen(" ( ", " (\n    ", 1)
                .replace(", \"", ",\n    \"")
                .replace(", CHECK", ",\n    CHECK")
                .replace(", FOREIGN KEY", ",\n    FOREIGN KEY")
                .replace(" )", "\n)")
        }

        // The builders are unit structs that `to_string` consumes and that
        // implement neither Copy nor Clone, so take a constructor and mint a
        // fresh one per statement.
        fn render<B>(builder: fn() -> B) -> String
        where
            B: sea_orm::sea_query::SchemaBuilder,
        {
            let mut rendered = String::new();
            for entry in super::baseline::tables() {
                rendered.push_str(&readable(&entry.table.to_string(builder())));
                rendered.push_str(";\n\n");
                for index in entry.indexes {
                    rendered.push_str(&index.to_string(builder()));
                    rendered.push_str(";\n");
                }
                rendered.push('\n');
            }
            for seed in super::baseline::SEED_STATEMENTS {
                rendered.push_str(seed);
                rendered.push_str(";\n");
            }
            rendered
        }

        let updating = std::env::var_os("UPDATE_SCHEMA_FIXTURE").is_some();
        for (fixture, rendered) in [
            (
                "fixtures/schema-baseline.sql",
                render(|| SqliteQueryBuilder),
            ),
            (
                "fixtures/schema-baseline.postgres.sql",
                render(|| PostgresQueryBuilder),
            ),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture);
            if updating {
                std::fs::create_dir_all(path.parent().expect("the fixture path has a parent"))
                    .expect("the fixture directory is creatable");
                std::fs::write(&path, &rendered).expect("the fixture path is writable");
                continue;
            }
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            assert_eq!(
                existing, rendered,
                "the schema baseline changed, and it is frozen; an edit here \
             reaches a fresh database and no existing one. Append a migration \
             in db/migration.rs instead. Regenerate with \
             UPDATE_SCHEMA_FIXTURE=1 cargo test -p tidebreak-core only when \
             squashing the chain for a release."
            );
        }
    }

    /// The condition-widening rebuild keeps every trigger and fire row, keeps
    /// the outbox identity, accepts the fact-edge tokens, and still refuses a
    /// garbage token. A wrong rebuild passes a fresh-database test easily —
    /// only seeded data shows a dropped row or a lost constraint.
    #[tokio::test]
    async fn the_condition_widening_keeps_trigger_and_fire_rows() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        // Everything up to, but not including, the widening.
        Migrator::up(&db, Some(9)).await.unwrap();

        for statement in [
            "INSERT INTO code_repo (id, owner, root_path, display_name, default_base_ref, \
             branch_prefix, quick_actions, created_at) VALUES ('repo-1', 'local', '/tmp/r', \
             'r', 'main', 'tidebreak/', '[]', '2026-08-20T00:00:00Z')",
            "INSERT INTO code_workspace (id, owner, repo_id, title, worktree_path, branch_name, \
             base_ref, status, created_at) VALUES ('ws-1', 'local', 'repo-1', 'w', '/tmp/w', \
             'tidebreak/w', 'main', 'active', '2026-08-20T00:00:00Z')",
            "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
             created_at, updated_at) VALUES ('trig-1', 'local', 'repo-1', 'checks_failed', \
             'deliver', TRUE, '2026-08-20T00:00:00Z', '2026-08-20T00:00:00Z')",
            "INSERT INTO code_trigger_fire (owner, trigger_id, workspace_id, pr_number, \
             head_sha, fired_at, delivery_id, delivery_condition, delivery_action, \
             delivery_message, state, attempt_count, next_attempt_at) VALUES ('local', \
             'trig-1', 'ws-1', 41, 'aaa', '2026-08-21T00:00:00Z', 'deliv-1', 'checks_failed', \
             'deliver', 'm', 'pending', 0, '2026-08-21T00:00:00Z')",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }

        Migrator::up(&db, None).await.unwrap();

        let fire = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT head_sha, state, delivery_condition FROM code_trigger_fire \
                 WHERE trigger_id = 'trig-1'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .expect("the seeded fire survives the rebuild");
        assert_eq!(fire.try_get::<String>("", "head_sha").unwrap(), "aaa");
        assert_eq!(fire.try_get::<String>("", "state").unwrap(), "pending");

        // The widened vocabulary is accepted on both rebuilt CHECKs...
        db.execute_unprepared(
            "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
             created_at, updated_at) VALUES ('trig-2', 'local', 'repo-1', 'pr_opened', \
             'notify', TRUE, '2026-08-22T00:00:00Z', '2026-08-22T00:00:00Z')",
        )
        .await
        .unwrap();
        db.execute_unprepared(
            "INSERT INTO code_trigger_fire (owner, trigger_id, workspace_id, pr_number, \
             head_sha, fired_at, delivery_id, delivery_condition, delivery_action, \
             delivery_message, state, attempt_count, next_attempt_at) VALUES ('local', \
             'trig-2', 'ws-1', 42, 'opened', '2026-08-22T00:00:00Z', 'deliv-2', 'pr_opened', \
             'notify', 'm', 'pending', 0, '2026-08-22T00:00:00Z')",
        )
        .await
        .unwrap();

        // ...and a token outside it still fails.
        assert!(db
            .execute_unprepared(
                "INSERT INTO code_trigger (id, owner, repo_id, condition, action, enabled, \
                 created_at, updated_at) VALUES ('trig-3', 'local', 'repo-1', 'pr_sparkled', \
                 'notify', TRUE, '2026-08-22T00:00:00Z', '2026-08-22T00:00:00Z')",
            )
            .await
            .is_err());
    }
}
