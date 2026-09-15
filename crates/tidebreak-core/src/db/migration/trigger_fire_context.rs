//! `m20260915_000001_trigger_fire_context`: the structured event a trigger
//! fire delivers.
//!
//! A fire's payload persisted as prose only, so the renderer could not draw a
//! delivered turn as the event it was — condition, pull request, failing
//! checks — without parsing the message back apart. This column holds one
//! JSON [`crate::TriggerTurnContext`] captured when the fire was minted, so a
//! retry after a restart delivers the same facts the edge fired on. Nullable:
//! rows minted before this migration carry none and render from the message.

use sea_orm_migration::prelude::*;

pub(super) struct TriggerFireContext;

impl MigrationName for TriggerFireContext {
    fn name(&self) -> &str {
        "m20260915_000001_trigger_fire_context"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TriggerFireContext {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("code_trigger_fire", "delivery_context")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("code_trigger_fire"))
                    .add_column(ColumnDef::new(Alias::new("delivery_context")).json_binary())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite cannot drop the column in place, and a nullable column costs
        // a rolled-back database nothing.
        Ok(())
    }
}

/// `m20260915_000002_trigger_queue_sink`: admit `queue` as a delivery sink.
///
/// The receipt table's CHECK enumerated the sinks that existed when it was
/// created, and SQLite cannot widen a CHECK in place, so the table is rebuilt
/// with the queue sink admitted (decision 69 as a trigger sink). The rows are
/// copied as they stand; the primary key and column set do not change.
pub(super) struct TriggerQueueSink;

impl MigrationName for TriggerQueueSink {
    fn name(&self) -> &str {
        "m20260915_000002_trigger_queue_sink"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for TriggerQueueSink {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let rebuild = Alias::new("code_trigger_delivery_receipt_rebuild");
        let table = Alias::new("code_trigger_delivery_receipt");
        manager
            .create_table(
                Table::create()
                    .table(rebuild.clone())
                    .col(
                        ColumnDef::new(Alias::new("delivery_id"))
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("owner")).string().not_null())
                    .col(ColumnDef::new(Alias::new("sink")).string_len(16).not_null())
                    .col(ColumnDef::new(Alias::new("session_id")).uuid().not_null())
                    .col(ColumnDef::new(Alias::new("turn_id")).uuid())
                    .col(
                        ColumnDef::new(Alias::new("acceptance_token"))
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("accepted_at"))
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .check(Expr::col(Alias::new("delivery_id")).ne(uuid::Uuid::nil()))
                    .check(Expr::col(Alias::new("acceptance_token")).ne(uuid::Uuid::nil()))
                    .check(Expr::col(Alias::new("sink")).is_in([
                        "turn",
                        "queue",
                        "steer",
                        "attention",
                    ]))
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "INSERT INTO code_trigger_delivery_receipt_rebuild \
                 (delivery_id, owner, sink, session_id, turn_id, acceptance_token, accepted_at) \
                 SELECT delivery_id, owner, sink, session_id, turn_id, acceptance_token, \
                 accepted_at FROM code_trigger_delivery_receipt",
            )
            .await?;
        manager
            .drop_table(Table::drop().table(table.clone()).to_owned())
            .await?;
        manager
            .rename_table(Table::rename().table(rebuild, table).to_owned())
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Widening the sink set is backward-compatible; a rolled-back binary
        // simply never writes `queue`.
        Ok(())
    }
}
