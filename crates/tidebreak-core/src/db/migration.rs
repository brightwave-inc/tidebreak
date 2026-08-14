//! The database schema.
//!
//! Fresh pre-v1 databases are still described by [`Baseline`], and desktop
//! SQLite changes bump `DESKTOP_SCHEMA_EPOCH` so disposable local data is
//! rebuilt. Self-host databases are durable, however, so any schema change
//! that must reach an already-recorded baseline also gets an ordered upgrade
//! migration in this module.

mod baseline;
mod idens;

use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(Baseline), Box::new(AddAppOwner)]
    }
}

/// Upgrade databases that recorded the original pre-launch baseline before
/// apps became principal-owned. Fresh databases already receive this shape
/// from [`Baseline`], so the migration is deliberately idempotent.
struct AddAppOwner;

impl MigrationName for AddAppOwner {
    fn name(&self) -> &str {
        "m20260814_000002_add_app_owner"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for AddAppOwner {
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

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("app", "owner").await? {
            manager
                .drop_index(
                    Index::drop()
                        .if_exists()
                        .name("idx_app_owner_updated")
                        .table(idens::App::Table)
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(idens::App::Table)
                        .drop_column(idens::App::Owner)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

struct Baseline;

impl MigrationName for Baseline {
    fn name(&self) -> &str {
        "m20260804_000001_baseline"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Baseline {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
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

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use super::Migrator;

    #[tokio::test]
    async fn an_existing_baseline_app_is_backfilled_to_the_local_owner() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, Some(1)).await.unwrap();
        db.execute_unprepared("DROP INDEX idx_app_owner_updated")
            .await
            .unwrap();
        db.execute_unprepared("ALTER TABLE app DROP COLUMN owner")
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
