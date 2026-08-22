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

use sea_orm::ConnectionTrait;
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
        ]
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

    /// A database that stopped at the baseline and later took the rest of the
    /// chain must land on the schema a fresh database gets in one pass.
    ///
    /// This is the property the chain exists for. The desktop profile can be
    /// deleted and rebuilt; the self-host PostgreSQL store cannot, so it only
    /// ever sees the appended migrations. If those disagree with the baseline
    /// by even a default or a nullability, the two deployments run different
    /// schemas against the same queries — and nothing else notices, because
    /// each one is internally consistent.
    ///
    /// With three migrations this is nearly free. It stops being free the
    /// first time an appended migration adds a column the baseline declares
    /// differently, which is the mistake the folded-in `ensure_*` branches
    /// were one edit away from making.
    ///
    /// It runs on SQLite while the store it protects is PostgreSQL, and that
    /// is enough: both sides are the same sea-query statements, so a baseline
    /// and a migration that disagree disagree on either backend. What differs
    /// between backends is the *rendering*, and `the_schema_baseline_is_pinned`
    /// pins that for both.
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
}
