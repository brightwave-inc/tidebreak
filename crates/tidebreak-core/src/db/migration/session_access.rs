//! `m20260904_000001_session_access`: who may read or drive a session.
//!
//! Decision 0086. A session keeps one owner, who stays its execution identity
//! and lifecycle authority. Access becomes a per-session list, and visibility
//! becomes a per-session default.
//!
//! A `session_access` subject carries its namespace in a prefix:
//! `principal:<owner key>` names a machine principal, and
//! `external:<channel kind>:<user id>` names an identity the machine only
//! knows through an adapter, so a Slack adapter can mirror a private
//! channel's membership without those people holding a principal here.
//!
//! Idempotent on the `visibility` column existing, so an interrupted run
//! resumes without failing on a column it already added.

use sea_orm::{ConnectionTrait, DbBackend};
use sea_orm_migration::prelude::*;

pub(super) struct SessionAccess;

impl MigrationName for SessionAccess {
    fn name(&self) -> &str {
        "m20260904_000001_session_access"
    }
}

#[derive(DeriveIden)]
enum Session {
    Table,
    Id,
    Visibility,
}

#[derive(DeriveIden)]
enum SessionAccessTable {
    #[sea_orm(iden = "session_access")]
    Table,
    SessionId,
    Subject,
    Level,
    GrantedBy,
    CreatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for SessionAccess {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("session", "visibility").await? {
            // The vocabulary is a column check on SQLite, which cannot add one
            // to a live table afterwards, and a named table constraint on
            // PostgreSQL, which can.
            let mut column = ColumnDef::new(Session::Visibility);
            column.text().not_null().default("private");
            if manager.get_database_backend() == DbBackend::Sqlite {
                column.check(Expr::col(Session::Visibility).is_in(["private", "deployment"]));
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Session::Table)
                        .add_column(column)
                        .to_owned(),
                )
                .await?;
            if manager.get_database_backend() == DbBackend::Postgres {
                manager
                    .get_connection()
                    .execute_unprepared(
                        r#"
ALTER TABLE "session"
    ADD CONSTRAINT "session_visibility_check"
    CHECK ("visibility" IN ('private', 'deployment'));
"#,
                    )
                    .await?;
            }
        }

        manager
            .create_table(
                Table::create()
                    .table(SessionAccessTable::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SessionAccessTable::SessionId)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionAccessTable::Subject)
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(SessionAccessTable::Level).text().not_null())
                    .col(
                        ColumnDef::new(SessionAccessTable::GrantedBy)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SessionAccessTable::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(SessionAccessTable::SessionId)
                            .col(SessionAccessTable::Subject),
                    )
                    // Deleting a session takes its access list with it: a row
                    // that outlived its session would grant access to an id
                    // a later session could reuse.
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_session_access_session")
                            .from(SessionAccessTable::Table, SessionAccessTable::SessionId)
                            .to(Session::Table, Session::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(SessionAccessTable::Level).is_in(["view", "contribute"]))
                    .check(Func::char_length(Expr::col(SessionAccessTable::Subject)).gt(0))
                    .check(Func::char_length(Expr::col(SessionAccessTable::GrantedBy)).gt(0))
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_session_access_subject")
                    .table(SessionAccessTable::Table)
                    .col(SessionAccessTable::Subject)
                    .col(SessionAccessTable::SessionId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The visibility column stays: SQLite cannot drop it in place, and a
        // rolled-back database reads the private default it already carries.
        manager
            .drop_table(
                Table::drop()
                    .table(SessionAccessTable::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}
