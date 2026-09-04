//! `m20260904_000002_turn_actor`: who submitted a turn, who settled a decision.
//!
//! Decision 0086. A session with several contributors cannot show who said
//! what while the row records only the text. Each of these columns holds one
//! JSON actor: a principal, a display name, and the channel identity an
//! adapter supplied. Every column is nullable, so a row written before this
//! migration stays null and renders as the session's owner.

use sea_orm_migration::prelude::*;

pub(super) struct TurnActor;

impl MigrationName for TurnActor {
    fn name(&self) -> &str {
        "m20260904_000002_turn_actor"
    }
}

/// Tables that record a submission or a decision, and so name an actor.
const ACTOR_TABLES: [&str; 3] = ["turn", "code_queued_turn", "approval"];

#[async_trait::async_trait]
impl MigrationTrait for TurnActor {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite accepts one ADD COLUMN per ALTER, so each table is its own
        // guarded statement.
        for table in ACTOR_TABLES {
            if manager.has_column(table, "actor").await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .add_column(ColumnDef::new(Alias::new("actor")).json_binary())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // The columns carry historical attribution, and SQLite cannot drop
        // them in place. A nullable column costs a rolled-back database
        // nothing.
        Ok(())
    }
}
