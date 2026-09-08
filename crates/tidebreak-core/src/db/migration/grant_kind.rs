//! `m20260908_000015_code_grant_kind`: person vs workspace adapter grants.
//!
//! Existing rows are person grants. A workspace grant is owned by a service
//! principal and covers one channel workspace; its handshake records the
//! admin who approved it.

use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

pub(super) struct GrantKind;

impl MigrationName for GrantKind {
    fn name(&self) -> &str {
        "m20260908_000015_code_grant_kind"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for GrantKind {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("code_external_grant", "kind").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("code_external_grant"))
                        .add_column(
                            ColumnDef::new(Alias::new("kind"))
                                .text()
                                .not_null()
                                .default("person"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager.has_column("code_connect_handshake", "kind").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("code_connect_handshake"))
                        .add_column(
                            ColumnDef::new(Alias::new("kind"))
                                .text()
                                .not_null()
                                .default("person"),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_column("code_connect_handshake", "approved_by")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("code_connect_handshake"))
                        .add_column(ColumnDef::new(Alias::new("approved_by")).text())
                        .to_owned(),
                )
                .await?;
        }
        if manager.get_database_backend() == DbBackend::Postgres {
            let connection = manager.get_connection();
            let _ = connection
                .execute_unprepared(
                    r#"
ALTER TABLE "code_external_grant"
    ADD CONSTRAINT "code_external_grant_kind_check"
    CHECK ("kind" IN ('person', 'workspace'));
"#,
                )
                .await;
            let _ = connection
                .execute_unprepared(
                    r#"
ALTER TABLE "code_connect_handshake"
    ADD CONSTRAINT "code_connect_handshake_kind_check"
    CHECK ("kind" IN ('person', 'workspace'));
"#,
                )
                .await;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
