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
            Box::new(PrePinCodeLifecycleRepair),
            Box::new(CodeApprovalBinding),
            Box::new(CodeSessionProcessIdentity),
            Box::new(CodeWorkspaceArchiving),
            Box::new(CodePermissionModeIntent),
            Box::new(AgentNotification),
            Box::new(CodeWorkflowRuns),
            Box::new(CodeSessionIncarnations),
            Box::new(IncarnationIngest),
            Box::new(CodeExternalBindings),
            Box::new(CodeExternalEvents),
            Box::new(CodeTurnRewrite),
            Box::new(CodeExternalGrants),
            Box::new(CodeConnectHandshakes),
        ]
    }
}

/// Record a non-reusable creation identity beside each live child pid.
struct CodeSessionProcessIdentity;

impl MigrationName for CodeSessionProcessIdentity {
    fn name(&self) -> &str {
        "m20260826_000017_code_session_process_identity"
    }
}

/// Add the transient workspace state that excludes writers during archive.
struct CodeWorkspaceArchiving;

impl MigrationName for CodeWorkspaceArchiving {
    fn name(&self) -> &str {
        "m20260826_000018_code_workspace_archiving"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeSessionProcessIdentity {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("code_session", "child_process_identity")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeSession::Table)
                        .add_column(ColumnDef::new(idens::CodeSession::ChildProcessIdentity).text())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Removing the identity would make a recorded pid unsafe to reap.
        Ok(())
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeWorkspaceArchiving {
    fn use_transaction(&self) -> Option<bool> {
        // SQLite needs a connection-bound transaction that starts only after
        // foreign-key enforcement is disabled on that exact connection. The
        // SQLite branch owns that transaction manually. PostgreSQL executes
        // its ALTER statements atomically on its own.
        None
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        match manager.get_database_backend() {
            DbBackend::Postgres => {
                let transaction = manager.begin().await?;
                transaction
                    .get_connection()
                    .execute_unprepared(
                        r#"
DO $repair$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_workspace'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'status'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_workspace" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$repair$;
ALTER TABLE "code_workspace"
    ADD CONSTRAINT "code_workspace_status_check"
    CHECK ("status" IN (
        'creating', 'setup_failed', 'active', 'archiving', 'archived', 'released'
    ));
"#,
                    )
                    .await?;
                transaction.commit().await
            }
            DbBackend::Sqlite => rebuild_sqlite_code_workspace_for_archiving(manager).await,
            backend => Err(DbErr::Custom(format!(
                "unsupported database backend for workspace archiving migration: {backend:?}"
            ))),
        }
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // An interrupted archive can persist this value. Removing it would
        // make that row unreadable instead of recoverable.
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
async fn rebuild_sqlite_code_workspace_for_archiving(
    manager: &SchemaManager<'_>,
) -> Result<(), DbErr> {
    rebuild_sqlite_code_workspace_for_archiving_inner(manager, false).await
}

#[cfg(not(feature = "sqlite"))]
async fn rebuild_sqlite_code_workspace_for_archiving(
    _manager: &SchemaManager<'_>,
) -> Result<(), DbErr> {
    Err(DbErr::Custom(
        "SQLite workspace migration support is not compiled".to_owned(),
    ))
}

#[cfg(feature = "sqlite")]
async fn rebuild_sqlite_code_workspace_for_archiving_inner(
    manager: &SchemaManager<'_>,
    _fail_after_drop_for_test: bool,
) -> Result<(), DbErr> {
    use sea_orm::sqlx::Acquire as _;
    use sea_orm::DatabaseExecutor;

    let DatabaseExecutor::Connection(database) = manager.get_connection() else {
        return Err(DbErr::Custom(
            "SQLite workspace rebuild requires the migration connection".to_owned(),
        ));
    };
    let mut connection = database
        .get_sqlite_connection_pool()
        .acquire()
        .await
        .map_err(|error| DbErr::Custom(format!("acquire SQLite migration connection: {error}")))?;
    sea_orm::sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .map_err(|error| DbErr::Custom(format!("disable SQLite foreign keys: {error}")))?;

    let mut transaction = connection
        .begin()
        .await
        .map_err(|error| DbErr::Custom(format!("begin SQLite workspace rebuild: {error}")))?;
    let rebuild = async {
        for statement in [
            r#"DROP TABLE IF EXISTS "code_workspace_archiving""#,
            r#"
CREATE TABLE "code_workspace_archiving" (
    "id" text NOT NULL PRIMARY KEY,
    "owner" text NOT NULL DEFAULT 'local',
    "repo_id" text NOT NULL,
    "title" text NOT NULL,
    "worktree_path" text NOT NULL UNIQUE,
    "branch_name" text NOT NULL,
    "base_ref" text NOT NULL,
    "status" text NOT NULL CHECK ("status" IN (
        'creating', 'setup_failed', 'active', 'archiving', 'archived', 'released'
    )),
    "pr" text,
    "created_at" text NOT NULL,
    "archived_at" text,
    "released_at" text,
    "released_tip" text,
    "bundle_bytes" integer,
    FOREIGN KEY ("repo_id") REFERENCES "code_repo" ("id")
)"#,
            r#"
INSERT INTO "code_workspace_archiving" (
    "id", "owner", "repo_id", "title", "worktree_path", "branch_name",
    "base_ref", "status", "pr", "created_at", "archived_at",
    "released_at", "released_tip", "bundle_bytes"
)
SELECT
    "id", "owner", "repo_id", "title", "worktree_path", "branch_name",
    "base_ref", "status", "pr", "created_at", "archived_at",
    "released_at", "released_tip", "bundle_bytes"
FROM "code_workspace""#,
            r#"DROP TABLE "code_workspace""#,
        ] {
            sea_orm::sqlx::query(statement)
                .execute(&mut *transaction)
                .await?;
        }
        #[cfg(test)]
        if _fail_after_drop_for_test {
            sea_orm::sqlx::query("SELECT * FROM missing_workspace_rebuild_table")
                .execute(&mut *transaction)
                .await?;
        }
        for statement in [
            r#"ALTER TABLE "code_workspace_archiving" RENAME TO "code_workspace""#,
            r#"CREATE UNIQUE INDEX "idx_code_workspace_repo_branch"
                ON "code_workspace" ("repo_id", "branch_name")"#,
            r#"CREATE INDEX "idx_code_workspace_owner_created"
                ON "code_workspace" ("owner", "created_at")"#,
        ] {
            sea_orm::sqlx::query(statement)
                .execute(&mut *transaction)
                .await?;
        }
        Ok::<(), sea_orm::sqlx::Error>(())
    }
    .await;

    let rebuild = match rebuild {
        Ok(()) => transaction
            .commit()
            .await
            .map_err(|error| DbErr::Custom(format!("commit SQLite workspace rebuild: {error}"))),
        Err(error) => {
            let rollback = transaction.rollback().await;
            match rollback {
                Ok(()) => Err(DbErr::Custom(format!(
                    "rebuild SQLite workspace table: {error}"
                ))),
                Err(rollback) => Err(DbErr::Custom(format!(
                    "rebuild SQLite workspace table: {error}; rollback failed: {rollback}"
                ))),
            }
        }
    };

    let enable = match sea_orm::sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
    {
        Ok(_) => {
            sea_orm::sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
                .fetch_one(&mut *connection)
                .await
        }
        Err(error) => Err(error),
    }
    .map_err(|error| DbErr::Custom(format!("restore SQLite foreign keys: {error}")))
    .and_then(|enabled| {
        if enabled == 1 {
            Ok(())
        } else {
            Err(DbErr::Custom(
                "restore SQLite foreign keys: PRAGMA remained disabled".to_owned(),
            ))
        }
    });
    match (rebuild, enable) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(rebuild), Err(enable)) => {
            Err(DbErr::Custom(format!("{rebuild}; additionally, {enable}")))
        }
    }
}

/// Add the durable claim that brackets a live permission-mode mutation.
struct CodePermissionModeIntent;

impl MigrationName for CodePermissionModeIntent {
    fn name(&self) -> &str {
        "m20260826_000019_code_permission_mode_intent"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodePermissionModeIntent {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            (
                "permission_mode_revision",
                ColumnDef::new(idens::CodeSession::PermissionModeRevision)
                    .big_integer()
                    .not_null()
                    .default(0)
                    .to_owned(),
            ),
            (
                "permission_mode_intent",
                ColumnDef::new(idens::CodeSession::PermissionModeIntent)
                    .text()
                    .to_owned(),
            ),
            (
                "permission_mode_intent_revision",
                ColumnDef::new(idens::CodeSession::PermissionModeIntentRevision)
                    .big_integer()
                    .to_owned(),
            ),
            (
                "permission_mode_intent_epoch",
                ColumnDef::new(idens::CodeSession::PermissionModeIntentEpoch)
                    .big_integer()
                    .to_owned(),
            ),
            (
                "permission_mode_intent_lifecycle",
                ColumnDef::new(idens::CodeSession::PermissionModeIntentLifecycle)
                    .text()
                    .to_owned(),
            ),
        ];
        for (name, column) in columns {
            if !manager.has_column("code_session", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(idens::CodeSession::Table)
                            .add_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // A pending intent is a crash-recovery fence. Removing it can make an
        // acknowledged native mode differ from the durable session posture.
        Ok(())
    }
}

/// Durable log of top-level agent turns that settled without cancel.
struct AgentNotification;

impl MigrationName for AgentNotification {
    fn name(&self) -> &str {
        "m20260826_000020_agent_notification"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AgentNotification {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::Notification::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::Notification::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::Notification::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(ColumnDef::new(idens::Notification::Kind).text().not_null())
                    .col(ColumnDef::new(idens::Notification::Title).text().not_null())
                    .col(
                        ColumnDef::new(idens::Notification::Context)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::Notification::DedupeKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::Notification::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::Notification::ReadAt).timestamp_with_time_zone())
                    .check(
                        Expr::col(idens::Notification::Kind)
                            .is_in(["agent_completed", "agent_failed"]),
                    )
                    .check(Func::char_length(Expr::col(idens::Notification::Title)).gt(0))
                    .check(Func::char_length(Expr::col(idens::Notification::DedupeKey)).gt(0))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ux_notification_owner_dedupe")
                    .table(idens::Notification::Table)
                    .col(idens::Notification::Owner)
                    .col(idens::Notification::DedupeKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_notification_owner_created")
                    .table(idens::Notification::Table)
                    .col(idens::Notification::Owner)
                    .col(idens::Notification::CreatedAt)
                    .col(idens::Notification::Id)
                    .if_not_exists()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would lose unread agent-finished rows.
        Ok(())
    }
}

/// Durable workflow-run summaries (decision 66, issue 2578).
///
/// `code_workflow_run` records confirmed observations of GitHub Actions
/// runs, keyed by full repository identity the way `code_pull_request`
/// is. `code_workflow_run_fetch` holds the list-endpoint ETag so the
/// reconcile sweep can send `If-None-Match` and a 304 costs nothing.
struct CodeWorkflowRuns;

impl MigrationName for CodeWorkflowRuns {
    fn name(&self) -> &str {
        "m20260827_000021_code_workflow_runs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeWorkflowRuns {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeWorkflowRun::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Host)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::RepoOwner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::RepoName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::GithubId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeWorkflowRun::RunAttempt).big_integer())
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Name)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Url)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::Status)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Conclusion).text())
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Workflow).text())
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Branch).text())
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Sha).text())
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Event).text())
                    .col(ColumnDef::new(idens::CodeWorkflowRun::Actor).text())
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::FirstSeenAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRun::LastSeenAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(idens::CodeWorkflowRun::GithubId).gte(1))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .unique()
                    .name("uq_code_workflow_run_identity")
                    .table(idens::CodeWorkflowRun::Table)
                    .col(idens::CodeWorkflowRun::Owner)
                    .col(idens::CodeWorkflowRun::Host)
                    .col(idens::CodeWorkflowRun::RepoOwner)
                    .col(idens::CodeWorkflowRun::RepoName)
                    .col(idens::CodeWorkflowRun::GithubId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_code_workflow_run_owner_updated")
                    .table(idens::CodeWorkflowRun::Table)
                    .col(idens::CodeWorkflowRun::Owner)
                    .col(idens::CodeWorkflowRun::UpdatedAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(idens::CodeWorkflowRunFetch::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRunFetch::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRunFetch::Host)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRunFetch::RepoOwner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRunFetch::RepoName)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeWorkflowRunFetch::ListEtag).text())
                    .col(
                        ColumnDef::new(idens::CodeWorkflowRunFetch::ObservedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(idens::CodeWorkflowRunFetch::Owner)
                            .col(idens::CodeWorkflowRunFetch::Host)
                            .col(idens::CodeWorkflowRunFetch::RepoOwner)
                            .col(idens::CodeWorkflowRunFetch::RepoName),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(idens::CodeWorkflowRunFetch::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(idens::CodeWorkflowRun::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

/// Bind every approval to one native request and serialize decisions.
struct CodeApprovalBinding;

impl MigrationName for CodeApprovalBinding {
    fn name(&self) -> &str {
        "m20260825_000016_code_approval_binding"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeApprovalBinding {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let columns = [
            (
                "native_call_id",
                ColumnDef::new(idens::CodeApproval::NativeCallId)
                    .text()
                    .to_owned(),
            ),
            (
                "server_capability",
                ColumnDef::new(idens::CodeApproval::ServerCapability)
                    .text()
                    .to_owned(),
            ),
            (
                "request_sha256",
                ColumnDef::new(idens::CodeApproval::RequestSha256)
                    .text()
                    .to_owned(),
            ),
            (
                "worker_epoch",
                ColumnDef::new(idens::CodeApproval::WorkerEpoch)
                    .big_integer()
                    .to_owned(),
            ),
            (
                "decision_claim",
                ColumnDef::new(idens::CodeApproval::DecisionClaim)
                    .uuid()
                    .to_owned(),
            ),
            (
                "claimed_at",
                ColumnDef::new(idens::CodeApproval::ClaimedAt)
                    .timestamp_with_time_zone()
                    .to_owned(),
            ),
        ];
        for (name, column) in columns {
            if !manager.has_column("code_approval", name).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(idens::CodeApproval::Table)
                            .add_column(column)
                            .to_owned(),
                    )
                    .await?;
            }
        }
        manager
            .create_index(
                Index::create()
                    .name("idx_code_approval_native_request")
                    .table(idens::CodeApproval::Table)
                    .col(idens::CodeApproval::SessionId)
                    .col(idens::CodeApproval::WorkerEpoch)
                    .col(idens::CodeApproval::NativeCallId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;

        let update = Query::update()
            .table(idens::CodeApproval::Table)
            .value(idens::CodeApproval::State, "abandoned")
            .value(idens::CodeApproval::DecidedAt, Expr::current_timestamp())
            .and_where(Expr::col(idens::CodeApproval::State).eq("pending"))
            .to_owned();
        manager.exec_stmt(update).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Removing the binding would make persisted decisions unsafe.
        Ok(())
    }
}

/// Close the known self-host gap left by the last mutable baseline.
///
/// Tidebreak v0.60.0 added repository removal and clone provenance, workspace
/// release state, session reasoning effort, and wider state vocabularies by
/// editing the baseline in place. A PostgreSQL database created by v0.58.0 or
/// v0.59.0 had already recorded that baseline name, so SeaORM never ran those
/// statements. Later migrations repaired added tables, but not changed tables.
///
/// The baseline is frozen now, so this is the one historical repair. Every
/// later schema change already has to append its own migration.
struct PrePinCodeLifecycleRepair;

impl MigrationName for PrePinCodeLifecycleRepair {
    fn name(&self) -> &str {
        "m20260825_000015_pre_pin_code_lifecycle_repair"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for PrePinCodeLifecycleRepair {
    fn use_transaction(&self) -> Option<bool> {
        Some(true)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_pre_pin_code_columns(manager).await?;
        replace_code_repo_registration_index(manager).await?;

        if manager.get_database_backend() == DbBackend::Postgres {
            widen_pre_pin_postgres_checks(manager).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The frozen baseline declares this schema. Removing any part of it
        // would leave both fresh and upgraded databases below that contract.
        Ok(())
    }
}

async fn add_pre_pin_code_columns(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    if !manager.has_column("code_repo", "removed_at").await? {
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeRepo::Table)
                    .add_column(
                        ColumnDef::new(idens::CodeRepo::RemovedAt).timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await?;
    }
    if !manager.has_column("code_repo", "cloned_from").await? {
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeRepo::Table)
                    .add_column(ColumnDef::new(idens::CodeRepo::ClonedFrom).text())
                    .to_owned(),
            )
            .await?;
    }
    if !manager.has_column("code_workspace", "released_at").await? {
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeWorkspace::Table)
                    .add_column(
                        ColumnDef::new(idens::CodeWorkspace::ReleasedAt).timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await?;
    }
    if !manager.has_column("code_workspace", "released_tip").await? {
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeWorkspace::Table)
                    .add_column(ColumnDef::new(idens::CodeWorkspace::ReleasedTip).text())
                    .to_owned(),
            )
            .await?;
    }
    if !manager.has_column("code_workspace", "bundle_bytes").await? {
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeWorkspace::Table)
                    .add_column(ColumnDef::new(idens::CodeWorkspace::BundleBytes).big_integer())
                    .to_owned(),
            )
            .await?;
    }
    if !manager
        .has_column("code_session", "reasoning_effort")
        .await?
    {
        let mut column = ColumnDef::new(idens::CodeSession::ReasoningEffort);
        column.text();
        if manager.get_database_backend() == DbBackend::Sqlite {
            column.check(
                Expr::col(idens::CodeSession::ReasoningEffort)
                    .is_null()
                    .or(Expr::col(idens::CodeSession::ReasoningEffort)
                        .is_in(["none", "low", "medium", "high", "xhigh", "max", "ultra"])),
            );
        }
        manager
            .alter_table(
                Table::alter()
                    .table(idens::CodeSession::Table)
                    .add_column(column)
                    .to_owned(),
            )
            .await?;
    }
    Ok(())
}

async fn replace_code_repo_registration_index(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .drop_index(
            Index::drop()
                .name("idx_code_repo_owner_root_path")
                .table(idens::CodeRepo::Table)
                .if_exists()
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_code_repo_owner_root_path")
                .table(idens::CodeRepo::Table)
                .col(idens::CodeRepo::Owner)
                .col(idens::CodeRepo::RootPath)
                .unique()
                .and_where(Expr::col(idens::CodeRepo::RemovedAt).is_null())
                .to_owned(),
        )
        .await
}

async fn widen_pre_pin_postgres_checks(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .get_connection()
        .execute_unprepared(
            r#"
DO $repair$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_workspace'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'status'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_workspace" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$repair$;
ALTER TABLE "code_workspace"
    ADD CONSTRAINT "code_workspace_status_check"
    CHECK ("status" IN ('creating', 'setup_failed', 'active', 'archived', 'released'));

DO $repair$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_session'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'reasoning_effort'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_session" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$repair$;
ALTER TABLE "code_session"
    ADD CONSTRAINT "code_session_reasoning_effort_check"
    CHECK (
        "reasoning_effort" IS NULL
        OR "reasoning_effort" IN ('none', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra')
    );

DO $repair$
DECLARE
    old_constraint text;
BEGIN
    FOR old_constraint IN
        SELECT constraint_row.conname
        FROM pg_constraint AS constraint_row
        JOIN pg_attribute AS attribute_row
          ON attribute_row.attrelid = constraint_row.conrelid
         AND attribute_row.attnum = ANY (constraint_row.conkey)
        WHERE constraint_row.conrelid = 'code_approval'::regclass
          AND constraint_row.contype = 'c'
          AND attribute_row.attname = 'state'
    LOOP
        EXECUTE format(
            'ALTER TABLE "code_approval" DROP CONSTRAINT %I',
            old_constraint
        );
    END LOOP;
END
$repair$;
ALTER TABLE "code_approval"
    ADD CONSTRAINT "code_approval_state_check"
    CHECK ("state" IN ('pending', 'approved', 'denied', 'abandoned'));
"#,
        )
        .await?;
    Ok(())
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

/// Durable pull-request facts and workspace attribution (decision 77).
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

/// The condition tokens including the fact edges (decision 77).
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

/// Accept the fact-edge conditions `pr_opened` and `pr_updated` (decision 77).
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

/// Record every sandbox lifetime of a remote session as a durable row.
///
/// The intent → active → stopped protocol for remote execution (issue
/// 2870): the row is written *before* the environment is asked to
/// provision, so a crash between the spawn returning and the activation
/// update leaves an intent row a reconcile sweep can find, instead of a
/// spending sandbox nothing remembers. The `sandbox_incarnation` advisory
/// row serializes the per-owner concurrency reservation the same way the
/// claim paths serialize theirs.
struct CodeSessionIncarnations;

impl MigrationName for CodeSessionIncarnations {
    fn name(&self) -> &str {
        "m20260827_000022_code_session_incarnations"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeSessionIncarnations {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeSessionIncarnation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::Owner)
                            .text()
                            .not_null()
                            .default("local"),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::Incarnation)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::State)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeSessionIncarnation::SandboxId).text())
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::StartingTurn)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeSessionIncarnation::StopReason).text())
                    .col(ColumnDef::new(idens::CodeSessionIncarnation::SpendMicrousd).big_integer())
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::TerminalEventsJournaled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::ActivatedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::StoppedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeSessionIncarnation::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_session_incarnation_session")
                            .from(
                                idens::CodeSessionIncarnation::Table,
                                idens::CodeSessionIncarnation::SessionId,
                            )
                            .to(idens::CodeSession::Table, idens::CodeSession::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(
                        Expr::col(idens::CodeSessionIncarnation::State)
                            .is_in(["intent", "active", "stopped"]),
                    )
                    .check(Expr::col(idens::CodeSessionIncarnation::Incarnation).gte(1))
                    .check(Expr::col(idens::CodeSessionIncarnation::StartingTurn).gte(1))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_session_incarnation_session")
                    .table(idens::CodeSessionIncarnation::Table)
                    .col(idens::CodeSessionIncarnation::SessionId)
                    .col(idens::CodeSessionIncarnation::Incarnation)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_session_incarnation_owner_state")
                    .table(idens::CodeSessionIncarnation::Table)
                    .col(idens::CodeSessionIncarnation::Owner)
                    .col(idens::CodeSessionIncarnation::State)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        // The reservation lock, seeded the way the baseline seeds the claim
        // paths: a missing row reads as a hard error at acquire time.
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO advisory_lock (name) VALUES ('sandbox_incarnation') \
                 ON CONFLICT DO NOTHING",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would forget sandboxes that may still be
        // running; the reconcile sweep needs these rows to cancel them.
        Ok(())
    }
}

/// Give each incarnation a durable ingest cursor and a place to keep the
/// run's deliverable.
///
/// The cursor is the highest sandbox event sequence whose journal projection
/// committed, so ingestion resumes exactly where it stopped after a server
/// restart. The task output is the supervisor's terminal deliverable, kept
/// on the incarnation that produced it.
struct IncarnationIngest;

impl MigrationName for IncarnationIngest {
    fn name(&self) -> &str {
        "m20260827_000023_incarnation_ingest"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for IncarnationIngest {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("code_session_incarnation", "events_cursor")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeSessionIncarnation::Table)
                        .add_column(
                            ColumnDef::new(idens::CodeSessionIncarnation::EventsCursor)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("code_session_incarnation", "task_output")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeSessionIncarnation::Table)
                        .add_column(
                            ColumnDef::new(idens::CodeSessionIncarnation::TaskOutput).text(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("code_session_incarnation", "last_wip_ref")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeSessionIncarnation::Table)
                        .add_column(
                            ColumnDef::new(idens::CodeSessionIncarnation::LastWipRef).text(),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the columns would lose ingest positions for sessions that
        // are still running.
        Ok(())
    }
}

/// Map an external conversation onto a session (docs/slack-sessions.md,
/// stage 2).
///
/// One row per conversation: `(owner, channel_kind, external_key)` is the
/// durable thread identity — Slack's thread key is one kind, and the key is
/// opaque to the machine so later channels reuse the table unchanged. The
/// grant id tags which adapter credential created the binding; every
/// grant-authenticated call scopes through it.
struct CodeExternalBindings;

impl MigrationName for CodeExternalBindings {
    fn name(&self) -> &str {
        "m20260828_000024_code_external_bindings"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeExternalBindings {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeExternalBinding::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::Owner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::ChannelKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::ExternalKey)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::GrantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalBinding::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_external_binding_session")
                            .from(
                                idens::CodeExternalBinding::Table,
                                idens::CodeExternalBinding::SessionId,
                            )
                            .to(idens::CodeSession::Table, idens::CodeSession::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // The race gate: two creates for one conversation cannot both
        // commit, so get-or-create converges on one session.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_binding_key")
                    .table(idens::CodeExternalBinding::Table)
                    .col(idens::CodeExternalBinding::Owner)
                    .col(idens::CodeExternalBinding::ChannelKind)
                    .col(idens::CodeExternalBinding::ExternalKey)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_binding_session")
                    .table(idens::CodeExternalBinding::Table)
                    .col(idens::CodeExternalBinding::SessionId)
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would orphan live conversations: the adapter's
        // routing rows would point at sessions this machine no longer maps.
        Ok(())
    }
}

/// Make external message delivery idempotent (docs/slack-sessions.md,
/// stage 2).
///
/// One row per channel event that caused a queue row or turn. The event id
/// commits in the same transaction as that row, and `turn_id` names it —
/// the queue row's id becomes the turn's id at promotion, so one column
/// follows the message through its whole life. A replayed delivery derives
/// its response from that row's current state; there is no separate outcome
/// snapshot to go stale.
struct CodeExternalEvents;

impl MigrationName for CodeExternalEvents {
    fn name(&self) -> &str {
        "m20260828_000025_code_external_events"
    }
}

/// Sibling column for a lucid rewrite of a completed turn's closing message.
///
/// The original journal text stays on the event. This column has one writer,
/// the same rule as `narrative`: `save_turn` must not carry it.
struct CodeTurnRewrite;

impl MigrationName for CodeTurnRewrite {
    fn name(&self) -> &str {
        "m20260828_000026_code_turn_rewrite"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeExternalEvents {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeExternalEvent::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::Owner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::EventId)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::ChannelTs)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::TurnId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalEvent::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_code_external_event_session")
                            .from(
                                idens::CodeExternalEvent::Table,
                                idens::CodeExternalEvent::SessionId,
                            )
                            .to(idens::CodeSession::Table, idens::CodeSession::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // The replay gate: one channel event causes at most one row.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_event_key")
                    .table(idens::CodeExternalEvent::Table)
                    .col(idens::CodeExternalEvent::Owner)
                    .col(idens::CodeExternalEvent::SessionId)
                    .col(idens::CodeExternalEvent::EventId)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would forget which deliveries already ran, and
        // the channel's retries would double-submit turns.
        Ok(())
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeTurnRewrite {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("code_turn", "rewrite").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::CodeTurn::Table)
                        .add_column(ColumnDef::new(idens::CodeTurn::Rewrite).text())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The column holds derived prose the reader may still be looking at.
        Ok(())
    }
}

/// The credential a channel adapter holds per linked user
/// (docs/slack-sessions.md, stage 2).
///
/// The machine stores only hashes: the access token authenticates every
/// grant call, the refresh token rotates the pair, and the previous
/// refresh hash stays behind for reuse detection — a replayed rotated
/// refresh token can only be theft, and it revokes the grant on sight.
struct CodeExternalGrants;

impl MigrationName for CodeExternalGrants {
    fn name(&self) -> &str {
        "m20260828_000027_code_external_grants"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeExternalGrants {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeExternalGrant::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::Owner)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::ChannelKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::ExternalIdentity)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::WorkspaceIdentity)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::TokenHash)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::RefreshHash)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::RotatedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrant::RevokedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(ColumnDef::new(idens::CodeExternalGrant::RevokedReason).text())
                    .to_owned(),
            )
            .await?;
        // Every authentication is a lookup by presented-token hash, and a
        // hash collision across grants would be an ambiguous credential.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_grant_token")
                    .table(idens::CodeExternalGrant::Table)
                    .col(idens::CodeExternalGrant::TokenHash)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_grant_refresh")
                    .table(idens::CodeExternalGrant::Table)
                    .col(idens::CodeExternalGrant::RefreshHash)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        // One live grant per linked identity is the contract; the partial
        // unique index enforces it against two concurrent mints, which the
        // read-then-insert check alone cannot.
        manager
            .create_index(
                Index::create()
                    .name("ix_code_external_grant_live_identity")
                    .table(idens::CodeExternalGrant::Table)
                    .col(idens::CodeExternalGrant::Owner)
                    .col(idens::CodeExternalGrant::ChannelKind)
                    .col(idens::CodeExternalGrant::ExternalIdentity)
                    .col(idens::CodeExternalGrant::WorkspaceIdentity)
                    .unique()
                    .and_where(Expr::col(idens::CodeExternalGrant::RevokedAt).is_null())
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        // Every refresh hash a rotation retires, kept for the life of the
        // grant. A replayed rotated token from any generation must revoke
        // the grant, not just the immediately previous one.
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeExternalGrantRetiredRefresh::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeExternalGrantRetiredRefresh::Hash)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrantRetiredRefresh::GrantId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeExternalGrantRetiredRefresh::RetiredAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Dropping the table would sever every linked channel user at once.
        Ok(())
    }
}

/// The connect handshake behind a grant (docs/slack-sessions.md, stage 2).
///
/// One row per connect card the adapter posts. The nonce is stored as a
/// hash and is one-time; the row walks pending -> approved -> completed,
/// and only the completed step — the adapter's closing confirm after its
/// DM proves control of the channel account — mints anything. A forwarded
/// link can reach "approved" at most, which binds nothing.
struct CodeConnectHandshakes;

impl MigrationName for CodeConnectHandshakes {
    fn name(&self) -> &str {
        "m20260828_000028_code_connect_handshakes"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CodeConnectHandshakes {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(idens::CodeConnectHandshake::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::NonceHash)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::Csrf)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::ChannelKind)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::ExternalIdentity)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::WorkspaceIdentity)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::DisplayName)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::WorkspaceName)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeConnectHandshake::AvatarUrl).text())
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::State)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(idens::CodeConnectHandshake::ApprovedOwner).text())
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::ApprovedAt)
                            .timestamp_with_time_zone(),
                    )
                    .col(
                        ColumnDef::new(idens::CodeConnectHandshake::CompletedAt)
                            .timestamp_with_time_zone(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("ix_code_connect_handshake_nonce")
                    .table(idens::CodeConnectHandshake::Table)
                    .col(idens::CodeConnectHandshake::NonceHash)
                    .unique()
                    .if_not_exists()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Handshakes are short-lived; nothing depends on dropping them.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::prelude::{PostgresQueryBuilder, SchemaManager, SqliteQueryBuilder};
    use sea_orm_migration::MigratorTrait;

    #[cfg(feature = "sqlite")]
    use super::rebuild_sqlite_code_workspace_for_archiving_inner;
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
                "m20260825_000015_pre_pin_code_lifecycle_repair",
                "m20260825_000016_code_approval_binding",
                "m20260826_000017_code_session_process_identity",
                "m20260826_000018_code_workspace_archiving",
                "m20260826_000019_code_permission_mode_intent",
                "m20260826_000020_agent_notification",
                "m20260827_000021_code_workflow_runs",
                "m20260827_000022_code_session_incarnations",
                "m20260827_000023_incarnation_ingest",
                "m20260828_000024_code_external_bindings",
                "m20260828_000025_code_external_events",
                "m20260828_000026_code_turn_rewrite",
                "m20260828_000027_code_external_grants",
                "m20260828_000028_code_connect_handshakes",
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

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn a_failed_workspace_rebuild_rolls_back_the_live_sqlite_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, Some(16)).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO code_repo (
                id, owner, root_path, display_name, default_base_ref,
                branch_prefix, quick_actions, created_at
             ) VALUES (
                'repo-archive', 'local', '/tmp/archive', 'archive', 'main',
                'tidebreak/', '[]', '2026-08-26T00:00:00Z'
             );
             INSERT INTO code_workspace (
                id, owner, repo_id, title, worktree_path, branch_name, base_ref,
                status, created_at
             ) VALUES (
                'workspace-archive', 'local', 'repo-archive', 'archive',
                '/tmp/archive-worktree', 'tidebreak/archive', 'main', 'active',
                '2026-08-26T00:00:00Z'
             );
             INSERT INTO code_session (
                id, owner, workspace_id, kind, harness_kind, permission_mode,
                lifecycle, attention_state, attention_source, created_at
             ) VALUES (
                'session-archive', 'local', 'workspace-archive', 'interactive',
                'claude_code', 'plan', 'idle', '{}', 'lifecycle',
                '2026-08-26T00:00:00Z'
             )",
        )
        .await
        .unwrap();

        let manager = SchemaManager::new(&db);
        let error = rebuild_sqlite_code_workspace_for_archiving_inner(&manager, true)
            .await
            .expect_err("the injected statement fails after the live table is dropped");
        assert!(error
            .to_string()
            .contains("missing_workspace_rebuild_table"));

        let workspace = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT status FROM code_workspace WHERE id = 'workspace-archive'".to_owned(),
            ))
            .await
            .unwrap()
            .expect("the original workspace table and row survive");
        assert_eq!(workspace.try_get::<String>("", "status").unwrap(), "active");
        assert!(db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT 1 AS present FROM code_session WHERE id = 'session-archive'".to_owned(),
            ))
            .await
            .unwrap()
            .is_some());
        assert!(db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' \
                 AND name = 'code_workspace_archiving'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .is_none());

        rebuild_sqlite_code_workspace_for_archiving_inner(&manager, false)
            .await
            .unwrap();
        db.execute_unprepared(
            "UPDATE code_workspace SET status = 'archiving' WHERE id = 'workspace-archive'",
        )
        .await
        .unwrap();
        assert!(db
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA foreign_key_check".to_owned(),
            ))
            .await
            .unwrap()
            .is_empty());
        let foreign_keys = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA foreign_keys".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(foreign_keys.try_get::<i64>("", "foreign_keys").unwrap(), 1);
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

    /// The first hosted machine ran v0.58.0, whose recorded baseline lacked
    /// repository lifecycle columns. The repair keeps its row and replaces
    /// the old all-rows uniqueness with the soft-removal contract.
    #[tokio::test]
    async fn a_v058_code_repo_gains_the_pre_pin_lifecycle_schema() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE app (
                id TEXT PRIMARY KEY NOT NULL,
                owner TEXT NOT NULL DEFAULT 'local',
                name TEXT NOT NULL,
                current_revision_id TEXT NOT NULL,
                revision_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );
            CREATE TABLE code_repo (
                id TEXT PRIMARY KEY NOT NULL,
                owner TEXT NOT NULL DEFAULT 'local',
                root_path TEXT NOT NULL,
                display_name TEXT NOT NULL,
                default_base_ref TEXT NOT NULL,
                branch_prefix TEXT NOT NULL,
                setup_script TEXT,
                archive_script TEXT,
                quick_actions TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX idx_code_repo_owner_root_path
                ON code_repo (owner, root_path);
            INSERT INTO code_repo (
                id, owner, root_path, display_name, default_base_ref,
                branch_prefix, quick_actions, created_at
            ) VALUES (
                '00000000-0000-0000-0000-000000000011', 'local', '/srv/pre-pin',
                'pre-pin', 'main', 'tidebreak/', '[]', '2026-08-20T12:00:00Z'
            )",
        )
        .await
        .unwrap();

        Migrator::up(&db, None).await.unwrap();

        db.execute_unprepared(
            "UPDATE code_repo
             SET removed_at = '2026-08-25T12:00:00Z'
             WHERE id = '00000000-0000-0000-0000-000000000011';
             INSERT INTO code_repo (
                 id, owner, root_path, display_name, default_base_ref,
                 branch_prefix, quick_actions, created_at
             ) VALUES (
                 '00000000-0000-0000-0000-000000000012', 'local', '/srv/pre-pin',
                 'pre-pin-again', 'main', 'tidebreak/', '[]',
                 '2026-08-25T12:00:00Z'
             )",
        )
        .await
        .unwrap();

        let rows = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT count(*) AS repo_count,
                        sum(cloned_from IS NULL) AS null_clone_count
                 FROM code_repo
                 WHERE owner = 'local' AND root_path = '/srv/pre-pin'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rows.try_get::<i64>("", "repo_count").unwrap(), 2);
        assert_eq!(rows.try_get::<i64>("", "null_clone_count").unwrap(), 2);
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
