//! `m20260924_000005_deployment_secrets`: the self-host profile's stored
//! secrets, encrypted (decision 102).
//!
//! One row per secret. `name` is the key the secret is stored under,
//! `key_id` names the key that encrypted the row, and `nonce` and
//! `ciphertext` are the AES-256-GCM output, authentication tag included. The
//! server encrypts a value before it writes the row and decrypts after it
//! reads, so no value reaches this table in the clear.
//!
//! The table exists on SQLite too, where nothing writes it: the desktop
//! profile keeps its secrets in the OS keychain.
use sea_orm_migration::prelude::*;

pub(super) struct DeploymentSecrets;

impl MigrationName for DeploymentSecrets {
    fn name(&self) -> &str {
        "m20260924_000005_deployment_secrets"
    }
}

const TABLE: &str = "deployment_secrets";

#[async_trait::async_trait]
impl MigrationTrait for DeploymentSecrets {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let column = |name: &str| Alias::new(name);
        manager
            .create_table(
                Table::create()
                    .table(Alias::new(TABLE))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(column("name"))
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(column("key_id")).text().not_null())
                    .col(ColumnDef::new(column("nonce")).binary().not_null())
                    .col(ColumnDef::new(column("ciphertext")).binary().not_null())
                    .col(
                        ColumnDef::new(column("updated_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new(TABLE))
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
