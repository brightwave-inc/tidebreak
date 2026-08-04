//! The database schema.
//!
//! Pre-v1 the schema is a single [`Baseline`] migration rather than a chain:
//! there are no deployed databases to carry forward, so a schema change edits
//! the baseline in place and bumps `DESKTOP_SCHEMA_EPOCH` in
//! `openwave-server`, which discards any database written by an older one.
//! Once the schema stabilizes for v1 this becomes an ordinary migration
//! chain, with the baseline as its first entry.

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
