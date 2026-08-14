//! The database schema.
//!
//! Fresh pre-v1 databases are described by a single [`Baseline`]. Desktop
//! SQLite changes bump `DESKTOP_SCHEMA_EPOCH` so disposable local data is
//! rebuilt. Self-host databases are durable: a renamed baseline must not
//! recreate tables that already exist, and any later in-place baseline edit
//! that must reach an already-recorded schema also gets an ordered upgrade
//! migration in this module. Squash again before `1.0.0` so that release's
//! first migration is a clean snapshot, not this public-opening chain.

mod baseline;
mod idens;

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Baseline)]
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
        // An existing self-host database already recorded an older baseline
        // name. Re-running CREATE TABLE would fail; fold leftover upgrades
        // into this snapshot instead and leave the rows alone.
        if manager.has_table("app").await? {
            return ensure_app_owner(manager).await;
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

/// Folded from the pre-public `add_app_owner` upgrade. Existing self-host
/// databases that recorded the August 4 baseline may still lack the column.
async fn ensure_app_owner(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
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

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use super::Migrator;

    #[tokio::test]
    async fn a_fresh_database_is_described_by_one_baseline() {
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
        assert_eq!(versions, ["m20260814_000001_baseline"]);
        assert!(db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT owner FROM app LIMIT 1".to_owned(),
            ))
            .await
            .unwrap()
            .is_none());
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
}
