//! `m20260924_000003_turn_versions_and_branches`: rerun a turn in place, and
//! branch a conversation into a new one.
//!
//! Two nullable columns on `turn`:
//!
//! - `replaces_turn_id` names the turn this one reran. A replaced turn leaves
//!   the model's view of the conversation. `NULL` for every ordinary turn.
//! - `replacement` says how: `regenerate` keeps the replaced answer as an
//!   earlier version the reader can page back to; `edit` drops it, because
//!   the message it answered no longer exists.
//!
//! A unique index keeps a turn from being replaced twice. Both backends treat
//! each `NULL` as distinct, so ordinary turns never collide on it.
//!
//! Two nullable columns on `session` record where a branch came from:
//! `branched_from_session_id` and `branched_from_turn_id`, the conversation
//! and the last turn the branch copied. Neither is a foreign key: deleting the
//! original leaves the branch whole, with a link that no longer resolves.
//!
//! One nullable column on `context_checkpoint`, `through_turn_id`, names the
//! latest turn the summarized view held. A summary can repeat anything in that
//! view, so it is dropped when a rerun takes that turn out of the
//! conversation, and a branch copies it only when that turn is copied too.
use sea_orm_migration::prelude::*;

pub(super) struct TurnVersionsAndBranches;

impl MigrationName for TurnVersionsAndBranches {
    fn name(&self) -> &str {
        "m20260924_000003_turn_versions_and_branches"
    }
}

const TURN_REPLACES: &str = "replaces_turn_id";
const TURN_REPLACEMENT: &str = "replacement";
const SESSION_BRANCHED_FROM_SESSION: &str = "branched_from_session_id";
const SESSION_BRANCHED_FROM_TURN: &str = "branched_from_turn_id";
const CHECKPOINT_THROUGH_TURN: &str = "through_turn_id";
const REPLACES_INDEX: &str = "idx_turn_replaces";

#[async_trait::async_trait]
impl MigrationTrait for TurnVersionsAndBranches {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite adds one column per statement, so each is its own alter.
        for (table, column) in [
            ("turn", TURN_REPLACES),
            ("turn", TURN_REPLACEMENT),
            ("session", SESSION_BRANCHED_FROM_SESSION),
            ("session", SESSION_BRANCHED_FROM_TURN),
            ("context_checkpoint", CHECKPOINT_THROUGH_TURN),
        ] {
            if manager.has_column(table, column).await? {
                continue;
            }
            let mut definition = ColumnDef::new(Alias::new(column));
            if column == TURN_REPLACEMENT {
                definition.text();
            } else {
                definition.uuid();
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .add_column(definition)
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(REPLACES_INDEX)
                    .table(Alias::new("turn"))
                    .col(Alias::new(TURN_REPLACES))
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name(REPLACES_INDEX)
                    .table(Alias::new("turn"))
                    .to_owned(),
            )
            .await?;
        for (table, column) in [
            ("turn", TURN_REPLACES),
            ("turn", TURN_REPLACEMENT),
            ("session", SESSION_BRANCHED_FROM_SESSION),
            ("session", SESSION_BRANCHED_FROM_TURN),
            ("context_checkpoint", CHECKPOINT_THROUGH_TURN),
        ] {
            if !manager.has_column(table, column).await? {
                continue;
            }
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new(table))
                        .drop_column(Alias::new(column))
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}
